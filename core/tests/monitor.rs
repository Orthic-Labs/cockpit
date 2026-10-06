use cockpit_core::monitor::*;
use cockpit_core::{Capability, Metric, ProcessIdentity, ProcessInfo};

fn counters(received_bytes: u64, transmitted_bytes: u64) -> NetworkCounters {
    NetworkCounters {
        received_bytes,
        transmitted_bytes,
    }
}

#[test]
fn rate_requires_interval_and_handles_zero_time() {
    let mut sampler = NetworkRateSampler::new();
    assert_eq!(
        sampler
            .sample(NetworkSample {
                timestamp_ms: 10,
                counters: counters(100, 50),
            })
            .capability,
        ObservationCapability::Unknown
    );
    let zero = sampler.sample(NetworkSample {
        timestamp_ms: 10,
        counters: counters(120, 80),
    });
    assert_eq!(zero.capability, ObservationCapability::Unknown);
    let good = sampler.sample(NetworkSample {
        timestamp_ms: 1_010,
        counters: counters(220, 180),
    });
    assert_eq!(good.capability, ObservationCapability::Available);
    let rates = good.value.expect("valid interval");
    assert_eq!(rates.received_bytes_per_second, 100.0);
    assert_eq!(rates.transmitted_bytes_per_second, 100.0);
}

#[test]
fn rate_rejects_time_backwards_counter_reset_and_overflow_shape() {
    let mut sampler = NetworkRateSampler::new();
    sampler.sample(NetworkSample {
        timestamp_ms: 100,
        counters: counters(u64::MAX - 1, u64::MAX - 1),
    });
    let wrapped = sampler.sample(NetworkSample {
        timestamp_ms: 200,
        counters: counters(0, 0),
    });
    assert_eq!(wrapped.capability, ObservationCapability::Unknown);

    let backwards = sampler.sample(NetworkSample {
        timestamp_ms: 150,
        counters: counters(1, 1),
    });
    assert_eq!(backwards.capability, ObservationCapability::Unknown);
}

#[test]
fn battery_rejects_invalid_units_and_preserves_valid_reading() {
    let invalid = normalize_battery(NativeBatteryReading {
        charge_percent: Some(150.0),
        seconds_remaining: None,
        charging: Some(true),
    });
    assert_eq!(
        invalid.charge_percent.capability,
        ObservationCapability::Unknown
    );
    assert_eq!(invalid.charging.value, Some(true));

    let valid = normalize_battery(NativeBatteryReading {
        charge_percent: Some(75.0),
        seconds_remaining: Some(3_600),
        charging: Some(false),
    });
    assert_eq!(valid.charge_percent.value, Some(75.0));
    assert_eq!(valid.seconds_remaining.value, Some(3_600));
}

#[test]
fn listener_owner_requires_start_time_and_rejects_pid_reuse() {
    let current = vec![ProcessIdentity {
        pid: 42,
        start_time: 9,
    }];
    let rows = vec![
        NativeListener {
            protocol: "tcp".into(),
            local_address: "127.0.0.1".into(),
            port: 80,
            pid: Some(42),
            start_time: Some(9),
        },
        NativeListener {
            protocol: "tcp".into(),
            local_address: "127.0.0.1".into(),
            port: 81,
            pid: Some(42),
            start_time: Some(8),
        },
        NativeListener {
            protocol: "tcp".into(),
            local_address: "0.0.0.0".into(),
            port: 82,
            pid: None,
            start_time: None,
        },
    ];
    let normalized = normalize_listeners(&rows, &current);
    assert_eq!(normalized.len(), 3);
    let owned = normalized.iter().find(|item| item.port == 80).unwrap();
    assert_eq!(owned.capability, ObservationCapability::Available);
    assert_eq!(owned.owner.as_ref().map(|id| id.start_time), Some(9));
    assert!(
        normalized
            .iter()
            .find(|item| item.port == 81)
            .unwrap()
            .owner
            .is_none()
    );
    assert_eq!(
        normalized
            .iter()
            .find(|item| item.port == 82)
            .unwrap()
            .capability,
        ObservationCapability::Unknown
    );
}

#[test]
fn history_is_bounded_and_pid_reuse_gets_new_identity() {
    let mut history = ResourceHistory::new(2);
    let first = ProcessIdentity {
        pid: 7,
        start_time: 10,
    };
    for at in 0..3 {
        history.push(ResourceSample {
            sampled_at_ms: at,
            identity: first.clone(),
            name: "old".into(),
            cpu_usage_percent: 1.0,
            memory_bytes: at,
        });
    }
    assert_eq!(history.get(&first).unwrap().len(), 2);
    assert_eq!(history.get(&first).unwrap()[0].sampled_at_ms, 1);

    let reused = ProcessIdentity {
        pid: 7,
        start_time: 11,
    };
    history.push(ResourceSample {
        sampled_at_ms: 3,
        identity: reused.clone(),
        name: "new".into(),
        cpu_usage_percent: 2.0,
        memory_bytes: 3,
    });
    assert_eq!(history.process_count(), 2);
    assert_eq!(history.get(&reused).unwrap()[0].name, "new");
}

#[test]
fn process_history_skips_invalid_cpu_or_unknown_memory() {
    let process = ProcessInfo {
        identity: ProcessIdentity {
            pid: 3,
            start_time: 1,
        },
        name: "x".into(),
        parent_pid: None,
        cpu_usage_percent: f32::NAN,
        memory: Metric {
            value: Some(10),
            capability: Capability::Available,
            label: "memory".into(),
        },
        gpu_usage_percent: Metric {
            value: None,
            capability: Capability::Unsupported,
            label: "gpu".into(),
        },
    };
    let mut history = ResourceHistory::new(2);
    history.push_processes(1, &[process]);
    assert_eq!(history.process_count(), 0);
}
