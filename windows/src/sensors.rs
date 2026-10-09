//! Local machine readings for the CPU, memory and disk rings: CPU busy share from
//! `GetSystemTimes` deltas, physical and commit memory from `GlobalMemoryStatusEx`, and the
//! fixed drives with the system drive singled out. A failing counter yields `None` (shown
//! as `--`), never zero; failures are logged once per episode.

use crate::diag::{self, FailureLatch, Transition};
use crate::lifecycle::cpu_fraction;
use std::mem::size_of;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;
use windows::core::{Error, PCWSTR};

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

#[derive(Clone, Debug, PartialEq)]
pub struct Machine {
    /// Share of all logical processors busy since the previous sample.
    pub cpu: Option<f32>,
    /// Logical processors, 0 when unknown.
    pub cores: u32,
    pub memory: Option<MemInfo>,
    pub drives: Vec<Drive>,
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
        Machine {
            cpu: cpu.map(|v| v.clamp(0.0, 1.0)),
            cores: std::thread::available_parallelism()
                .map(|n| n.get() as u32)
                .unwrap_or(0),
            memory,
            drives,
        }
    }
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
fn read_drives() -> Vec<Drive> {
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
