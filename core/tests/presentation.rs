use cockpit_core::presentation::{RenderOptions, View, render};
use serde_json::{Value, json};

fn opts(max_rows: usize) -> RenderOptions {
    RenderOptions { max_rows }
}

fn entry(path: &str, logical: u64, alloc: u64) -> Value {
    json!({"path": path, "logical_bytes": logical, "attributed_allocation_bytes": alloc})
}

fn scan_value(entries: Vec<Value>, incomplete: bool) -> Value {
    let reasons: Vec<&str> = if incomplete {
        vec!["permission denied under /data/private"]
    } else {
        vec![]
    };
    json!({"snapshot": {
        "id": "snap-1",
        "report": {
            "roots": ["/data"],
            "entries": entries,
            "accounting": {
                "logical_bytes": 3 * 1024 * 1024,
                "attributed_allocation_bytes": 2 * 1024 * 1024 * 1024u64,
                "reclaim": {"lower_bytes": 0, "upper_bytes": null, "state": null, "reasons": ["no cleanup rule applied"]},
                "incomplete": incomplete
            },
            "inspection_errors": [],
            "skipped_links": [],
            "incomplete_reasons": reasons
        },
        "findings": []
    }, "saved_to": null})
}

#[test]
fn default_options_and_trailing_newline() {
    assert_eq!(RenderOptions::default().max_rows, 20);
    for view in [
        View::Status,
        View::Scan,
        View::Findings,
        View::Explain,
        View::History,
        View::Procs,
        View::Usage,
    ] {
        let out = render(view, &Value::Null, &RenderOptions::default());
        assert!(out.ends_with('\n'));
        let out = render(
            view,
            &json!([1, "x", {"a": null}]),
            &RenderOptions::default(),
        );
        assert!(out.ends_with('\n'));
    }
}

#[test]
fn scan_aggregates_folders_and_escapes_paths() {
    let value = scan_value(
        vec![
            entry("/data/big/a", 10, 5 * 1024 * 1024),
            entry("/data/big/b", 10, 5 * 1024 * 1024),
            entry("/data/small/c", 1, 1024),
            entry("/data/evil\u{1b}[2Jdir/f", 1, 512),
        ],
        false,
    );
    let out = render(View::Scan, &value, &opts(20));
    assert!(out.contains("/data/big"));
    assert!(out.contains("10.0 MiB"), "{out}");
    assert!(out.contains("2.0 GiB"));
    assert!(out.contains("evil\\x1b[2Jdir"), "{out}");
    assert!(!out.contains('\u{1b}'));
    assert!(out.contains("Coverage: complete"));
    // Largest folder is listed before the smaller one.
    assert!(out.find("/data/big").unwrap() < out.find("/data/small").unwrap());
}

#[test]
fn scan_keeps_reclaim_unknown_and_reports_incomplete_coverage() {
    let out = render(
        View::Scan,
        &scan_value(vec![entry("/data/a/x", 1, 1)], true),
        &opts(20),
    );
    assert!(out.contains("INCOMPLETE"));
    assert!(out.contains("permission denied under /data/private"));
    assert!(out.contains("lower bound"));
    assert!(out.contains("Reclaimable: unknown"));
    assert!(out.contains("Attributed allocation"));
}

#[test]
fn scan_bounds_rows_with_notice() {
    let entries: Vec<_> = (0..7)
        .map(|i| entry(&format!("/data/d{i}/f"), 1, 100 + i))
        .collect();
    let out = render(View::Scan, &scan_value(entries, false), &opts(3));
    assert!(out.contains("… 4 more not shown"), "{out}");
}

#[test]
fn status_shows_unavailable_not_zero() {
    let value = json!({
        "schema_version": 1,
        "system": {
            "cpu_usage_percent": {"value": 12.34, "capability": "Available", "label": "OS sample"},
            "memory_used_bytes": {"value": 3 * 1024 * 1024 * 1024u64, "capability": "Available", "label": "OS-reported system memory"},
            "memory_total_bytes": {"value": 16u64 * 1024 * 1024 * 1024, "capability": "Available", "label": "total"},
            "memory_pressure": {"value": null, "capability": "Unavailable", "label": "pressure level not exposed"},
            "swap_used_bytes": {"value": 2048, "capability": "Available", "label": "swap"},
            "swap_total_bytes": {"value": null, "capability": "PermissionDenied", "label": "swap total"},
            "disks": [
                {"mount_point": "/", "total_bytes": 1000000, "available_bytes": null, "removable": false, "capability": "Available"},
                {"mount_point": "/a", "total_bytes": null, "available_bytes": null, "removable": true, "capability": "Unavailable"},
                {"mount_point": "/b", "total_bytes": 1, "available_bytes": 1, "removable": false, "capability": "Available"}
            ]
        },
        "snapshots": {"capability": "unavailable", "reason": "snapshot provider pending"},
        "purgeable_bytes": null
    });
    let out = render(View::Status, &value, &opts(2));
    assert!(out.contains("12.3%"));
    assert!(out.contains("3.0 GiB"));
    assert!(out.contains("16.0 GiB"));
    assert!(out.contains("2.0 KiB"));
    assert!(
        out.contains("unavailable (pressure level not exposed; Unavailable)"),
        "{out}"
    );
    assert!(out.contains("Purgeable:       unavailable"));
    assert!(out.contains("snapshot provider pending"));
    assert!(out.contains("… 1 more not shown"));
}

#[test]
fn procs_show_pid_start_time_and_metric_label() {
    let value = json!({
        "processes": [
            {"identity": {"pid": 4242, "start_time": 1700000123u64}, "name": "bad\u{7}name\u{9b}",
             "parent_pid": 1, "cpu_usage_percent": 3.5,
             "memory": {"value": 5 * 1024 * 1024, "capability": "Available", "label": "resident memory (RSS); physical footprint unavailable"},
             "gpu_usage_percent": {"value": null, "capability": "Unsupported", "label": "no per-process GPU"}},
            {"identity": {"pid": 7, "start_time": 1700000999u64}, "name": "nomem", "parent_pid": null,
             "cpu_usage_percent": 0.0,
             "memory": {"value": null, "capability": "PermissionDenied", "label": "RSS denied"},
             "gpu_usage_percent": {"value": null, "capability": "Unsupported", "label": "x"}}
        ],
        "grouping": "individual_processes",
        "gpu": {"capability": "unavailable"},
        "actions_enabled": false
    });
    let out = render(View::Procs, &value, &opts(20));
    assert!(out.contains("PID 4242"));
    assert!(out.contains("1700000123"));
    assert!(
        out.contains("RSS 5.0 MiB [resident memory (RSS); physical footprint unavailable]"),
        "{out}"
    );
    assert!(out.contains("CPU 3.5%"));
    assert!(out.contains("bad\\x07name\\x9b"));
    assert!(
        out.contains("unavailable (RSS denied; PermissionDenied)"),
        "{out}"
    );
    assert!(out.contains("GPU: unavailable"));
    assert!(!out.contains('\u{7}'));
}

#[test]
fn findings_explain_history_usage_render() {
    let finding = json!({
        "id": "f1", "rule_id": "r1", "rule_version": 1, "path": "/cache/\u{1b}x",
        "liveness": "unknown", "risk": "review", "route": "native_tool", "eligible": false,
        "reasons": ["liveness unknown"], "logical_bytes": 2048, "attributed_bytes": null,
        "unique_bytes": null, "deletion_estimate_lower": null, "deletion_estimate_upper": null
    });
    let out = render(
        View::Findings,
        &json!({"snapshot_id": "s", "findings": [finding.clone(), finding.clone()], "explanations": [], "mode": "report_only"}),
        &opts(1),
    );
    assert!(out.contains("/cache/\\x1bx"));
    assert!(out.contains("2.0 KiB"));
    assert!(out.contains("attributed unavailable"));
    assert!(out.contains("reclaim unknown"));
    assert!(out.contains("… 1 more not shown"));

    let rule = json!({"id": "r1", "name": "Rule\u{1b}One", "rule_version": 2, "risk": "explanation", "explanation": "swap is managed by the OS"});
    let bare = render(View::Explain, &rule, &opts(5));
    assert!(bare.contains("Rule\\x1bOne") && bare.contains("swap is managed"));
    let wrapped = render(
        View::Explain,
        &json!({"finding": finding, "rule": null}),
        &opts(5),
    );
    assert!(wrapped.contains("f1") && wrapped.contains("Rule: unavailable"));

    let hist = render(
        View::History,
        &json!({"history": [
            {"id": "a", "created_at": 1, "roots": ["/r"], "accounting": {"attributed_allocation_bytes": 1048576, "logical_bytes": 1, "incomplete": false}, "attributed_growth_bytes": null, "comparison": null, "findings_count": 0},
            {"id": "b", "created_at": 2, "roots": ["/r"], "accounting": {}, "attributed_growth_bytes": -2048, "comparison": {"comparable": true}, "findings_count": 1}
        ]}),
        &opts(5),
    );
    assert!(hist.contains("1.0 MiB"));
    assert!(hist.contains("-2.0 KiB"));
    assert!(hist.contains("unavailable (first snapshot"));

    let usage = render(
        View::Usage,
        &json!({"claude": {"value": null, "state": "unavailable", "source": null, "observed_at": null},
                "codex": {"value": null, "state": "unavailable"}, "reason": "integration pending"}),
        &opts(5),
    );
    assert!(usage.contains("Claude: unavailable (unavailable)"));
    assert!(usage.contains("integration pending"));
}

#[test]
fn scan_uses_provider_subtree_folder_totals() {
    let mut value = scan_value(vec![entry("/data/tree/deep/a", 1, 1024)], false);
    value["snapshot"]["report"]["folders"] = json!([
        {"path":"/data/tree","logical_bytes":2048,"attributed_allocation_bytes":1048576},
        {"path":"/data/tree/deep","logical_bytes":1,"attributed_allocation_bytes":1024}
    ]);
    let out = render(View::Scan, &value, &opts(20));
    assert!(out.contains("1.0 MiB  logical"), "{out}");
    assert!(out.contains("(2 total)"), "{out}");
}
