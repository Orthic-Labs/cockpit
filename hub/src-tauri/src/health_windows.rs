//! Drive health on Windows. The core's `drive_health::refresh` and `report` only know
//! how to find a macOS whole disk (`disk_of_mount` answers `None` elsewhere), so this
//! module does the Windows part and reuses the core's parser, alert rules, file names
//! and report types, so the page and the saved history have one shape on both systems.
//!
//! Mapping: `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` on `\\.\C:` opened with no access
//! rights (no administrator needed) gives the physical drive number; a volume that
//! spans several drives, or has none (network, optical), is "unknown" and never sampled.
//!
//! Reading, in order:
//!  1. `smartctl.exe -a -j /dev/pdN` (smartmontools addresses `\\.\PhysicalDriveN` as
//!     `pdN`), run hidden. smartctl needs an elevated process to open a physical
//!     drive, so for the usual unelevated hub it reports a permission error.
//!  2. Storage Management's reliability counters (`MSFT_StorageReliabilityCounter`:
//!     temperature, wear, power-on hours, uncorrected errors) and the disk's health
//!     status, read through PowerShell's `Get-PhysicalDisk | Get-StorageReliabilityCounter`
//!     (hidden). This works without elevation on most drives, but has no written-bytes
//!     counter, self-test log or critical-warning flags, and some drivers return nothing.
//! When both fail the drive is "unavailable through this connection" and keeps its last
//! good reading, exactly like the Mac.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use pulse_core::drive_health::{
    self as dh, Alert, DiskHistory, DriveCard, History, Outcome, Reading, Report, Status, ALERTS_FILE, HISTORY_FILE,
    RETENTION_SECS, SAMPLE_INTERVAL_SECS,
};
use serde::{Deserialize, Serialize};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const FORMAT: u32 = 1;
const MAX_ALERTS: usize = 100;
const TOOL_TIMEOUT: Duration = Duration::from_secs(30);

const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
const OPEN_EXISTING: u32 = 3;
const FILE_SHARE_READ_WRITE: u32 = 0x0000_0003;
const INVALID_HANDLE_VALUE: isize = -1;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *const c_void,
        creation: u32,
        flags: u32,
        template: *mut c_void,
    ) -> *mut c_void;
    fn DeviceIoControl(
        handle: *mut c_void,
        code: u32,
        input: *const c_void,
        input_size: u32,
        output: *mut c_void,
        output_size: u32,
        returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

/// The physical drive number under a mount point like `C:\`, when exactly one drive holds it.
fn drive_number(mount: &str) -> Option<u32> {
    let letter = mount.chars().next().filter(char::is_ascii_alphabetic)?;
    if mount.chars().nth(1) != Some(':') {
        return None;
    }
    let name: Vec<u16> = format!(r"\\.\{}:", letter.to_ascii_uppercase()).encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: NUL-terminated name; access 0 asks only for the handle, and it is closed below.
    let handle = unsafe {
        CreateFileW(name.as_ptr(), 0, FILE_SHARE_READ_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut())
    };
    if handle.is_null() || handle as isize == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut buffer = [0u8; 1024];
    let mut returned = 0u32;
    // SAFETY: `buffer` is a writable 1024-byte block and `returned` a live u32; the handle is open.
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
            std::ptr::null(),
            0,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    // SAFETY: the handle came from CreateFileW above and is closed once.
    unsafe { CloseHandle(handle) };
    if ok == 0 || (returned as usize) < 8 {
        return None;
    }
    // VOLUME_DISK_EXTENTS: a u32 count, 4 bytes of padding, then 24-byte DISK_EXTENTs
    // that start with the u32 disk number.
    let count = u32::from_le_bytes(buffer[0..4].try_into().ok()?) as usize;
    if count == 0 || 8 + count * 24 > returned as usize {
        return None;
    }
    let number = |index: usize| -> Option<u32> {
        let at = 8 + index * 24;
        Some(u32::from_le_bytes(buffer[at..at + 4].try_into().ok()?))
    };
    let first = number(0)?;
    (1..count).all(|index| number(index) == Some(first)).then_some(first)
}

fn disk_key(number: u32) -> String {
    format!("PhysicalDrive{number}")
}

/// The drive key for a mount point.
fn disk_of_mount(mount: &str) -> Option<(String, u32)> {
    drive_number(mount).map(|number| (disk_key(number), number))
}

// ---------------------------------------------------------------------------
// Reading a drive
// ---------------------------------------------------------------------------

/// Runs a hidden console program and returns what it printed, or `None` if it could not
/// start or did not finish in time.
fn run_hidden(program: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
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
            // smartctl's exit status is a bit mask of findings; the output decides.
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    reader.join().ok()
}

fn smartctl(tool: &Path, number: u32, at: u64) -> Outcome {
    let device = format!("/dev/pd{number}");
    let Some(output) = run_hidden(tool, &["-a", "-j", device.as_str()]) else {
        return Outcome::Unavailable { model: None };
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&output) else {
        return Outcome::Unavailable { model: None };
    };
    match dh::parse(&json, at) {
        Some((reading, model)) => Outcome::Read { reading, model },
        None => Outcome::Unavailable { model: json.get("model_name").and_then(|v| v.as_str()).map(str::to_string) },
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Counters {
    model: Option<String>,
    health: Option<String>,
    temp: Option<f64>,
    wear: Option<f64>,
    hours: Option<f64>,
    read_err: Option<f64>,
    write_err: Option<f64>,
}

fn whole(value: Option<f64>) -> Option<u64> {
    value.filter(|v| v.is_finite() && *v >= 0.0).map(|v| v as u64)
}

fn storage_counters(number: u32, at: u64) -> Option<Outcome> {
    let script = format!(
        "$d = Get-PhysicalDisk | Where-Object {{ $_.DeviceId -eq '{number}' }}; \
         if (-not $d) {{ exit 3 }}; \
         $r = $d | Get-StorageReliabilityCounter; \
         [pscustomobject]@{{ Model = [string]$d.FriendlyName; Health = [string]$d.HealthStatus; \
         Temp = $r.Temperature; Wear = $r.Wear; Hours = $r.PowerOnHours; \
         ReadErr = $r.ReadErrorsUncorrected; WriteErr = $r.WriteErrorsUncorrected }} | ConvertTo-Json -Compress"
    );
    let system_root = std::env::var_os("SystemRoot").map(PathBuf::from)?;
    let shell = system_root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let output = run_hidden(
        &shell,
        &["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script.as_str()],
    )?;
    let counters: Counters = serde_json::from_slice(&output).ok()?;
    let passed = match counters.health.as_deref() {
        Some("Healthy") => Some(true),
        Some("Unhealthy") => Some(false),
        _ => None,
    };
    let temperature_c = counters.temp.filter(|t| t.is_finite() && *t > 0.0 && *t < 150.0);
    let wear_percent = whole(counters.wear).filter(|w| *w <= 100).map(|w| w as u32);
    let power_on_hours = whole(counters.hours);
    let media_errors = match (whole(counters.read_err), whole(counters.write_err)) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
    };
    // Nothing at all means the driver gave no counters; that is "unavailable", not a healthy drive.
    if passed.is_none() && temperature_c.is_none() && wear_percent.is_none() && power_on_hours.is_none() && media_errors.is_none() {
        return Some(Outcome::Unavailable { model: counters.model });
    }
    Some(Outcome::Read {
        reading: Reading {
            at,
            passed,
            temperature_c,
            wear_percent,
            written_bytes: None,
            power_on_hours,
            critical_warning: None,
            media_errors,
            self_tests: Vec::new(),
        },
        model: counters.model,
    })
}

fn sample_drive(tool: Option<&Path>, number: u32, at: u64) -> Outcome {
    let mut model = None;
    if let Some(tool) = tool {
        match smartctl(tool, number, at) {
            read @ Outcome::Read { .. } => return read,
            Outcome::Unavailable { model: found } => model = found,
        }
    }
    match storage_counters(number, at) {
        Some(Outcome::Read { reading, model: found }) => Outcome::Read { reading, model: found.or(model) },
        Some(Outcome::Unavailable { model: found }) => Outcome::Unavailable { model: found.or(model) },
        None => Outcome::Unavailable { model },
    }
}

// ---------------------------------------------------------------------------
// History files (the core's names and layout)
// ---------------------------------------------------------------------------

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
    let body = serde_json::to_vec(&Saved { version: FORMAT, data }).map_err(std::io::Error::other)?;
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&temp)?;
    file.write_all(&body)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temp, path)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct AlertLog {
    alerts: Vec<Alert>,
}

// ---------------------------------------------------------------------------
// Refresh and report (the core's logic, with Windows drive numbers)
// ---------------------------------------------------------------------------

/// Samples every drive behind `mounts` when the interval has passed; returns this pass's alerts.
pub fn refresh(dir: &Path, candidates: &[PathBuf], mounts: &[String], now: u64) -> Vec<Alert> {
    let history_path = dir.join(HISTORY_FILE);
    let mut history: History = load(&history_path);
    if let Some(last) = history.last_sample {
        if now.saturating_sub(last) < SAMPLE_INTERVAL_SECS {
            return Vec::new();
        }
    }
    let tool = dh::find_tool(candidates);
    history.last_sample = Some(now);
    let mut drives: BTreeMap<String, u32> = BTreeMap::new();
    for mount in mounts {
        if let Some((key, number)) = disk_of_mount(mount) {
            drives.insert(key, number);
        }
    }
    let mut alerts = Vec::new();
    for (key, number) in drives {
        let entry: &mut DiskHistory = history.disks.entry(key.clone()).or_default();
        entry.last_attempt = Some(now);
        match sample_drive(tool.as_deref(), number, now) {
            Outcome::Read { reading, model } => {
                let previous = entry.readings.last().cloned();
                alerts.extend(dh::changes(previous.as_ref(), &reading, &key));
                entry.readings.push(reading);
                entry.readings.retain(|r| now.saturating_sub(r.at) <= RETENTION_SECS);
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

/// The saved view for the given mounts. Never runs smartctl or PowerShell.
pub fn report(dir: &Path, mounts: &[String], now: u64) -> Report {
    let history: History = load(&dir.join(HISTORY_FILE));
    let log: AlertLog = load(&dir.join(ALERTS_FILE));
    let drives = mounts
        .iter()
        .map(|mount| {
            let disk = disk_of_mount(mount).map(|(key, _)| key);
            let entry = disk.as_ref().and_then(|d| history.disks.get(d));
            let readings: Vec<Reading> = entry
                .map(|e| e.readings.iter().filter(|r| now.saturating_sub(r.at) <= RETENTION_SECS).cloned().collect())
                .unwrap_or_default();
            let latest = readings.last().cloned();
            let reachable = entry.is_some_and(|e| e.reachable);
            let status = match (&disk, &latest) {
                (None, _) => Status::Unknown,
                (Some(_), None) => {
                    if entry.is_some_and(|e| e.last_attempt.is_some()) { Status::Unavailable } else { Status::Unknown }
                }
                (Some(_), Some(_)) if !reachable => Status::Unavailable,
                (Some(_), Some(reading)) if reading.is_warning() => Status::Warning,
                (Some(_), Some(_)) => Status::Ok,
            };
            DriveCard {
                mount: mount.clone(),
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
    alerts.sort_by(|a, b| b.at.cmp(&a.at));
    alerts.truncate(20);
    // The storage counters need no extra tool, so the page is never told to install one.
    Report { tool_available: true, sampled_at: history.last_sample, drives, alerts }
}
