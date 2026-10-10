//! Local machine readings for the CPU, memory and disk rings: CPU busy share from
//! `GetSystemTimes` deltas, physical and commit memory from `GlobalMemoryStatusEx`, and the
//! fixed drives with the system drive singled out. A failing counter yields `None` (shown
//! as `--`), never zero; failures are logged once per episode.
//!
//! The System card's extra rows (GPU busy share from the PDH "GPU Engine" counters, network
//! rates from `GetIfTable2` over physical adapters) are sampled on their own background
//! thread, because the GPU counter needs two collections a second apart. The UI thread only
//! copies the latest result. The one temperature Windows gives an unelevated process is the
//! NVIDIA GPU's, read through the driver's `nvml.dll` (loaded from System32, handles cached,
//! sampled every few seconds). CPU zones (`MSAcpi_ThermalZoneTemperature`, the "Thermal Zone
//! Information" counters) are absent or need administrator rights on the PC probed, and
//! `Win32_Fan` carries no speed, so a CPU temperature or fan speed is never estimated: the
//! reading stays unavailable and the card leaves it out.

use crate::diag::{self, FailureLatch, Transition};
use crate::lifecycle::cpu_fraction;
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::{Mutex, Once, PoisonError};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIfTable2, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211,
    IF_TYPE_SOFTWARE_LOOPBACK, IF_TYPE_TUNNEL, MIB_IF_TABLE2,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
    PDH_NO_DATA, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
    PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;
use windows::core::{Error, PCWSTR, s, w};

/// `GetDriveTypeW` result for a fixed (non-removable, non-network) drive.
const DRIVE_FIXED: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemInfo {
    pub total: u64,
    pub available: u64,
    /// System commit limit and bytes committed (page-file backed).
    pub commit_limit: u64,
    pub commit_used: u64,
}

impl MemInfo {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    pub fn used_fraction(&self) -> f32 {
        (self.used() as f64 / self.total.max(1) as f64).clamp(0.0, 1.0) as f32
    }

    /// The Mac's pressure words from Windows' own inputs: physical memory still available and
    /// the commit charge against the commit limit (the bands of `layout::memory_band`: under
    /// 12.5% free or 85% committed is "warning", under 5% free or 95% committed "critical").
    /// `None` without a total. The value to publish as `memoryPressure`
    /// (`core/src/lib.rs` reports none on Windows).
    pub fn pressure(&self) -> Option<&'static str> {
        if self.total == 0 {
            return None;
        }
        let free = self.available as f64 / self.total as f64;
        let commit = if self.commit_limit > 0 {
            self.commit_used as f64 / self.commit_limit as f64
        } else {
            0.0
        };
        Some(if free < 0.05 || commit >= 0.95 {
            "critical"
        } else if free < 0.125 || commit >= 0.85 {
            "warning"
        } else {
            "normal"
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drive {
    /// Root path such as `C:\`.
    pub root: String,
    pub total: u64,
    pub free: u64,
    pub system: bool,
}

impl Drive {
    pub fn used_fraction(&self) -> f32 {
        let used = self.total.saturating_sub(self.free);
        (used as f64 / self.total.max(1) as f64).clamp(0.0, 1.0) as f32
    }
}

/// A reading the notch may not have yet or may never get: the card says which.
#[derive(Clone, Debug, PartialEq)]
pub enum Reading<T> {
    /// Not sampled yet.
    Pending,
    /// This PC cannot provide it.
    Unavailable,
    Value(T),
}

/// Receive and send rates summed over the physical adapters that are up.
#[derive(Clone, Debug, PartialEq)]
pub struct NetRate {
    /// Bytes per second.
    pub down: f64,
    pub up: f64,
    /// "Wi-Fi", "Ethernet", "Cellular" or "Network": the adapter carrying the most traffic.
    pub kind: String,
}

/// One temperature and where it was read, so the card can say whose it is ("GPU 57 °C").
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Temp {
    pub source: &'static str,
    pub celsius: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Machine {
    /// Share of all logical processors busy since the previous sample.
    pub cpu: Option<f32>,
    /// Logical processors, 0 when unknown.
    pub cores: u32,
    pub memory: Option<MemInfo>,
    pub drives: Vec<Drive>,
    /// Busiest adapter's 3D engine share, 0..=1.
    pub gpu: Reading<f32>,
    pub network: Reading<NetRate>,
    /// Every temperature an unelevated process can read (the NVIDIA GPU's); `Value` is never
    /// empty. There is no CPU temperature without administrator rights or a kernel driver.
    pub temperature: Reading<Vec<Temp>>,
    /// Fan speeds in rpm; Windows reports none (`Win32_Fan` has no speed field filled).
    pub fans: Reading<Vec<u32>>,
}

impl Machine {
    pub fn memory_fraction(&self) -> Option<f32> {
        self.memory.map(|m| m.used_fraction())
    }

    pub fn system_drive(&self) -> Option<&Drive> {
        self.drives.iter().find(|d| d.system)
    }

    /// Used share of the system drive.
    pub fn disk_fraction(&self) -> Option<f32> {
        self.system_drive().map(Drive::used_fraction)
    }
}

/// Sole sampling owner: only the controller's timer path calls `sample`.
pub struct Sampler {
    pressure: Option<&'static str>,
    previous_times: Option<(u64, u64, u64)>,
    cpu: FailureLatch,
    memory: FailureLatch,
    disk: FailureLatch,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub const fn new() -> Self {
        Self {
            pressure: None,
            previous_times: None,
            cpu: FailureLatch::new(),
            memory: FailureLatch::new(),
            disk: FailureLatch::new(),
        }
    }

    pub fn sample(&mut self) -> Machine {
        let cpu = match read_cpu_times() {
            Ok(current) => {
                note(&mut self.cpu, "GetSystemTimes", "cpu");
                let value = cpu_fraction(self.previous_times, current);
                self.previous_times = Some(current);
                value
            }
            Err(error) => {
                if self.cpu.observe(true) == Transition::Failed {
                    diag::win32_error("GetSystemTimes", &error, "cpu");
                }
                self.previous_times = None;
                None
            }
        };
        let memory = match read_memory() {
            Ok(info) => {
                note(&mut self.memory, "GlobalMemoryStatusEx", "memory");
                Some(info)
            }
            Err(error) => {
                if self.memory.observe(true) == Transition::Failed {
                    diag::win32_error("GlobalMemoryStatusEx", &error, "memory");
                }
                None
            }
        };
        let pressure = memory.and_then(|m| m.pressure());
        if pressure != self.pressure {
            diag::info(
                "memory_pressure",
                &[("level", pressure.unwrap_or("unknown"))],
            );
            self.pressure = pressure;
        }
        let drives = read_drives();
        let disk_failed = !drives.iter().any(|d| d.system);
        match self.disk.observe(disk_failed) {
            Transition::Failed => diag::info(
                "sampler_failed",
                &[("op", "GetDiskFreeSpaceExW"), ("ctx", "system_drive")],
            ),
            Transition::Recovered => diag::info(
                "sampler_recovered",
                &[("op", "GetDiskFreeSpaceExW"), ("ctx", "disk")],
            ),
            Transition::Unchanged => {}
        }
        start_extras();
        let extras = EXTRAS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        Machine {
            cpu: cpu.map(|v| v.clamp(0.0, 1.0)),
            cores: std::thread::available_parallelism()
                .map(|n| n.get() as u32)
                .unwrap_or(0),
            memory,
            drives,
            gpu: extras.gpu,
            network: extras.network,
            temperature: with_cpu_temperature(extras.temperature),
            fans: Reading::Unavailable,
        }
    }
}

#[derive(Clone)]
struct Extras {
    gpu: Reading<f32>,
    network: Reading<NetRate>,
    temperature: Reading<Vec<Temp>>,
}

static EXTRAS: Mutex<Extras> = Mutex::new(Extras {
    gpu: Reading::Pending,
    network: Reading::Pending,
    temperature: Reading::Pending,
});
static EXTRAS_START: Once = Once::new();

/// Starts the background sampler once; if it cannot start, the rows say unavailable.
fn start_extras() {
    EXTRAS_START.call_once(|| {
        let spawned = std::thread::Builder::new()
            .name("pulse-sensors".into())
            .spawn(extras_loop);
        if spawned.is_err() {
            diag::info(
                "sampler_failed",
                &[("op", "spawn"), ("ctx", "sensors_thread")],
            );
            let mut extras = EXTRAS.lock().unwrap_or_else(PoisonError::into_inner);
            extras.gpu = Reading::Unavailable;
            extras.network = Reading::Unavailable;
            extras.temperature = Reading::Unavailable;
        }
    });
}

fn extras_loop() {
    let mut gpu_latch = FailureLatch::new();
    let mut net_latch = FailureLatch::new();
    let mut temp_latch = FailureLatch::new();
    let mut previous: Option<(Instant, Vec<Iface>)> = None;
    // The GPU's thermal sensor moves slowly: asking every few seconds is plenty and keeps a
    // sleeping hybrid-graphics GPU from being poked on every pass.
    let nvml = Nvml::open();
    let mut temperature = if nvml.is_some() {
        Reading::Pending
    } else {
        Reading::Unavailable
    };
    let mut tick = 0u32;
    loop {
        let taken = Instant::now();
        let current = read_interfaces();
        let network = match &current {
            Some(now) => {
                let rate = previous
                    .as_ref()
                    .and_then(|(at, before)| net_rate(before, now, taken.duration_since(*at)));
                match rate {
                    Some(rate) => Reading::Value(rate),
                    None if now.is_empty() => Reading::Unavailable,
                    None => Reading::Pending,
                }
            }
            None => Reading::Unavailable,
        };
        let net_failed = matches!(network, Reading::Unavailable);
        log_transition(net_latch.observe(net_failed), "GetIfTable2", "network");
        previous = current.map(|now| (taken, now));

        let gpu = match read_gpu() {
            Ok(share) => Reading::Value(share),
            Err(_) => Reading::Unavailable,
        };
        log_transition(
            gpu_latch.observe(matches!(gpu, Reading::Unavailable)),
            "PdhGetFormattedCounterArrayW",
            "gpu",
        );
        if let Some(nvml) = &nvml
            && tick.is_multiple_of(NVML_EVERY)
        {
            temperature = match nvml.temperature() {
                Some(celsius) => Reading::Value(vec![Temp {
                    source: "GPU",
                    celsius,
                }]),
                None => Reading::Unavailable,
            };
            log_transition(
                temp_latch.observe(matches!(temperature, Reading::Unavailable)),
                "nvmlDeviceGetTemperature",
                "gpu_temperature",
            );
        }
        tick = tick.wrapping_add(1);
        {
            let mut extras = EXTRAS.lock().unwrap_or_else(PoisonError::into_inner);
            extras.gpu = gpu;
            extras.network = network;
            extras.temperature = temperature.clone();
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn log_transition(transition: Transition, op: &str, ctx: &str) {
    match transition {
        Transition::Failed => diag::info("sampler_failed", &[("op", op), ("ctx", ctx)]),
        Transition::Recovered => diag::info("sampler_recovered", &[("op", op), ("ctx", ctx)]),
        Transition::Unchanged => {}
    }
}

/// Loop passes (about three seconds each) between two NVML temperature reads.
const NVML_EVERY: u32 = 4;

type NvmlDevice = *mut c_void;

/// NVIDIA's management library: the display driver ships `nvml.dll` in System32 and its
/// temperature query needs no elevation. Loaded dynamically (no import, so nothing breaks
/// on PCs without the driver); the device handles are fetched once and kept. Owned by the
/// sampling thread alone, so the raw handles never cross threads.
struct Nvml {
    devices: Vec<NvmlDevice>,
    /// `nvmlDeviceGetTemperature(device, NVML_TEMPERATURE_GPU = 0, &mut degrees_celsius)`.
    get_temperature: unsafe extern "C" fn(NvmlDevice, i32, *mut u32) -> i32,
}

impl Nvml {
    /// Loads the library and the first few GPUs; `None` without an NVIDIA driver or GPU.
    fn open() -> Option<Self> {
        // The system directory only: a copy next to the exe is never loaded.
        // SAFETY: a valid NUL-terminated name; the module stays loaded for the process.
        let module =
            unsafe { LoadLibraryExW(w!("nvml.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }.ok()?;
        // SAFETY (the four transmutes): the exports have exactly these C signatures in NVML's
        // public header; the function pointer types differ only in ABI and arguments.
        let init: unsafe extern "C" fn() -> i32 =
            unsafe { std::mem::transmute(GetProcAddress(module, s!("nvmlInit_v2"))?) };
        let device_count: unsafe extern "C" fn(*mut u32) -> i32 =
            unsafe { std::mem::transmute(GetProcAddress(module, s!("nvmlDeviceGetCount_v2"))?) };
        let device_at: unsafe extern "C" fn(u32, *mut NvmlDevice) -> i32 = unsafe {
            std::mem::transmute(GetProcAddress(module, s!("nvmlDeviceGetHandleByIndex_v2"))?)
        };
        let get_temperature: unsafe extern "C" fn(NvmlDevice, i32, *mut u32) -> i32 =
            unsafe { std::mem::transmute(GetProcAddress(module, s!("nvmlDeviceGetTemperature"))?) };
        // SAFETY: `init` takes no arguments; NVML_SUCCESS is 0.
        if unsafe { init() } != 0 {
            return None;
        }
        let mut count = 0u32;
        // SAFETY: `count` is a valid out pointer for the call.
        if unsafe { device_count(&mut count) } != 0 {
            return None;
        }
        let mut devices = Vec::new();
        for index in 0..count.min(8) {
            let mut handle: NvmlDevice = std::ptr::null_mut();
            // SAFETY: `handle` is a valid out pointer; the handle stays valid until NVML is
            // shut down, which this process never does.
            if unsafe { device_at(index, &mut handle) } == 0 && !handle.is_null() {
                devices.push(handle);
            }
        }
        if devices.is_empty() {
            return None;
        }
        Some(Self {
            devices,
            get_temperature,
        })
    }

    /// The hottest GPU's core temperature in degrees Celsius; `None` when none answers.
    fn temperature(&self) -> Option<f32> {
        self.devices
            .iter()
            .filter_map(|device| {
                let mut degrees = 0u32;
                // SAFETY: a handle NVML returned and an out pointer valid for the call.
                let status = unsafe { (self.get_temperature)(*device, 0, &mut degrees) };
                (status == 0).then_some(degrees as f32)
            })
            .reduce(f32::max)
    }
}

/// One physical adapter's lifetime byte counters.
struct Iface {
    index: u32,
    kind: &'static str,
    received: u64,
    sent: u64,
}

/// `MIB_IF_ROW2` interface-flag bit 0: a physical (hardware) adapter. Virtual switches,
/// VPN and Wi-Fi Direct adapters lack it, so their traffic (already counted on the physical
/// adapter, or never leaving the PC) is excluded along with loopback and tunnels.
const HARDWARE_INTERFACE: u8 = 1;
/// `IF_TYPE` values for mobile broadband (GSM and CDMA).
const IF_TYPE_WWANPP: u32 = 243;
const IF_TYPE_WWANPP2: u32 = 244;

/// Physical adapters that are up, or `None` when the table cannot be read.
fn read_interfaces() -> Option<Vec<Iface>> {
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    // `GetIfTable2` allocates the table; it is read here and released with `FreeMibTable`.
    let status = unsafe { GetIfTable2(&mut table) };
    if status.0 != 0 || table.is_null() {
        return None;
    }
    let rows = unsafe {
        std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize)
    };
    let found = rows
        .iter()
        .filter(|row| {
            row.OperStatus == IfOperStatusUp
                && row.InterfaceAndOperStatusFlags._bitfield & HARDWARE_INTERFACE != 0
                && row.Type != IF_TYPE_SOFTWARE_LOOPBACK
                && row.Type != IF_TYPE_TUNNEL
        })
        .map(|row| Iface {
            index: row.InterfaceIndex,
            kind: match row.Type {
                IF_TYPE_IEEE80211 => "Wi-Fi",
                IF_TYPE_ETHERNET_CSMACD => "Ethernet",
                IF_TYPE_WWANPP | IF_TYPE_WWANPP2 => "Cellular",
                _ => "Network",
            },
            received: row.InOctets,
            sent: row.OutOctets,
        })
        .collect();
    unsafe { FreeMibTable(table as *const _) };
    Some(found)
}

/// Bytes per second since `before`. An adapter whose counter went backwards (reset) is
/// skipped; `None` when no adapter was present in both samples.
fn net_rate(before: &[Iface], now: &[Iface], elapsed: Duration) -> Option<NetRate> {
    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        return None;
    }
    let (mut down, mut up) = (0u64, 0u64);
    let mut busiest: Option<(u64, &'static str)> = None;
    for adapter in now {
        let Some(old) = before.iter().find(|o| o.index == adapter.index) else {
            continue;
        };
        let (Some(rx), Some(tx)) = (
            adapter.received.checked_sub(old.received),
            adapter.sent.checked_sub(old.sent),
        ) else {
            continue;
        };
        down = down.saturating_add(rx);
        up = up.saturating_add(tx);
        let traffic = rx.saturating_add(tx);
        if busiest.is_none_or(|(most, _)| traffic > most) {
            busiest = Some((traffic, adapter.kind));
        }
    }
    let (_, kind) = busiest?;
    Some(NetRate {
        down: down as f64 / seconds,
        up: up as f64 / seconds,
        kind: kind.to_string(),
    })
}

/// 3D-engine utilisation of the busiest GPU adapter, 0..=1. The PDH counter needs two
/// collections, so this takes about a second. Instances are per process and engine
/// ("pid_..._luid_0xHIGH_0xLOW_phys_0_eng_3_engtype_3D"); they are summed per adapter (luid).
/// Fails (PDH status) when the counter set does not exist, e.g. no WDDM GPU driver.
fn read_gpu() -> Result<f32, u32> {
    let mut query = PDH_HQUERY(std::ptr::null_mut());
    let status = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) };
    if status != 0 {
        return Err(status);
    }
    let result = gpu_in(query);
    unsafe { PdhCloseQuery(query) };
    result
}

fn gpu_in(query: PDH_HQUERY) -> Result<f32, u32> {
    let path: Vec<u16> = "\\GPU Engine(*engtype_3D)\\Utilization Percentage"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut counter = PDH_HCOUNTER(std::ptr::null_mut());
    let status = unsafe { PdhAddEnglishCounterW(query, PCWSTR(path.as_ptr()), 0, &mut counter) };
    if status != 0 {
        return Err(status);
    }
    let status = unsafe { PdhCollectQueryData(query) };
    if status != 0 {
        return Err(status);
    }
    std::thread::sleep(Duration::from_secs(1));
    let status = unsafe { PdhCollectQueryData(query) };
    if status != 0 {
        return Err(status);
    }
    let mut size = 0u32;
    let mut count = 0u32;
    let status = unsafe {
        PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, None)
    };
    // No 3D engine instance exists: nothing is using the GPU.
    if status == PDH_NO_DATA {
        return Ok(0.0);
    }
    if status != PDH_MORE_DATA {
        return Err(status);
    }
    // 8-byte aligned storage for the item array and the names that follow it.
    let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
    let items = buffer.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
    let status = unsafe {
        PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items))
    };
    if status != 0 {
        return Err(status);
    }
    // PDH filled `count` items inside `buffer`, which outlives this borrow.
    let items = unsafe { std::slice::from_raw_parts(items, count as usize) };
    let mut per_adapter: HashMap<String, f64> = HashMap::new();
    for item in items {
        // CStatus 0 is valid data, 1 is valid new data.
        if item.FmtValue.CStatus > 1 {
            continue;
        }
        let name = unsafe { item.szName.to_string() }.unwrap_or_default();
        let value = unsafe { item.FmtValue.Anonymous.doubleValue };
        *per_adapter.entry(adapter_of(&name)).or_default() += value.max(0.0);
    }
    let busiest = per_adapter.values().copied().fold(0.0f64, f64::max);
    Ok((busiest / 100.0).clamp(0.0, 1.0) as f32)
}

/// The adapter part of a GPU Engine instance name: the text from "luid_" to "_phys".
fn adapter_of(instance: &str) -> String {
    let start = instance.find("luid_").unwrap_or(0);
    let tail = &instance[start..];
    let end = tail.find("_phys").unwrap_or(tail.len());
    tail[..end].to_string()
}

fn note(latch: &mut FailureLatch, op: &str, ctx: &str) {
    if latch.observe(false) == Transition::Recovered {
        diag::info("sampler_recovered", &[("op", op), ("ctx", ctx)]);
    }
}

fn read_cpu_times() -> Result<(u64, u64, u64), Error> {
    let mut idle = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }?;
    Ok((filetime(idle), filetime(kernel), filetime(user)))
}

fn filetime(value: FILETIME) -> u64 {
    ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64
}

fn read_memory() -> Result<MemInfo, Error> {
    let mut status = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe { GlobalMemoryStatusEx(&mut status) }?;
    Ok(MemInfo {
        total: status.ullTotalPhys,
        available: status.ullAvailPhys,
        commit_limit: status.ullTotalPageFile,
        commit_used: status
            .ullTotalPageFile
            .saturating_sub(status.ullAvailPageFile),
    })
}

/// Fixed local drives that report a capacity. A drive that cannot be read (locked, offline)
/// is left out rather than shown as empty.
pub fn read_drives() -> Vec<Drive> {
    let system_letter = std::env::var("SystemDrive")
        .ok()
        .and_then(|d| d.chars().next())
        .map(|c| c.to_ascii_uppercase())
        .unwrap_or('C');
    let mask = unsafe { GetLogicalDrives() };
    let mut drives = Vec::new();
    for index in 0..26u32 {
        if mask & (1 << index) == 0 {
            continue;
        }
        let letter = char::from(b'A' + index as u8);
        let root = format!("{letter}:\\");
        let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
        if unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) } != DRIVE_FIXED {
            continue;
        }
        let mut free = 0u64;
        let mut total = 0u64;
        let read = unsafe {
            GetDiskFreeSpaceExW(
                PCWSTR(wide.as_ptr()),
                Some(&mut free),
                Some(&mut total),
                None,
            )
        };
        if read.is_ok() && total > 0 {
            drives.push(Drive {
                root,
                total,
                free,
                system: letter == system_letter,
            });
        }
    }
    drives
}

/// Binary gigabytes, labelled as the Mac's cards read them: three significant digits and no
/// trailing ".0" ("22.4 GB", "994 GB", "1 TB", "1.5 TB").
pub fn size_text(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let gib = bytes as f64 / GIB;
    let (value, unit) = if gib >= 1024.0 {
        (gib / 1024.0, "TB")
    } else {
        (gib, "GB")
    };
    let number = if value >= 100.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}").trim_end_matches(".0").to_string()
    };
    format!("{number} {unit}")
}


// ---- battery ----------------------------------------------------------------------------------

#[repr(C)]
#[allow(dead_code)] // mirrors the Win32 layout; not every field is read
struct SystemPowerStatus {
    ac_line_status: u8,
    battery_flag: u8,
    battery_life_percent: u8,
    system_status_flag: u8,
    battery_life_time: u32,
    battery_full_life_time: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetSystemPowerStatus(status: *mut SystemPowerStatus) -> i32;
}

/// The battery as the Mac's System card words it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    /// On mains power (charging or full).
    pub plugged_in: bool,
    pub charging: bool,
    /// Seconds of charge left on battery, when Windows can tell.
    pub seconds_left: Option<u32>,
}

impl Battery {
    /// "82% \u{b7} charging", "64% \u{b7} 2 h 10 min left", "100% \u{b7} plugged in".
    pub fn text(&self) -> String {
        let state = if self.charging {
            "charging".to_string()
        } else if self.plugged_in {
            "plugged in".to_string()
        } else {
            match self.seconds_left {
                Some(s) => format!("{} h {:02} min left", s / 3600, s % 3600 / 60),
                None => "on battery".to_string(),
            }
        };
        format!("{}% \u{b7} {state}", self.percent)
    }
}

/// `Unavailable` on a desktop PC (no battery) or when Windows gives no reading. Win32
/// `GetSystemPowerStatus`; no elevation. Read by the System card's Battery row.
pub fn battery() -> Reading<Battery> {
    let mut raw = SystemPowerStatus {
        ac_line_status: 255,
        battery_flag: 255,
        battery_life_percent: 255,
        system_status_flag: 0,
        battery_life_time: u32::MAX,
        battery_full_life_time: u32::MAX,
    };
    // SAFETY: `raw` is a valid, correctly laid out SYSTEM_POWER_STATUS.
    if unsafe { GetSystemPowerStatus(&mut raw) } == 0 {
        return Reading::Unavailable;
    }
    // Flag 128 is "no system battery"; 255 is unknown. Percent 255 is unknown.
    if raw.battery_flag & 128 != 0 || raw.battery_flag == 255 || raw.battery_life_percent > 100 {
        return Reading::Unavailable;
    }
    Reading::Value(Battery {
        percent: raw.battery_life_percent,
        plugged_in: raw.ac_line_status == 1,
        charging: raw.battery_flag & 8 != 0,
        seconds_left: (raw.battery_life_time != u32::MAX).then_some(raw.battery_life_time),
    })
}

// ---- CPU temperature --------------------------------------------------------------------------

/// Why the row has no number: shown as its tooltip / value on the System card.
pub const CPU_TEMPERATURE_REASON: &str = "Windows only reports it to administrators";
/// The text for a fan row: Windows exposes no fan speed without a vendor driver.
pub const FANS_UNAVAILABLE: &str = "Not available on this PC";

static CPU_TEMPERATURE: Mutex<Option<f32>> = Mutex::new(None);
static CPU_TEMPERATURE_START: Once = Once::new();

/// Puts the CPU's ACPI thermal-zone reading (when this account may read it) ahead of the GPU's.
fn with_cpu_temperature(gpu: Reading<Vec<Temp>>) -> Reading<Vec<Temp>> {
    CPU_TEMPERATURE_START.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("pulse-cpu-temp".into())
            .spawn(cpu_temperature_loop);
    });
    let cpu = *CPU_TEMPERATURE.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(celsius) = cpu else {
        return gpu;
    };
    let mut temps = vec![Temp {
        source: "CPU",
        celsius,
    }];
    if let Reading::Value(more) = gpu {
        temps.extend(more);
    }
    Reading::Value(temps)
}

/// `MSAcpi_ThermalZoneTemperature` (root\wmi) through a hidden PowerShell: first asked once;
/// when the account may not read it (the normal case without administrator rights) the thread
/// ends and the row stays "Windows only reports it to administrators". Otherwise every 30 s.
fn cpu_temperature_loop() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SCRIPT: &str = "$ErrorActionPreference='Stop'; \
        (Get-CimInstance -Namespace root/wmi -ClassName MSAcpi_ThermalZoneTemperature | \
        Measure-Object -Property CurrentTemperature -Maximum).Maximum";
    loop {
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        let tenths_kelvin = output
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|t| t.trim().parse::<f32>().ok());
        let celsius = tenths_kelvin.map(|t| t / 10.0 - 273.15);
        match celsius.filter(|c| (1.0..150.0).contains(c)) {
            Some(c) => {
                *CPU_TEMPERATURE.lock().unwrap_or_else(PoisonError::into_inner) = Some(c);
            }
            None => {
                *CPU_TEMPERATURE.lock().unwrap_or_else(PoisonError::into_inner) = None;
                diag::info(
                    "sampler_failed",
                    &[("op", "MSAcpi_ThermalZoneTemperature"), ("ctx", "cpu_temperature")],
                );
                return;
            }
        }
        std::thread::sleep(Duration::from_secs(30));
    }
}
