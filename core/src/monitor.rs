//! Bounded, read-only extended monitoring.
//!
//! OS-specific socket and power APIs are intentionally kept behind small
//! supplied-table adapters.  This keeps the core truthful when an adapter is
//! unavailable, denied, or cannot prove process identity.

use crate::{ProcessIdentity, ProcessInfo, procs};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;
use sysinfo::Networks;

/// Maximum number of samples retained for one process identity by default.
pub const DEFAULT_HISTORY_CAP: usize = 120;

/// Extended state includes Unknown because absence of a fact is distinct from
/// a provider that is known to be unavailable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ObservationCapability {
    Available,
    Unavailable,
    PermissionDenied,
    Unsupported,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Observation<T> {
    pub value: Option<T>,
    pub capability: ObservationCapability,
    pub label: String,
}

impl<T> Observation<T> {
    fn unavailable(label: impl Into<String>) -> Self {
        Self {
            value: None,
            capability: ObservationCapability::Unavailable,
            label: label.into(),
        }
    }

    fn unsupported(label: impl Into<String>) -> Self {
        Self {
            value: None,
            capability: ObservationCapability::Unsupported,
            label: label.into(),
        }
    }

    fn unknown(label: impl Into<String>) -> Self {
        Self {
            value: None,
            capability: ObservationCapability::Unknown,
            label: label.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct NetworkCounters {
    pub received_bytes: u64,
    pub transmitted_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetworkRates {
    pub received_bytes_per_second: f64,
    pub transmitted_bytes_per_second: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetworkStatus {
    pub counters: Observation<NetworkCounters>,
    pub rates: Observation<NetworkRates>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetworkSample {
    /// Monotonic sample time supplied by caller. Milliseconds avoid wall-clock
    /// jumps and make persisted/fixture samples straightforward.
    pub timestamp_ms: u64,
    pub counters: NetworkCounters,
}

/// Computes rates from monotonically increasing OS counters. A decrease is
/// treated as a provider reset/restart, never as a huge rate.
#[derive(Clone, Debug, Default)]
pub struct NetworkRateSampler {
    previous: Option<NetworkSample>,
}

impl NetworkRateSampler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.previous = None;
    }

    pub fn sample_current(&mut self, timestamp_ms: u64) -> Observation<NetworkRates> {
        let Observation {
            value,
            capability,
            label,
        } = read_network_counters();
        match value {
            Some(counters) => self.sample(NetworkSample {
                timestamp_ms,
                counters,
            }),
            None => Observation {
                value: None,
                capability,
                label,
            },
        }
    }

    pub fn sample(&mut self, current: NetworkSample) -> Observation<NetworkRates> {
        let Some(previous) = self.previous.replace(current) else {
            return Observation::unknown("network rate requires two samples");
        };
        let Some(elapsed_ms) = current.timestamp_ms.checked_sub(previous.timestamp_ms) else {
            return Observation::unknown("network sample time moved backwards");
        };
        if elapsed_ms == 0 {
            return Observation::unknown("network sample interval is zero");
        }
        let Some(received) = current
            .counters
            .received_bytes
            .checked_sub(previous.counters.received_bytes)
        else {
            return Observation::unknown("received counter reset or wrapped");
        };
        let Some(transmitted) = current
            .counters
            .transmitted_bytes
            .checked_sub(previous.counters.transmitted_bytes)
        else {
            return Observation::unknown("transmitted counter reset or wrapped");
        };
        let seconds = elapsed_ms as f64 / 1_000.0;
        Observation {
            value: Some(NetworkRates {
                received_bytes_per_second: received as f64 / seconds,
                transmitted_bytes_per_second: transmitted as f64 / seconds,
            }),
            capability: ObservationCapability::Available,
            label: "OS network byte counters over monotonic interval".into(),
        }
    }
}

/// Aggregate sysinfo's per-interface counters without allowing integer wrap.
pub fn read_network_counters() -> Observation<NetworkCounters> {
    let networks = Networks::new_with_refreshed_list();
    if networks.list().is_empty() {
        return Observation::unavailable("OS reported no network interfaces");
    }
    let mut counters = NetworkCounters::default();
    for (_name, data) in &networks {
        // `received`/`transmitted` are refresh deltas. Lifetime totals are
        // required here because this function creates a fresh Networks view.
        let Some(received) = counters.received_bytes.checked_add(data.total_received()) else {
            return Observation::unknown("received network counter overflow");
        };
        let Some(transmitted) = counters
            .transmitted_bytes
            .checked_add(data.total_transmitted())
        else {
            return Observation::unknown("transmitted network counter overflow");
        };
        counters.received_bytes = received;
        counters.transmitted_bytes = transmitted;
    }
    Observation {
        value: Some(counters),
        capability: ObservationCapability::Available,
        label: "OS network byte counters, aggregated by interface".into(),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BatteryStatus {
    pub charge_percent: Observation<f32>,
    pub seconds_remaining: Observation<u64>,
    pub charging: Observation<bool>,
}

/// Portable input for native power adapters. `charge_percent` is a percentage,
/// never a 0..1 fraction; invalid units become Unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NativeBatteryReading {
    pub charge_percent: Option<f64>,
    pub seconds_remaining: Option<u64>,
    pub charging: Option<bool>,
}

pub fn normalize_battery(reading: NativeBatteryReading) -> BatteryStatus {
    let charge_percent = match reading.charge_percent {
        Some(value) if value.is_finite() && (0.0..=100.0).contains(&value) => Observation {
            value: Some(value as f32),
            capability: ObservationCapability::Available,
            label: "battery charge percentage".into(),
        },
        Some(_) => Observation::unknown("battery charge has invalid percentage units"),
        None => Observation::unknown("battery charge was not supplied by native adapter"),
    };
    let seconds_remaining = reading
        .seconds_remaining
        .map(|value| Observation {
            value: Some(value),
            capability: ObservationCapability::Available,
            label: "battery estimated seconds remaining".into(),
        })
        .unwrap_or_else(|| Observation::unknown("battery time remaining unavailable"));
    let charging = reading
        .charging
        .map(|value| Observation {
            value: Some(value),
            capability: ObservationCapability::Available,
            label: "battery charging state".into(),
        })
        .unwrap_or_else(|| Observation::unknown("battery charging state unavailable"));
    BatteryStatus {
        charge_percent,
        seconds_remaining,
        charging,
    }
}

pub fn unsupported_battery() -> BatteryStatus {
    BatteryStatus {
        charge_percent: Observation::unsupported("battery provider is not wired on this target"),
        seconds_remaining: Observation::unsupported("battery provider is not wired on this target"),
        charging: Observation::unsupported("battery provider is not wired on this target"),
    }
}

#[cfg(windows)]
#[repr(C)]
struct NativeSystemPowerStatus {
    ac_line_status: u8,
    battery_flag: u8,
    battery_life_percent: u8,
    system_status_flag: u8,
    battery_life_time: u32,
    battery_full_life_time: u32,
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetSystemPowerStatus(status: *mut NativeSystemPowerStatus) -> i32;
}

/// Read Windows' bounded power summary without spawning a process. Windows
/// uses 255 and UINT32_MAX as unknown sentinels; these become Unknown.
#[cfg(windows)]
pub fn read_battery() -> BatteryStatus {
    let mut raw = std::mem::MaybeUninit::<NativeSystemPowerStatus>::uninit();
    let ok = unsafe { GetSystemPowerStatus(raw.as_mut_ptr()) } != 0;
    if !ok {
        return BatteryStatus {
            charge_percent: Observation::unavailable("GetSystemPowerStatus failed"),
            seconds_remaining: Observation::unavailable("GetSystemPowerStatus failed"),
            charging: Observation::unavailable("GetSystemPowerStatus failed"),
        };
    }
    let raw = unsafe { raw.assume_init() };
    normalize_battery(NativeBatteryReading {
        charge_percent: (raw.battery_life_percent != 255)
            .then_some(raw.battery_life_percent as f64),
        seconds_remaining: (raw.battery_life_time != u32::MAX)
            .then_some(raw.battery_life_time as u64),
        charging: match raw.ac_line_status {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        },
    })
}

#[cfg(not(windows))]
pub fn read_battery() -> BatteryStatus {
    unsupported_battery()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeListener {
    pub protocol: String,
    pub local_address: String,
    pub port: u16,
    pub pid: Option<u32>,
    /// Required for safe association. A PID alone is not a process identity.
    pub start_time: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ListeningPort {
    pub protocol: String,
    pub local_address: String,
    pub port: u16,
    pub owner: Option<ProcessIdentity>,
    pub capability: ObservationCapability,
    pub reason: String,
}

/// Associate native socket rows only when PID and process start time both
/// match. This rejects PID reuse and missing-owner ambiguity.
pub fn normalize_listeners(
    rows: &[NativeListener],
    processes: &[ProcessIdentity],
) -> Vec<ListeningPort> {
    let identities: BTreeMap<u32, Vec<&ProcessIdentity>> =
        processes.iter().fold(BTreeMap::new(), |mut map, identity| {
            map.entry(identity.pid).or_default().push(identity);
            map
        });
    let mut normalized = rows
        .iter()
        .map(|row| {
            let (owner, capability, reason) = match (row.pid, row.start_time) {
                (None, _) => (
                    None,
                    ObservationCapability::Unknown,
                    "owner PID unavailable",
                ),
                (Some(_), None) => (
                    None,
                    ObservationCapability::Unknown,
                    "owner start time unavailable; PID reuse cannot be excluded",
                ),
                (Some(pid), Some(start_time)) => match identities.get(&pid) {
                    Some(found) if found.iter().any(|item| item.start_time == start_time) => (
                        Some(ProcessIdentity { pid, start_time }),
                        ObservationCapability::Available,
                        "owner matched by PID and start time",
                    ),
                    Some(_) => (
                        None,
                        ObservationCapability::Unknown,
                        "owner PID exists but start time does not match",
                    ),
                    None => (
                        None,
                        ObservationCapability::Unknown,
                        "owner process was not present in process snapshot",
                    ),
                },
            };
            ListeningPort {
                protocol: row.protocol.clone(),
                local_address: row.local_address.clone(),
                port: row.port,
                owner,
                capability,
                reason: reason.into(),
            }
        })
        .collect::<Vec<_>>();
    normalized.sort_by(|left, right| {
        (
            &left.protocol,
            &left.local_address,
            left.port,
            left.owner.as_ref().map(|identity| identity.pid),
        )
            .cmp(&(
                &right.protocol,
                &right.local_address,
                right.port,
                right.owner.as_ref().map(|identity| identity.pid),
            ))
    });
    normalized
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResourceSample {
    pub sampled_at_ms: u64,
    pub identity: ProcessIdentity,
    pub name: String,
    pub cpu_usage_percent: f32,
    pub memory_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResourceHistory {
    capacity_per_process: usize,
    samples: BTreeMap<(u32, u64), Vec<ResourceSample>>,
}

impl ResourceHistory {
    pub fn new(capacity_per_process: usize) -> Self {
        Self {
            capacity_per_process: capacity_per_process.max(1),
            samples: BTreeMap::new(),
        }
    }

    pub fn capacity_per_process(&self) -> usize {
        self.capacity_per_process
    }

    pub fn push(&mut self, sample: ResourceSample) {
        let key = (sample.identity.pid, sample.identity.start_time);
        let entries = self.samples.entry(key).or_default();
        entries.push(sample);
        if entries.len() > self.capacity_per_process {
            let excess = entries.len() - self.capacity_per_process;
            entries.drain(0..excess);
        }
    }

    pub fn push_processes(&mut self, sampled_at_ms: u64, processes: &[ProcessInfo]) {
        for process in processes {
            if process.cpu_usage_percent.is_finite() && process.cpu_usage_percent >= 0.0 {
                if let Some(memory_bytes) = process.memory.value {
                    self.push(ResourceSample {
                        sampled_at_ms,
                        identity: process.identity.clone(),
                        name: process.name.clone(),
                        cpu_usage_percent: process.cpu_usage_percent,
                        memory_bytes,
                    });
                }
            }
        }
    }

    pub fn get(&self, identity: &ProcessIdentity) -> Option<&[ResourceSample]> {
        self.samples
            .get(&(identity.pid, identity.start_time))
            .map(Vec::as_slice)
    }

    pub fn process_count(&self) -> usize {
        self.samples.len()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExtendedStatus {
    pub network: NetworkStatus,
    pub battery: BatteryStatus,
    pub listening_ports: Observation<Vec<ListeningPort>>,
    pub processes: Vec<ProcessInfo>,
}

/// Perform bounded real OS reads currently available through shared core APIs.
/// Battery and socket enumeration remain explicit unsupported adapters until a
/// target-native implementation can prove permissions and process identity.
pub fn sample_extended() -> ExtendedStatus {
    let counters = read_network_counters();
    let processes = procs();
    let rates = Observation::unavailable("network rate requires caller-owned sampler state");
    ExtendedStatus {
        network: NetworkStatus { counters, rates },
        battery: read_battery(),
        listening_ports: Observation::unsupported("listening socket provider is not wired"),
        processes,
    }
}

/// Converts a standard duration to a bounded sample interval. Kept public for
/// native adapters that use `Duration` while avoiding division by zero.
pub fn duration_millis(duration: Duration) -> Option<u64> {
    u64::try_from(duration.as_millis())
        .ok()
        .filter(|value| *value > 0)
}
