//! Pulse's conservative, read-only shared core.

pub mod activity;
#[cfg(unix)]
pub mod app_manager;
pub mod apps;
#[cfg(windows)]
pub mod apps_windows;
#[cfg(feature = "localsend")]
pub mod bridge;
pub mod claude_sync;
pub mod cleanup;
pub mod cleanup_scan;
pub mod compression;
pub mod dashboard_export;
pub mod drive_health;
pub mod duplicates;
pub mod folder_growth;
pub mod history;
pub mod ipc;
#[cfg(feature = "localsend")]
pub mod localsend;
pub mod model;
pub mod monitor;
pub mod platform;
pub mod presentation;
#[cfg(unix)]
pub mod process_control;
pub mod processes;
pub mod rules;
pub mod scan;
pub mod state_migration;
pub mod storage_browser;
pub mod store;
pub mod usage_snapshot;
pub mod worker;

pub use model::*;
pub use scan::{FilesystemProvider, StdFilesystemProvider, scan, scan_paths, scan_with_provider};

use serde::{Deserialize, Serialize};
use sysinfo::{Disks, System};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Capability {
    Available,
    Unavailable,
    PermissionDenied,
    Unsupported,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metric<T> {
    pub value: Option<T>,
    pub capability: Capability,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SystemStatus {
    pub cpu_usage_percent: Metric<f32>,
    pub memory_used_bytes: Metric<u64>,
    pub memory_total_bytes: Metric<u64>,
    pub memory_pressure: Metric<String>,
    pub swap_used_bytes: Metric<u64>,
    pub swap_total_bytes: Metric<u64>,
    pub disks: Vec<DiskStatus>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiskStatus {
    pub mount_point: String,
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub removable: bool,
    pub capability: Capability,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_time: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub identity: ProcessIdentity,
    pub name: String,
    pub parent_pid: Option<u32>,
    pub cpu_usage_percent: f32,
    pub memory: Metric<u64>,
    pub gpu_usage_percent: Metric<f32>,
}

/// One cheap reading of CPU, memory and swap. No process walk and no disk
/// list, so it can run every couple of seconds.
#[derive(Clone, Debug)]
pub struct LiveReading {
    pub cpu_percent: f32,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub swap_used_bytes: u64,
    pub swap_total_bytes: u64,
    /// "Normal", "Elevated" or "Critical"; `None` where it is not read.
    pub memory_pressure: Option<String>,
}

/// Keeps a `System` between readings: sysinfo computes CPU deltas between
/// refreshes, so the first reading after `new` is not a measurement.
pub struct LiveSampler {
    system: System,
}

impl Default for LiveSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveSampler {
    pub fn new() -> Self {
        let mut system = System::new();
        system.refresh_cpu_usage();
        system.refresh_memory();
        Self { system }
    }

    pub fn sample(&mut self) -> LiveReading {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        LiveReading {
            cpu_percent: self.system.global_cpu_usage(),
            memory_used_bytes: self.system.used_memory(),
            memory_total_bytes: self.system.total_memory(),
            swap_used_bytes: self.system.used_swap(),
            swap_total_bytes: self.system.total_swap(),
            memory_pressure: memory_pressure(),
        }
    }
}

/// The platform's memory pressure, or `None` where it is not read.
pub fn memory_pressure() -> Option<String> {
    memory_pressure_metric().value
}

pub fn system_status() -> SystemStatus {
    let mut sampler = LiveSampler::new();
    // sysinfo computes CPU deltas between refreshes. Keep this bounded and
    // avoid publishing an immediate zero as a reading.
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    let reading = sampler.sample();
    let cpu = reading.cpu_percent;
    let memory_label = "OS-reported system memory";
    let swap_total = reading.swap_total_bytes;
    let swap_used = reading.swap_used_bytes;
    let disks = Disks::new_with_refreshed_list();
    let disks = disks
        .list()
        .iter()
        .map(|disk| DiskStatus {
            mount_point: disk.mount_point().to_string_lossy().into_owned(),
            total_bytes: Some(disk.total_space()),
            available_bytes: Some(disk.available_space()),
            removable: disk.is_removable(),
            capability: Capability::Available,
        })
        .collect();
    SystemStatus {
        cpu_usage_percent: Metric {
            value: Some(cpu),
            capability: Capability::Available,
            label: "total CPU usage".into(),
        },
        memory_used_bytes: Metric {
            value: Some(reading.memory_used_bytes),
            capability: Capability::Available,
            label: memory_label.into(),
        },
        memory_total_bytes: Metric {
            value: Some(reading.memory_total_bytes),
            capability: Capability::Available,
            label: memory_label.into(),
        },
        memory_pressure: memory_pressure_metric(),
        swap_used_bytes: Metric {
            value: Some(swap_used),
            capability: Capability::Available,
            label: "swap used".into(),
        },
        swap_total_bytes: Metric {
            value: Some(swap_total),
            capability: Capability::Available,
            label: "swap total".into(),
        },
        disks,
    }
}

pub fn procs() -> Vec<ProcessInfo> {
    let mut system = System::new_all();
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let mut processes: Vec<_> = system
        .processes()
        .values()
        .map(|process| ProcessInfo {
            identity: ProcessIdentity {
                pid: process.pid().as_u32(),
                start_time: process.start_time(),
            },
            name: process.name().to_string_lossy().into_owned(),
            parent_pid: process.parent().map(|pid| pid.as_u32()),
            cpu_usage_percent: process.cpu_usage(),
            memory: Metric {
                value: Some(process.memory()),
                capability: Capability::Available,
                label: if cfg!(windows) {
                    "working set; private bytes unavailable"
                } else {
                    "resident memory (RSS); physical footprint unavailable"
                }
                .into(),
            },
            gpu_usage_percent: Metric {
                value: None,
                capability: Capability::Unsupported,
                label: "per-process GPU unavailable".into(),
            },
        })
        .collect();
    processes.sort_by_key(|process| (process.identity.pid, process.identity.start_time));
    processes
}

/// macOS reports memory pressure as 1 (normal), 2 (warning) or 4 (critical).
#[cfg(target_os = "macos")]
fn read_memory_pressure() -> Option<String> {
    let mut level: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.memorystatus_vm_pressure_level".as_ptr(),
            (&mut level as *mut libc::c_int).cast::<libc::c_void>(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    match level {
        1 => Some("Normal".into()),
        2 => Some("Elevated".into()),
        4 => Some("Critical".into()),
        _ => None,
    }
}

/// Windows has no pressure level, so it is derived from `GlobalMemoryStatusEx`:
/// commit charge against the commit limit (RAM plus page files), and available
/// physical memory. Critical at 95% commit or under 5% of RAM available;
/// Elevated at 85% commit or under 10% of RAM available; otherwise Normal.
#[cfg(target_os = "windows")]
fn read_memory_pressure() -> Option<String> {
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(status: *mut MemoryStatusEx) -> i32;
    }
    let mut status = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        memory_load: 0,
        total_phys: 0,
        avail_phys: 0,
        total_page_file: 0,
        avail_page_file: 0,
        total_virtual: 0,
        avail_virtual: 0,
        avail_extended_virtual: 0,
    };
    // SAFETY: `status` is a live, correctly sized MEMORYSTATUSEX with `length` set.
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 || status.total_phys == 0 {
        return None;
    }
    let commit_limit = status.total_page_file;
    let committed = commit_limit.saturating_sub(status.avail_page_file);
    let commit_ratio = if commit_limit == 0 {
        0.0
    } else {
        committed as f64 / commit_limit as f64
    };
    let available_ratio = status.avail_phys as f64 / status.total_phys as f64;
    Some(
        if commit_ratio >= 0.95 || available_ratio < 0.05 {
            "Critical"
        } else if commit_ratio >= 0.85 || available_ratio < 0.10 {
            "Elevated"
        } else {
            "Normal"
        }
        .into(),
    )
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn read_memory_pressure() -> Option<String> {
    None
}

fn memory_pressure_metric() -> Metric<String> {
    #[cfg(target_os = "macos")]
    let label = "macOS memory pressure";
    #[cfg(target_os = "windows")]
    let label = "Windows commit pressure";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let label = "platform memory pressure";
    let value = read_memory_pressure();
    Metric {
        capability: if value.is_some() {
            Capability::Available
        } else {
            Capability::Unavailable
        },
        value,
        label: label.into(),
    }
}
