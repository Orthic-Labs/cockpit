//! A rolling history of system readings for the hub's graphs. The hub runs in
//! the background for nearby sharing, so the history keeps filling while its
//! window is closed. Each tick is a cheap CPU/memory read plus the network
//! counters; there is no process walk.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pulse_core::monitor::NetworkRateSampler;
use pulse_core::LiveSampler;
use serde::Serialize;

const INTERVAL: Duration = Duration::from_secs(2);
/// 30 minutes at one sample every 2 seconds.
const KEEP: usize = 900;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    /// Unix time in milliseconds.
    pub ts: u64,
    /// Overall CPU use, 0 to 100 across all cores.
    pub cpu: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    /// Null where the platform does not report memory pressure.
    pub pressure: Option<String>,
    /// Bytes per second; null until two counter reads exist or when unreadable.
    pub net_down: Option<f64>,
    pub net_up: Option<f64>,
}

static HISTORY: Mutex<VecDeque<Sample>> = Mutex::new(VecDeque::new());

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn start_background() {
    std::thread::spawn(|| {
        let mut live = LiveSampler::new();
        let mut network = NetworkRateSampler::new();
        let origin = Instant::now();
        // The first CPU reading after creation is not a measurement.
        std::thread::sleep(Duration::from_millis(500));
        let mut next = Instant::now();
        loop {
            let reading = live.sample();
            let rates = network
                .sample_current(origin.elapsed().as_millis() as u64)
                .value;
            let sample = Sample {
                ts: now_ms(),
                cpu: reading.cpu_percent,
                mem_used: reading.memory_used_bytes,
                mem_total: reading.memory_total_bytes,
                swap_used: reading.swap_used_bytes,
                swap_total: reading.swap_total_bytes,
                pressure: pulse_core::memory_pressure(),
                net_down: rates.map(|r| r.received_bytes_per_second),
                net_up: rates.map(|r| r.transmitted_bytes_per_second),
            };
            {
                let mut history = HISTORY.lock().unwrap_or_else(|e| e.into_inner());
                if history.len() >= KEEP {
                    history.pop_front();
                }
                history.push_back(sample);
            }
            next += INTERVAL;
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
        }
    });
}

/// Samples newer than `since_ms`, or the whole 30 minutes when it is absent.
#[tauri::command]
pub fn metrics_history(since_ms: Option<u64>) -> Vec<Sample> {
    let history = HISTORY.lock().unwrap_or_else(|e| e.into_inner());
    history
        .iter()
        .filter(|s| since_ms.is_none_or(|since| s.ts > since))
        .cloned()
        .collect()
}
