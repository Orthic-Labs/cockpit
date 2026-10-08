//! Pulse's conservative, read-only shared core.

pub mod activity;
#[cfg(unix)]
pub mod app_manager;
pub mod apps;
pub mod cleanup;
pub mod cleanup_scan;
pub mod compression;
pub mod dashboard_export;
pub mod duplicates;
pub mod folder_growth;
pub mod history;
pub mod ipc;
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

pub fn system_status() -> SystemStatus {
    let mut system = System::new_all();
    // sysinfo computes CPU deltas between refreshes. Keep this bounded and
    // avoid publishing an immediate zero as a reading.
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    system.refresh_cpu_usage();
    let cpu = system.global_cpu_usage();
    let memory_label = "OS-reported system memory";
    let swap_total = system.total_swap();
    let swap_used = system.used_swap();
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
            value: Some(system.used_memory()),
            capability: Capability::Available,
            label: memory_label.into(),
        },
        memory_total_bytes: Metric {
            value: Some(system.total_memory()),
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

fn memory_pressure_metric() -> Metric<String> {
    #[cfg(target_os = "macos")]
    let label = "macOS memory pressure";
    #[cfg(target_os = "windows")]
    let label = "Windows commit pressure";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let label = "platform memory pressure";
    Metric {
        value: None,
        capability: Capability::Unavailable,
        label: label.into(),
    }
}
