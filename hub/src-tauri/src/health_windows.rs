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
//!  2. For NVMe, the SMART / Health Information log page through
//!     `IOCTL_STORAGE_QUERY_PROPERTY` on a handle opened with no access rights. That
//!     needs no administrator and gives temperature, wear, data written, power-on hours,
//!     media errors and the critical-warning flags (no self-test log).
//!  3. Storage Management's reliability counters (`MSFT_StorageReliabilityCounter`:
//!     temperature, wear, power-on hours, uncorrected errors) and the disk's health
//!     status, read through PowerShell's `Get-PhysicalDisk | Get-StorageReliabilityCounter`
//!     (hidden). That is a CIM call and is refused without elevation on many machines; it
//!     has no written-bytes counter, self-test log or critical-warning flags.
//!  4. Just the temperature (`StorageDeviceTemperatureProperty`, also unelevated).
//! When all fail the drive is "unavailable through this connection" and keeps its last
//! good reading, exactly like the Mac. SATA SMART attributes need an elevated smartctl.

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
const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
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

/// Opens `\\.\PhysicalDriveN` with no access rights (`dwDesiredAccess = 0`). Windows allows
/// that unelevated, and it is enough for `IOCTL_STORAGE_QUERY_PROPERTY`. The caller closes it.
fn open_physical(number: u32) -> Option<*mut c_void> {
    let name: Vec<u16> = format!(r"\\.\PhysicalDrive{number}").encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: NUL-terminated name; access 0 asks only for the handle.
    let handle = unsafe {
        CreateFileW(name.as_ptr(), 0, FILE_SHARE_READ_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut())
    };
    (!handle.is_null() && handle as isize != INVALID_HANDLE_VALUE).then_some(handle)
}

/// One `IOCTL_STORAGE_QUERY_PROPERTY`; returns how many bytes of `output` were filled.
fn storage_query(handle: *mut c_void, input: &[u8], output: &mut [u8]) -> Option<usize> {
    let mut returned = 0u32;
    // SAFETY: `input` and `output` are live slices of the sizes passed and `returned` a live
    // u32; the handle is open. The query is read-only and has no overlapped I/O.
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            input.as_ptr().cast(),
            input.len() as u32,
            output.as_mut_ptr().cast(),
            output.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some((returned as usize).min(output.len()))
}

fn le_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// The low 64 bits of a little-endian 128-bit NVMe counter (the high half is ignored; a
/// counter that large is not real).
fn le_u64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// The NVMe SMART / Health Information log page (0x02) through the storage stack, with no
/// elevation. The query is a `STORAGE_PROPERTY_QUERY` (PropertyId u32, QueryType u32) followed
/// by a 40-byte `STORAGE_PROTOCOL_SPECIFIC_DATA`; the answer is a `STORAGE_PROTOCOL_DATA_DESCRIPTOR`
/// (Version u32, Size u32) holding that same structure, then the 512-byte log at
/// `ProtocolDataOffset` from the start of the structure. Built as plain bytes, so no packing
/// or alignment can go wrong. `None` for a drive that is not NVMe or refuses the query.
fn native_nvme(number: u32, at: u64) -> Option<Reading> {
    const PROTOCOL_DATA: usize = 8; // where the specific-data structure starts in both buffers
    const SPECIFIC_DATA_SIZE: u32 = 40;
    const LOG_SIZE: u32 = 512;
    let mut query = [0u8; PROTOCOL_DATA + SPECIFIC_DATA_SIZE as usize];
    let fields: [u32; 8] = [
        50,                 // PropertyId: StorageDeviceProtocolSpecificProperty
        0,                  // QueryType: PropertyStandardQuery
        3,                  // ProtocolType: ProtocolTypeNvme
        2,                  // DataType: NVMeDataTypeLogPage
        2,                  // ProtocolDataRequestValue: log page 0x02 (SMART / Health)
        0,                  // ProtocolDataRequestSubValue
        SPECIFIC_DATA_SIZE, // ProtocolDataOffset: the log follows the structure
        LOG_SIZE,           // ProtocolDataLength
    ];
    for (index, field) in fields.iter().enumerate() {
        query[index * 4..index * 4 + 4].copy_from_slice(&field.to_le_bytes());
    }
    let mut output = [0u8; PROTOCOL_DATA + SPECIFIC_DATA_SIZE as usize + LOG_SIZE as usize];
    let handle = open_physical(number)?;
    let filled = storage_query(handle, &query, &mut output);
    // SAFETY: the handle came from CreateFileW above and is closed once.
    unsafe { CloseHandle(handle) };
    let filled = filled?;
    let log = PROTOCOL_DATA + le_u32(&output, PROTOCOL_DATA + 16)? as usize;
    // Through "media and data integrity errors" (bytes 160..176) must be present.
    if filled < log + 176 || le_u32(&output, PROTOCOL_DATA)? != 3 || le_u32(&output, PROTOCOL_DATA + 4)? != 2 {
        return None;
    }
    let log = &output[log..filled];
    let critical = u32::from(log[0]);
    let kelvin = le_u16(log, 1)?;
    // NVMe counts data units of 1000 512-byte blocks.
    let written_bytes = le_u64(log, 48)?.checked_mul(512_000);
    Some(Reading {
        at,
        passed: Some(critical == 0),
        // 0 means "not reported"; the field is a composite temperature in Kelvin.
        temperature_c: (kelvin > 273).then(|| f64::from(kelvin) - 273.0).filter(|c| *c < 150.0),
        wear_percent: Some(u32::from(log[5])),
        written_bytes,
        power_on_hours: le_u64(log, 128),
        critical_warning: Some(critical),
        media_errors: le_u64(log, 160),
        self_tests: Vec::new(),
    })
}

/// The temperature, in Celsius, from `StorageDeviceTemperatureProperty` (PropertyId 52): works
/// unelevated for any drive whose driver reports it, NVMe or not. A `STORAGE_TEMPERATURE_DATA_
/// DESCRIPTOR` is 24 bytes (Version, Size, Critical i16, Warning i16, InfoCount u16, padding)
/// followed by 16-byte infos (Index u16, Temperature i16, ...); the first info is the one wanted.
fn native_temperature(number: u32) -> Option<f64> {
    let mut query = [0u8; 12]; // STORAGE_PROPERTY_QUERY with its one byte of parameters, padded
    query[0..4].copy_from_slice(&52u32.to_le_bytes());
    let mut output = [0u8; 256];
    let handle = open_physical(number)?;
    let filled = storage_query(handle, &query, &mut output);
    // SAFETY: the handle came from CreateFileW above and is closed once.
    unsafe { CloseHandle(handle) };
    let filled = filled?;
    if filled < 40 || le_u16(&output, 12)? == 0 {
        return None;
    }
    let celsius = f64::from(i16::from_le_bytes(output[26..28].try_into().ok()?));
    (celsius > 0.0 && celsius < 150.0).then_some(celsius)
}

/// The drive's product name from `StorageDeviceProperty` (a `STORAGE_DEVICE_DESCRIPTOR` whose
/// vendor and product strings are NUL-terminated ASCII at the stated offsets).
fn native_model(number: u32) -> Option<String> {
    let query = [0u8; 12]; // PropertyId 0 (StorageDeviceProperty), standard query
    let mut output = [0u8; 1024];
    let handle = open_physical(number)?;
    let filled = storage_query(handle, &query, &mut output);
    // SAFETY: the handle came from CreateFileW above and is closed once.
    unsafe { CloseHandle(handle) };
    let filled = filled?;
    let text = |offset: usize| -> String {
        if offset == 0 || offset >= filled {
            return String::new();
        }
        let end = output[offset..filled].iter().position(|b| *b == 0).map_or(filled, |n| offset + n);
        String::from_utf8_lossy(&output[offset..end]).trim().to_string()
    };
    let vendor = text(le_u32(&output, 12)? as usize);
    let product = text(le_u32(&output, 16)? as usize);
    let name = match vendor.as_str() {
        "" | "NVMe" | "ATA" => product,
        _ => format!("{vendor} {product}").trim().to_string(),
    };
    (!name.is_empty()).then_some(name)
}

fn sample_drive(tool: Option<&Path>, number: u32, at: u64) -> Outcome {
    let mut model = None;
    if let Some(tool) = tool {
        match smartctl(tool, number, at) {
            read @ Outcome::Read { .. } => return read,
            Outcome::Unavailable { model: found } => model = found,
        }
    }
    // NVMe's own log page: temperature, wear, writes, hours and errors with no elevation.
    if let Some(reading) = native_nvme(number, at) {
        return Outcome::Read { reading, model: model.or_else(|| native_model(number)) };
    }
    match storage_counters(number, at) {
        Some(Outcome::Read { mut reading, model: found }) => {
            if reading.temperature_c.is_none() {
                reading.temperature_c = native_temperature(number);
            }
            Outcome::Read { reading, model: found.or(model) }
        }
        Some(Outcome::Unavailable { model: found }) => unavailable_or_temperature(number, at, found.or(model)),
        None => unavailable_or_temperature(number, at, model),
    }
}

/// The last resort: a reading holding only the temperature, if the driver gives that much.
fn unavailable_or_temperature(number: u32, at: u64, model: Option<String>) -> Outcome {
    let model = model.or_else(|| native_model(number));
    match native_temperature(number) {
        Some(celsius) => Outcome::Read {
            reading: Reading {
                at,
                passed: None,
                temperature_c: Some(celsius),
                wear_percent: None,
                written_bytes: None,
                power_on_hours: None,
                critical_warning: None,
                media_errors: None,
                self_tests: Vec::new(),
            },
            model,
        },
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
