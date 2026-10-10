//! Drive health for the Disks card, from smartmontools (`smartctl -a -j`), parsed the way
//! `core/src/drive_health.rs` parses it (the notch does not depend on pulse-core).
//!
//! `smartctl.exe` is looked up next to the notch, under `Helpers\`, then in the standard
//! smartmontools install folder. Windows Storage Reliability counters (WMI
//! `MSFT_StorageReliabilityCounter`) are deliberately not a fallback: they need a COM/VARIANT
//! client that cannot be checked without a build here, and they carry no SMART pass/fail
//! verdict, so a missing tool is said so ("Install smartmontools") rather than half-answered.
//!
//! A worker thread samples every ten minutes (smartctl can take seconds and opening a
//! physical drive may need administrator rights, so never on the UI thread). Each drive's
//! last good reading is kept with its date in `%LOCALAPPDATA%\Pulse\drive-health.json`; a drive
//! whose connection hides SMART shows "n/a" plus that last reading. Unknown stays explicit.

use crate::diag;
use crate::json::{self, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SAMPLE_SECONDS: u64 = 600;
const TOOL_TIMEOUT: Duration = Duration::from_secs(30);
const OUTPUT_MAX: usize = 4 * 1024 * 1024;
const FILE_MAX: usize = 256 * 1024;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MAX_DRIVES: usize = 16;

/// One successful SMART reading. `at` is Unix seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub at: u64,
    pub passed: Option<bool>,
    pub temperature_c: Option<f64>,
    pub wear_percent: Option<u32>,
    pub written_bytes: Option<u64>,
    pub media_errors: Option<u64>,
    pub critical_warning: Option<u32>,
}

impl Reading {
    pub fn is_warning(&self) -> bool {
        self.passed == Some(false)
            || self.critical_warning.unwrap_or(0) != 0
            || self.media_errors.unwrap_or(0) > 0
    }
}

/// One physical disk: its name, the last good reading, and whether the latest attempt got
/// SMART data through this connection.
#[derive(Clone, Debug, PartialEq)]
pub struct Drive {
    pub name: String,
    pub last: Option<Reading>,
    pub reachable: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Report {
    /// No sampling pass has finished yet: the card shows no health rows.
    Pending,
    /// No smartctl.exe was found.
    Missing,
    Drives(Vec<Drive>),
}

static REPORT: Mutex<Report> = Mutex::new(Report::Pending);

pub fn current() -> Report {
    REPORT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

fn publish(report: Report) {
    *REPORT.lock().unwrap_or_else(PoisonError::into_inner) = report;
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Starts the sampling thread (detached; it dies with the process).
pub fn start() {
    let spawned = std::thread::Builder::new()
        .name("drive-health".into())
        .spawn(worker);
    if let Err(error) = spawned {
        diag::info(
            "drive_health_thread_failed",
            &[("reason", error.to_string().as_str())],
        );
    }
}

fn worker() {
    let mut known = load();
    loop {
        match find_tool() {
            None => publish(Report::Missing),
            Some(tool) => {
                sample(&tool, &mut known);
                publish(Report::Drives(known.values().cloned().collect()));
                save(&known);
            }
        }
        std::thread::sleep(Duration::from_secs(SAMPLE_SECONDS));
    }
}

fn sample(tool: &Path, known: &mut BTreeMap<String, Drive>) {
    let at = now_secs();
    let Some(scan) = run_tool(tool, &["--scan", "-j"]).and_then(|b| parse_json(&b)) else {
        // smartctl ran but listed nothing usable: every known drive is unreachable now.
        for drive in known.values_mut() {
            drive.reachable = false;
        }
        return;
    };
    let devices: Vec<String> = scan
        .get("devices")
        .and_then(Value::as_array)
        .unwrap_or(&[])
        .iter()
        .filter_map(|d| d.get("name").and_then(Value::as_str).map(str::to_string))
        .take(MAX_DRIVES)
        .collect();
    for drive in known.values_mut() {
        drive.reachable = false;
    }
    for device in devices {
        let json = run_tool(tool, &["-a", "-j", device.as_str()]).and_then(|b| parse_json(&b));
        let model = json
            .as_ref()
            .and_then(|j| j.get("model_name"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let name = model.unwrap_or_else(|| device.clone());
        let entry = known.entry(name.clone()).or_insert(Drive {
            name,
            last: None,
            reachable: false,
        });
        if let Some(reading) = json.as_ref().and_then(|j| parse(j, at)) {
            entry.last = Some(reading);
            entry.reachable = true;
        }
    }
}

// ---- finding and running smartctl ------------------------------------------------------------

fn find_tool() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        candidates.push(dir.join("smartctl.exe"));
        candidates.push(dir.join("Helpers").join("smartctl.exe"));
    }
    if let Some(files) = std::env::var_os("ProgramFiles").map(PathBuf::from)
        && files.is_absolute()
    {
        candidates.push(files.join("smartmontools").join("bin").join("smartctl.exe"));
    }
    candidates.into_iter().find(|path| path.is_file())
}

fn run_tool(tool: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let mut child = Command::new(tool)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout.take(OUTPUT_MAX as u64).read_to_end(&mut buffer);
        buffer
    });
    let deadline = Instant::now() + TOOL_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    // smartctl's exit status is a bit mask of findings; the output decides.
    reader.join().ok()
}

/// The JSON reader maps `true` and `false` to `Null`, so booleans become 1 and 0 (outside
/// strings) before parsing.
pub fn numeric_booleans(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let (mut in_string, mut escaped) = (false, false);
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if in_string {
            out.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            at += 1;
        } else if byte == b'"' {
            in_string = true;
            out.push(byte);
            at += 1;
        } else if bytes[at..].starts_with(b"true") {
            out.push(b'1');
            at += 4;
        } else if bytes[at..].starts_with(b"false") {
            out.push(b'0');
            at += 5;
        } else {
            out.push(byte);
            at += 1;
        }
    }
    out
}

fn parse_json(bytes: &[u8]) -> Option<Value> {
    json::parse(&numeric_booleans(bytes), OUTPUT_MAX)
}

// ---- smartctl JSON -> Reading (same fields as core/src/drive_health.rs) ----------------------

fn integer(value: Option<&Value>) -> Option<u64> {
    value
        .and_then(Value::as_f64)
        .map(|f| f.clamp(0.0, u64::MAX as f64) as u64)
}

/// None when there is no SMART status (the connection hides it).
fn parse(json: &Value, at: u64) -> Option<Reading> {
    let passed = json.path(&["smart_status", "passed"])?.as_f64()? != 0.0;
    let mut reading = Reading {
        at,
        passed: Some(passed),
        temperature_c: json
            .path(&["temperature", "current"])
            .and_then(Value::as_f64),
        wear_percent: None,
        written_bytes: None,
        media_errors: None,
        critical_warning: None,
    };
    if let Some(nvme) = json.get("nvme_smart_health_information_log") {
        reading.wear_percent = integer(nvme.get("percentage_used")).map(|v| v as u32);
        reading.written_bytes =
            integer(nvme.get("data_units_written")).map(|units| units.saturating_mul(512_000));
        reading.critical_warning = integer(nvme.get("critical_warning")).map(|v| v as u32);
        reading.media_errors = integer(nvme.get("media_errors"));
        if reading.temperature_c.is_none() {
            reading.temperature_c = nvme.get("temperature").and_then(Value::as_f64);
        }
    } else if let Some(table) = json
        .path(&["ata_smart_attributes", "table"])
        .and_then(Value::as_array)
    {
        let mut media = 0u64;
        let mut seen_media = false;
        for attribute in table {
            let name = attribute.get("name").and_then(Value::as_str).unwrap_or("");
            let normalized = integer(attribute.get("value")).map(|v| v as u32);
            let raw = integer(attribute.path(&["raw", "value"]));
            match name {
                "Wear_Leveling_Count"
                | "Media_Wearout_Indicator"
                | "SSD_Life_Left"
                | "Percent_Lifetime_Remain" => {
                    if let Some(normalized) = normalized.filter(|v| *v <= 100) {
                        reading.wear_percent = Some(100 - normalized);
                    }
                }
                "Total_LBAs_Written" => {
                    reading.written_bytes = raw.map(|v| v.saturating_mul(512));
                }
                "Reallocated_Sector_Ct" | "Reported_Uncorrect" => {
                    if let Some(raw) = raw.filter(|v| *v > 0) {
                        media = media.saturating_add(raw);
                        seen_media = true;
                    }
                }
                _ => {}
            }
        }
        if seen_media {
            reading.media_errors = Some(media);
        }
    }
    Some(reading)
}

// ---- last good reading on disk ---------------------------------------------------------------

fn store_path() -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    base.is_absolute()
        .then(|| base.join("Pulse").join("drive-health.json"))
}

fn load() -> BTreeMap<String, Drive> {
    let mut drives = BTreeMap::new();
    let Some(path) = store_path() else {
        return drives;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return drives;
    };
    let Some(root) = json::parse(&bytes, FILE_MAX) else {
        return drives;
    };
    for item in root
        .get("disks")
        .and_then(Value::as_array)
        .unwrap_or(&[])
        .iter()
        .take(MAX_DRIVES)
    {
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            continue;
        };
        let last = integer(item.get("at")).map(|at| Reading {
            at,
            passed: item.get("passed").and_then(Value::as_f64).map(|p| p != 0.0),
            temperature_c: item.get("temperatureC").and_then(Value::as_f64),
            wear_percent: integer(item.get("wearPercent")).map(|v| v as u32),
            written_bytes: integer(item.get("writtenBytes")),
            media_errors: integer(item.get("mediaErrors")),
            critical_warning: integer(item.get("criticalWarning")).map(|v| v as u32),
        });
        drives.insert(
            name.to_string(),
            Drive {
                name: name.to_string(),
                last,
                reachable: false,
            },
        );
    }
    drives
}

fn quoted(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn save(known: &BTreeMap<String, Drive>) {
    let Some(path) = store_path() else {
        return;
    };
    let mut items = Vec::new();
    for drive in known.values() {
        let Some(last) = &drive.last else {
            continue;
        };
        let mut fields = vec![
            format!("\"name\":{}", quoted(&drive.name)),
            format!("\"at\":{}", last.at),
        ];
        if let Some(passed) = last.passed {
            fields.push(format!("\"passed\":{}", u8::from(passed)));
        }
        if let Some(t) = last.temperature_c.filter(|t| t.is_finite()) {
            fields.push(format!("\"temperatureC\":{t}"));
        }
        if let Some(v) = last.wear_percent {
            fields.push(format!("\"wearPercent\":{v}"));
        }
        if let Some(v) = last.written_bytes {
            fields.push(format!("\"writtenBytes\":{v}"));
        }
        if let Some(v) = last.media_errors {
            fields.push(format!("\"mediaErrors\":{v}"));
        }
        if let Some(v) = last.critical_warning {
            fields.push(format!("\"criticalWarning\":{v}"));
        }
        items.push(format!("{{{}}}", fields.join(",")));
    }
    let body = format!("{{\"version\":1,\"disks\":[{}]}}", items.join(","));
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    if std::fs::write(&temp, body).is_ok() && std::fs::rename(&temp, &path).is_err() {
        let _ = std::fs::remove_file(&temp);
    }
}

// ---- card text -------------------------------------------------------------------------------

/// "Oct 2, 2026" for a Unix time (UTC calendar date, civil-from-days).
pub fn date_text(at: u64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let z = (at / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{} {day}, {year}", MONTHS[(month - 1) as usize])
}

/// Decimal units, like the Mac's byte formatter: "21.4 TB".
pub fn written_text(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e12 {
        format!("{:.1} TB", b / 1e12)
    } else if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{:.0} KB", b / 1e3)
    }
}

fn verdict(reading: &Reading) -> &'static str {
    if reading.is_warning() {
        "Warning"
    } else {
        "OK"
    }
}

/// Keeps a model name short enough for the one-line row.
fn short_name(name: &str) -> String {
    const MAX: usize = 16;
    if name.chars().count() <= MAX {
        name.to_string()
    } else {
        let head: String = name.chars().take(MAX - 1).collect();
        format!("{}\u{2026}", head.trim_end())
    }
}

/// `Name: OK · 3% worn · 21.4 TB written` (temperature stays out: Windows shows none beside
/// the volume).
pub fn ok_line(name: &str, reading: &Reading) -> String {
    let mut parts = vec![verdict(reading).to_string()];
    if let Some(percent) = reading.wear_percent {
        parts.push(format!("{percent}% worn"));
    }
    if let Some(bytes) = reading.written_bytes {
        parts.push(format!("{} written", written_text(bytes)));
    }
    format!("{}: {}", short_name(name), parts.join(" \u{b7} "))
}

pub fn unavailable_line(name: &str) -> String {
    format!("{}: Health n/a on this connection", short_name(name))
}

/// `Last reading Oct 2, 2026: OK · 36 °C · 9% worn`.
pub fn last_line(reading: &Reading) -> String {
    let mut parts = vec![verdict(reading).to_string()];
    if let Some(t) = reading.temperature_c {
        parts.push(format!("{} \u{b0}C", t.round() as i64));
    }
    if let Some(percent) = reading.wear_percent {
        parts.push(format!("{percent}% worn"));
    }
    format!(
        "Last reading {}: {}",
        date_text(reading.at),
        parts.join(" \u{b7} ")
    )
}

/// The plain lines under the volumes: one per drive, plus the dated last reading for a drive
/// whose connection hides SMART.
pub fn lines(drives: &[Drive]) -> Vec<String> {
    let mut out = Vec::new();
    for drive in drives {
        if drive.reachable {
            if let Some(last) = &drive.last {
                out.push(ok_line(&drive.name, last));
            }
        } else {
            out.push(unavailable_line(&drive.name));
            if let Some(last) = &drive.last {
                out.push(last_line(last));
            }
        }
    }
    out
}

/// Names of the drives whose latest good reading is a SMART warning (the notch's drive alert
/// card reads this; a drive with no reading is never listed).
pub fn warnings() -> Vec<String> {
    match current() {
        Report::Drives(drives) => drives
            .into_iter()
            .filter(|d| d.reachable && d.last.as_ref().is_some_and(Reading::is_warning))
            .map(|d| d.name)
            .collect(),
        Report::Pending | Report::Missing => Vec::new(),
    }
}
