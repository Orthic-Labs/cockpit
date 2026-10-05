//! Real-APFS fixture checks. Only meaningful in CI after `scripts/fixtures/apfs/setup.sh`;
//! otherwise it skips explicitly. See docs/apfs-fixtures.md.
#![cfg(target_os = "macos")]

use cockpit_core::{ReclaimState, ScanOptions, VolumeDelta, scan_paths};
use std::path::{Path, PathBuf};
use std::process::Command;

const MIB: u64 = 1024 * 1024;

fn used_bytes(path: &Path) -> Option<u64> {
    let out = Command::new("df")
        .args(["-k", "-P"])
        .arg(path)
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let kb: u64 = text
        .lines()
        .nth(1)?
        .split_whitespace()
        .nth(2)?
        .parse()
        .ok()?;
    kb.checked_mul(1024)
}

fn entry<'a>(report: &'a cockpit_core::ScanReport, suffix: &str) -> &'a cockpit_core::ScannedEntry {
    report
        .entries
        .iter()
        .find(|e| e.path.ends_with(suffix))
        .unwrap_or_else(|| panic!("missing entry {suffix}"))
}

#[test]
fn apfs_fixture_accounting_is_conservative() {
    let Some(root) = std::env::var_os("COCKPIT_APFS_FIXTURE_ROOT").map(PathBuf::from) else {
        eprintln!("SKIP apfs_fixture: COCKPIT_APFS_FIXTURE_ROOT not set (CI-only harness)");
        return;
    };
    assert!(root.is_dir(), "fixture root is not a directory");

    let baseline: Option<u64> = std::env::var("COCKPIT_APFS_FIXTURE_BASELINE_USED")
        .ok()
        .and_then(|v| v.parse().ok());
    let after = used_bytes(&root);
    let (volume, _) = cockpit_core::platform::volume_identity_for_path(&root)
        .expect("volume identity for fixture root");
    let options = ScanOptions {
        volume_deltas: vec![VolumeDelta::new(volume, baseline, after)],
        ..ScanOptions::default()
    };

    let report = scan_paths(std::slice::from_ref(&root), &options);

    // Hard links: three names, one inode, allocation attributed exactly once.
    let hard: Vec<_> = report
        .entries
        .iter()
        .filter(|e| e.path.starts_with(root.join("hard")) && e.path.extension().is_some())
        .collect();
    assert_eq!(hard.len(), 3, "expected three hard-link names");
    let owners = hard
        .iter()
        .filter(|e| e.attributed_allocation_bytes > 0)
        .count();
    assert_eq!(owners, 1, "hard-linked inode must be attributed once");
    let hard_total: u64 = hard.iter().map(|e| e.attributed_allocation_bytes).sum();
    assert!(
        (4 * MIB..8 * MIB).contains(&hard_total),
        "hard total {hard_total}"
    );

    // Sparse: allocation well below logical size.
    let sparse = entry(&report, "sparse.bin");
    assert_eq!(sparse.logical_bytes, 256 * MIB);
    assert!(
        sparse.attributed_allocation_bytes < sparse.logical_bytes,
        "sparse allocation {} not below logical {}",
        sparse.attributed_allocation_bytes,
        sparse.logical_bytes
    );

    // Clones: shared extents are never claimed as reclaim.
    for name in ["orig.bin", "copy1.bin", "copy2.bin"] {
        let e = entry(&report, name);
        let reclaim = e
            .reclaim
            .as_ref()
            .expect("file entries carry a reclaim estimate");
        assert_eq!(reclaim.lower_bytes, 0, "{name} claims reclaim");
        assert!(
            matches!(reclaim.state, ReclaimState::Unknown),
            "{name} state"
        );
    }
    assert_eq!(
        report.accounting.reclaim.lower_bytes, 0,
        "no reclaim may be claimed"
    );
    assert!(matches!(
        report.accounting.reclaim.state,
        Some(ReclaimState::Unknown)
    ));

    // chmod 000 directory: report must be incomplete, never silently complete.
    assert!(
        report.accounting.incomplete,
        "inaccessible dir must make report incomplete"
    );
    assert!(
        report.accounting.reclaim.upper_bytes.is_none(),
        "incomplete report must not offer a full-selection upper bound"
    );
    let locked = root.join("locked");
    let mentioned = report
        .inspection_errors
        .iter()
        .any(|e| e.path.starts_with(&locked))
        || report
            .incomplete_reasons
            .iter()
            .any(|r| r.contains(locked.to_string_lossy().as_ref()));
    assert!(mentioned, "incompleteness not attributed to locked dir");

    // Volume used-delta vs attributed bytes (conservative: APFS container accounting is noisy).
    match (baseline, after) {
        (Some(before), Some(now)) => {
            let delta = now as i128 - before as i128;
            let attributed = report.accounting.attributed_allocation_bytes as i128;
            // Unique data: hard-linked 4 MiB + one 8 MiB clone extent set + ~1 MiB sparse data.
            let unique_floor = (13 * MIB) as i128;
            let slack = (64 * MIB) as i128; // metadata, snapshot retention, container noise
            assert!(
                delta <= attributed + slack,
                "volume grew {delta} > attributed {attributed} + slack"
            );
            assert!(
                delta >= unique_floor / 2,
                "volume grew {delta}, below half of unique data {unique_floor}"
            );
        }
        _ => eprintln!("SKIP volume delta: baseline or current used bytes unavailable"),
    }
}
