//! Drive health from smartmontools (`smartctl -a -j`).
//!
//! Each physical disk is sampled at most every ten minutes. Every successful
//! reading is kept with its time for ninety days, so the hub can draw history.
//! A connection that hides SMART (for example USB NVMe on macOS) is recorded as
//! unreachable and keeps its last good reading and date. A change between two
//! successful readings (health turns to Warning, wear rises by five points or
//! more, media errors increase) is written to the alert log.
//!
//! The caller passes the state directory and the candidate smartctl paths, so
//! this module reads and writes nothing outside them.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const HISTORY_FILE: &str = "drive-health-history.json";
pub const ALERTS_FILE: &str = "drive-health-alerts.json";
const FORMAT: u32 = 1;
pub const SAMPLE_INTERVAL_SECS: u64 = 600;
pub const RETENTION_SECS: u64 = 90 * 24 * 3600;
pub const WEAR_ALERT_POINTS: u32 = 5;
const MAX_ALERTS: usize = 100;
const TOOL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SelfTest {
    pub kind: String,
    pub result: String,
    pub passed: Option<bool>,
    pub power_on_hours: Option<u64>,
}

/// One successful SMART reading. `at` is Unix seconds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    pub at: u64,
    pub passed: Option<bool>,
    pub temperature_c: Option<f64>,
    pub wear_percent: Option<u32>,
    pub written_bytes: Option<u64>,
    pub power_on_hours: Option<u64>,
    pub critical_warning: Option<u32>,
    pub media_errors: Option<u64>,
    #[serde(default)]
    pub self_tests: Vec<SelfTest>,
}

impl Reading {
    pub fn is_warning(&self) -> bool {
        self.passed == Some(false)
            || self.critical_warning.unwrap_or(0) != 0
            || self.media_errors.unwrap_or(0) > 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    Warning,
    Wear,
    MediaErrors,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Alert {
    pub id: String,
    pub at: u64,
    pub disk: String,
    pub kind: AlertKind,
    pub message: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiskHistory {
    pub model: Option<String>,
    /// Successful readings, oldest first, inside the retention window.
    pub readings: Vec<Reading>,
    /// Whether the latest sample attempt got SMART data from this connection.
    pub reachable: bool,
    pub last_attempt: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct History {
    /// When the last sampling pass ran; gates the ten-minute interval.
    pub last_sample: Option<u64>,
    pub disks: BTreeMap<String, DiskHistory>,
}

#[derive(Serialize)]
struct Saved<'a, T: Serialize> {
    version: u32,
    data: &'a T,
}

#[derive(Deserialize)]
struct Loaded<T> {
    version: u32,
    data: T,
}

fn load<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Loaded<T>>(&bytes).ok())
        .filter(|loaded| loaded.version == FORMAT)
        .map(|loaded| loaded.data)
        .unwrap_or_default()
}

fn save<T: Serialize>(path: &Path, data: &T) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = serde_json::to_vec(&Saved {
        version: FORMAT,
        data,
    })
    .map_err(std::io::Error::other)?;
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    file.write_all(&body)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temp, path)
}

/// Alert entries that a change between two successful readings produces.
pub fn changes(previous: Option<&Reading>, next: &Reading, disk: &str) -> Vec<Alert> {
    let mut alerts = Vec::new();
    let mut push = |kind: AlertKind, message: String| {
        alerts.push(Alert {
            id: format!("{disk}-{}-{kind:?}", next.at),
            at: next.at,
            disk: disk.to_string(),
            kind,
            message,
        });
    };
    if next.is_warning() && !previous.is_some_and(Reading::is_warning) {
        let reason = if next.passed == Some(false) {
            "SMART self-assessment failed".to_string()
        } else if next.critical_warning.unwrap_or(0) != 0 {
            format!("critical warning {}", next.critical_warning.unwrap_or(0))
        } else {
            format!("{} media errors", next.media_errors.unwrap_or(0))
        };
        push(AlertKind::Warning, format!("Health is Warning: {reason}."));
    }
    if let Some(previous) = previous {
        if let (Some(before), Some(after)) = (previous.wear_percent, next.wear_percent)
            && after >= before + WEAR_ALERT_POINTS
        {
            push(
                AlertKind::Wear,
                format!("Wear rose from {before}% to {after}%."),
            );
        }
        if let Some(after) = next.media_errors {
            let before = previous.media_errors.unwrap_or(0);
            if after > before {
                push(
                    AlertKind::MediaErrors,
                    format!("Media errors rose from {before} to {after}."),
                );
            }
        }
    }
    alerts
}

/// What one smartctl run found for a disk.
#[derive(Debug)]
pub enum Outcome {
    Read {
        reading: Reading,
        model: Option<String>,
    },
    /// smartctl ran but the connection gave no SMART status.
    Unavailable { model: Option<String> },
}

/// The first candidate that exists and is executable.
pub fn find_tool(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|path| path.is_file()).cloned()
}

fn run_tool(tool: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let mut child = Command::new(tool)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout.read_to_end(&mut buffer);
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

fn number(value: Option<&serde_json::Value>) -> Option<f64> {
    value.and_then(serde_json::Value::as_f64)
}

fn integer(value: Option<&serde_json::Value>) -> Option<u64> {
    value.and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)))
}

/// Parse `smartctl -a -j` output. None when there is no SMART status.
pub fn parse(json: &serde_json::Value, at: u64) -> Option<(Reading, Option<String>)> {
    let passed = json.pointer("/smart_status/passed")?.as_bool()?;
    let model = json
        .get("model_name")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let mut reading = Reading {
        at,
        passed: Some(passed),
        temperature_c: number(json.pointer("/temperature/current")),
        wear_percent: None,
        written_bytes: None,
        power_on_hours: integer(json.pointer("/power_on_time/hours")),
        critical_warning: None,
        media_errors: None,
        self_tests: Vec::new(),
    };
    if let Some(nvme) = json.get("nvme_smart_health_information_log") {
        reading.wear_percent = integer(nvme.get("percentage_used")).map(|v| v as u32);
        reading.written_bytes =
            integer(nvme.get("data_units_written")).map(|units| units * 512_000);
        reading.critical_warning = integer(nvme.get("critical_warning")).map(|v| v as u32);
        reading.media_errors = integer(nvme.get("media_errors"));
        if reading.temperature_c.is_none() {
            reading.temperature_c = number(nvme.get("temperature"));
        }
        if let Some(table) = json
            .pointer("/nvme_self_test_log/table")
            .and_then(|v| v.as_array())
        {
            reading.self_tests = table
                .iter()
                .take(5)
                .map(|entry| SelfTest {
                    kind: entry
                        .pointer("/self_test_code/string")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Test")
                        .to_string(),
                    result: entry
                        .pointer("/self_test_result/string")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown")
                        .to_string(),
                    passed: entry
                        .pointer("/self_test_result/value")
                        .and_then(|v| v.as_u64())
                        .map(|value| value == 0),
                    power_on_hours: integer(entry.get("power_on_hours")),
                })
                .collect();
        }
    } else if let Some(table) = json
        .pointer("/ata_smart_attributes/table")
        .and_then(|v| v.as_array())
    {
        let mut media = 0u64;
        let mut seen_media = false;
        for attribute in table {
            let name = attribute.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let normalized = integer(attribute.get("value")).map(|v| v as u32);
            let raw = integer(attribute.pointer("/raw/value"));
            match name {
                "Wear_Leveling_Count"
                | "Media_Wearout_Indicator"
                | "SSD_Life_Left"
                | "Percent_Lifetime_Remain" => {
                    if let Some(normalized) = normalized.filter(|v| *v <= 100) {
                        reading.wear_percent = Some(100 - normalized);
                    }
                }
                "Total_LBAs_Written" => reading.written_bytes = raw.map(|v| v * 512),
                "Reallocated_Sector_Ct" | "Reported_Uncorrect" => {
                    if let Some(raw) = raw.filter(|v| *v > 0) {
                        media += raw;
                        seen_media = true;
                    }
                }
                _ => {}
            }
        }
        if seen_media {
            reading.media_errors = Some(media);
        }
        if let Some(table) = json
            .pointer("/ata_smart_self_test_log/standard/table")
            .and_then(|v| v.as_array())
        {
            reading.self_tests = table
                .iter()
                .take(5)
                .map(|entry| SelfTest {
                    kind: entry
                        .pointer("/type/string")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Test")
                        .to_string(),
                    result: entry
                        .pointer("/status/string")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown")
                        .to_string(),
                    passed: entry.pointer("/status/passed").and_then(|v| v.as_bool()),
                    power_on_hours: integer(entry.get("lifetime_hours")),
                })
                .collect();
        }
    }
    Some((reading, model))
}

/// Run smartctl on one whole disk (`disk0`, not `disk0s2`).
pub fn sample_disk(tool: &Path, disk: &str, at: u64) -> Outcome {
    let device = format!("/dev/{disk}");
    let Some(output) = run_tool(tool, &["-a", "-j", device.as_str()]) else {
        return Outcome::Unavailable { model: None };
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&output) else {
        return Outcome::Unavailable { model: None };
    };
    match parse(&json, at) {
        Some((reading, model)) => Outcome::Read { reading, model },
        None => Outcome::Unavailable {
            model: json
                .get("model_name")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        },
    }
}

#[cfg(target_os = "macos")]
fn plist_string(text: &str, key: &str) -> Option<String> {
    let marker = format!("<key>{key}</key>");
    let rest = text[text.find(&marker)? + marker.len()..].trim_start();
    let value = rest.strip_prefix("<string>")?;
    Some(value[..value.find("</string>")?].to_string())
}

/// "disk0s2" -> "disk0".
#[cfg(target_os = "macos")]
fn whole_disk(identifier: &str) -> Option<String> {
    let digits: String = identifier
        .strip_prefix("disk")?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (!digits.is_empty()).then(|| format!("disk{digits}"))
}

/// The physical whole disk under a mount point. An APFS volume reports its
/// physical store (disk0s2), which is what SMART can address.
#[cfg(target_os = "macos")]
pub fn disk_of_mount(mount: &str) -> Option<String> {
    let output = Command::new("/usr/sbin/diskutil")
        .args(["info", "-plist", mount])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let identifier = plist_string(&text, "APFSPhysicalStore")
        .or_else(|| plist_string(&text, "ParentWholeDisk"))
        .or_else(|| plist_string(&text, "DeviceIdentifier"))?;
    whole_disk(&identifier)
}

/// The physical drive under a drive-letter mount (`C:\`), in smartmontools'
/// `pdN` form (`\\.\PhysicalDriveN`). The volume is opened with no access rights
/// (no administrator needed) and asked `IOCTL_STORAGE_GET_DEVICE_NUMBER`; a
/// volume that is not a plain basic-disk partition (network, optical, spanned
/// dynamic volume) answers `None` and is never sampled.
#[cfg(target_os = "windows")]
pub fn disk_of_mount(mount: &str) -> Option<String> {
    use std::ffi::c_void;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::Win32::System::Ioctl::{IOCTL_STORAGE_GET_DEVICE_NUMBER, STORAGE_DEVICE_NUMBER};
    use windows::core::PCWSTR;

    let mut chars = mount.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    if chars.next() != Some(':') {
        return None;
    }
    let name: Vec<u16> = format!(r"\\.\{}:", letter.to_ascii_uppercase())
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: NUL-terminated name; access 0 asks only for the handle, closed below.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .ok()?;
    let mut number = STORAGE_DEVICE_NUMBER::default();
    let mut returned = 0u32;
    // SAFETY: `number` is a writable STORAGE_DEVICE_NUMBER of the size passed.
    let result = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            None,
            0,
            Some((&mut number as *mut STORAGE_DEVICE_NUMBER).cast::<c_void>()),
            std::mem::size_of::<STORAGE_DEVICE_NUMBER>() as u32,
            Some(std::ptr::addr_of_mut!(returned)),
            None,
        )
    };
    // SAFETY: the handle came from CreateFileW above and is closed once.
    unsafe {
        let _ = CloseHandle(handle);
    }
    result.ok()?;
    Some(format!("pd{}", number.DeviceNumber))
}

/// Other platforms show no drives.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn disk_of_mount(_mount: &str) -> Option<String> {
    None
}

/// Mount point and its whole disk, for each mount. Disks shared by several
/// volumes appear once per volume.
fn resolve(mounts: &[String]) -> Vec<(String, Option<String>)> {
    mounts
        .iter()
        .map(|mount| (mount.clone(), disk_of_mount(mount)))
        .collect()
}

/// Sample every disk behind `mounts` when the interval has passed; otherwise
/// do nothing. Returns the alerts this pass produced (empty when not due).
pub fn refresh(
    dir: &Path,
    candidates: &[PathBuf],
    mounts: &[String],
    now: u64,
    force: bool,
) -> Vec<Alert> {
    let history_path = dir.join(HISTORY_FILE);
    let mut history: History = load(&history_path);
    if !force
        && let Some(last) = history.last_sample
        && now.saturating_sub(last) < SAMPLE_INTERVAL_SECS
    {
        return Vec::new();
    }
    let Some(tool) = find_tool(candidates) else {
        return Vec::new();
    };
    history.last_sample = Some(now);
    let mut alerts = Vec::new();
    let mut disks: Vec<String> = resolve(mounts)
        .into_iter()
        .filter_map(|(_, disk)| disk)
        .collect();
    disks.sort();
    disks.dedup();
    for disk in disks {
        let entry = history.disks.entry(disk.clone()).or_default();
        entry.last_attempt = Some(now);
        match sample_disk(&tool, &disk, now) {
            Outcome::Read { reading, model } => {
                let previous = entry.readings.last().cloned();
                alerts.extend(changes(previous.as_ref(), &reading, &disk));
                entry.readings.push(reading);
                entry
                    .readings
                    .retain(|r| now.saturating_sub(r.at) <= RETENTION_SECS);
                entry.reachable = true;
                if model.is_some() {
                    entry.model = model;
                }
            }
            Outcome::Unavailable { model } => {
                entry.reachable = false;
                if model.is_some() {
                    entry.model = model;
                }
            }
        }
    }
    // Persist before reporting alerts so a crash cannot repeat them.
    let _ = save(&history_path, &history);
    if !alerts.is_empty() {
        let log_path = dir.join(ALERTS_FILE);
        let mut log: AlertLog = load(&log_path);
        log.alerts.extend(alerts.iter().cloned());
        let excess = log.alerts.len().saturating_sub(MAX_ALERTS);
        log.alerts.drain(..excess);
        let _ = save(&log_path, &log);
    }
    alerts
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct AlertLog {
    alerts: Vec<Alert>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Warning,
    /// The connection hides SMART; `latest` is the last good reading.
    Unavailable,
    /// Never sampled, or the disk could not be identified.
    Unknown,
}

#[derive(Clone, Debug, Serialize)]
pub struct DriveCard {
    pub mount: String,
    pub disk: Option<String>,
    pub model: Option<String>,
    pub status: Status,
    pub reachable: bool,
    /// The latest successful reading, with its date in `at`.
    pub latest: Option<Reading>,
    /// Successful readings inside the retention window, oldest first.
    pub history: Vec<Reading>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub tool_available: bool,
    pub sampled_at: Option<u64>,
    pub drives: Vec<DriveCard>,
    /// Newest first.
    pub alerts: Vec<Alert>,
}

/// The saved view for the given mounts. Never runs smartctl.
pub fn report(dir: &Path, candidates: &[PathBuf], mounts: &[String], now: u64) -> Report {
    let history: History = load(&dir.join(HISTORY_FILE));
    let log: AlertLog = load(&dir.join(ALERTS_FILE));
    let drives = resolve(mounts)
        .into_iter()
        .map(|(mount, disk)| {
            let entry = disk.as_ref().and_then(|d| history.disks.get(d));
            let readings: Vec<Reading> = entry
                .map(|e| {
                    e.readings
                        .iter()
                        .filter(|r| now.saturating_sub(r.at) <= RETENTION_SECS)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let latest = readings.last().cloned();
            let reachable = entry.is_some_and(|e| e.reachable);
            let status = match (&disk, &latest) {
                (None, _) => Status::Unknown,
                (Some(_), None) => {
                    if entry.is_some_and(|e| e.last_attempt.is_some()) {
                        Status::Unavailable
                    } else {
                        Status::Unknown
                    }
                }
                (Some(_), Some(_)) if !reachable => Status::Unavailable,
                (Some(_), Some(reading)) if reading.is_warning() => Status::Warning,
                (Some(_), Some(_)) => Status::Ok,
            };
            DriveCard {
                mount,
                disk: disk.clone(),
                model: entry.and_then(|e| e.model.clone()),
                status,
                reachable,
                latest,
                history: readings,
            }
        })
        .collect();
    let mut alerts = log.alerts;
    alerts.sort_by_key(|a| std::cmp::Reverse(a.at));
    alerts.truncate(20);
    Report {
        tool_available: find_tool(candidates).is_some(),
        sampled_at: history.last_sample,
        drives,
        alerts,
    }
}
