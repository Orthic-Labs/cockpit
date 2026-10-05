//! Human-readable rendering of the CLI's JSON values.
//!
//! Pure presentation: input is the exact `serde_json::Value` emitted for
//! `--json`; nothing here changes that machine output. Every externally
//! sourced string is escaped for terminal control characters, unavailable
//! metrics are never shown as zero, and row lists are bounded.

use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Status,
    Scan,
    Findings,
    Explain,
    History,
    Procs,
    Usage,
    /// A read-only worker IPC `Response` serialized as JSON.
    Worker,
}

#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub max_rows: usize,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { max_rows: 20 }
    }
}

/// Render `value` for `view` as human-readable text ending in a newline.
pub fn render(view: View, value: &Value, options: &RenderOptions) -> String {
    let mut out = String::new();
    match view {
        View::Status => status(&mut out, value, options),
        View::Scan => scan(&mut out, value, options),
        View::Findings => findings(&mut out, value, options),
        View::Explain => explain(&mut out, value, options),
        View::History => history(&mut out, value, options),
        View::Procs => procs(&mut out, value, options),
        View::Usage => usage(&mut out, value),
        View::Worker => worker(&mut out, value, options),
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------- helpers

/// Escape C0, DEL, C1 and bidi-override characters so untrusted text cannot
/// drive the terminal or visually reorder output.
fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let n = c as u32;
        if n < 0x20 || (0x7f..=0x9f).contains(&n) {
            let _ = write!(out, "\\x{n:02x}");
        } else if (0x202a..=0x202e).contains(&n)
            || (0x2066..=0x2069).contains(&n)
            || n == 0x200e
            || n == 0x200f
            || n == 0x2028
            || n == 0x2029
        {
            let _ = write!(out, "\\u{{{n:04x}}}");
        } else {
            out.push(c);
        }
    }
    out
}

fn text(v: &Value) -> String {
    match v {
        Value::Null => "unknown".into(),
        Value::String(s) => esc(s),
        other => esc(&other.to_string()),
    }
}

fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Parse a possibly-wide integer: JSON number (i64/u64) or decimal string.
fn as_i128(v: &Value) -> Option<i128> {
    if let Some(n) = v.as_i64() {
        return Some(i128::from(n));
    }
    if let Some(n) = v.as_u64() {
        return Some(i128::from(n));
    }
    match v {
        Value::String(s) => s.trim().parse::<i128>().ok(),
        Value::Number(n) => n.to_string().parse::<i128>().ok(),
        _ => None,
    }
}

fn signed_bytes(n: i128) -> String {
    let magnitude = n.unsigned_abs();
    let clamped = u64::try_from(magnitude).unwrap_or(u64::MAX);
    let shown = if magnitude > u128::from(u64::MAX) {
        format!("over {}", bytes(clamped))
    } else {
        bytes(clamped)
    };
    if n < 0 {
        format!("-{shown}")
    } else {
        format!("+{shown}")
    }
}

fn unavailable(reason: &str) -> String {
    if reason.is_empty() {
        "unavailable".into()
    } else {
        format!("unavailable ({})", esc(reason))
    }
}

/// Reason text for a metric-shaped object: label, then capability/state.
fn reason_of(m: &Value) -> String {
    let mut parts = Vec::new();
    for key in ["label", "capability", "state", "reason"] {
        if let Some(s) = m[key].as_str()
            && !s.is_empty()
            && !parts.contains(&s)
        {
            parts.push(s);
        }
    }
    parts.join("; ")
}

#[derive(Clone, Copy)]
enum Kind {
    Bytes,
    Percent,
    Text,
}

fn fmt_value(v: &Value, kind: Kind) -> Option<String> {
    match (kind, v) {
        (_, Value::Null) => None,
        (Kind::Bytes, v) => v.as_u64().map(bytes),
        (Kind::Percent, v) => v.as_f64().map(|p| format!("{p:.1}%")),
        (Kind::Text, v) => Some(text(v)),
    }
}

/// Format a `{value, capability, label}` metric (or a bare value).
/// A missing value is always "unavailable (...)", never zero.
fn metric(m: &Value, kind: Kind) -> String {
    if m.is_object() {
        match fmt_value(&m["value"], kind) {
            Some(s) => {
                let label = m["label"].as_str().unwrap_or("");
                if label.is_empty() {
                    s
                } else {
                    format!("{s} [{}]", esc(label))
                }
            }
            None => unavailable(&reason_of(m)),
        }
    } else {
        fmt_value(m, kind).unwrap_or_else(|| unavailable("not reported"))
    }
}

fn opt_bytes(v: &Value, reason: &str) -> String {
    v.as_u64().map(bytes).unwrap_or_else(|| unavailable(reason))
}

fn items(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}

/// Write at most `max` rows through `row`, then an explicit truncation notice.
fn bounded<T>(out: &mut String, rows: &[T], max: usize, mut row: impl FnMut(&mut String, &T)) {
    for r in rows.iter().take(max) {
        row(out, r);
    }
    if rows.len() > max {
        let _ = writeln!(out, "  … {} more not shown", rows.len() - max);
    }
}

fn diagnostics(out: &mut String, v: &Value, o: &RenderOptions) {
    let skipped = items(&v["history_diagnostics"]);
    if !skipped.is_empty() {
        let _ = writeln!(out, "History files skipped ({})", skipped.len());
        bounded(out, skipped, o.max_rows, |out, d| {
            let _ = writeln!(out, "  {}: {}", text(&d["file"]), text(&d["reason"]));
        });
    }
    let notes = items(&v["capability_notes"]);
    if !notes.is_empty() {
        let _ = writeln!(out, "Store capability notes ({})", notes.len());
        bounded(out, notes, o.max_rows, |out, n| {
            let _ = writeln!(out, "  {}", text(n));
        });
    }
}

fn strings(v: &Value) -> Vec<String> {
    items(v).iter().map(text).collect()
}

// ------------------------------------------------------------------ views

fn status(out: &mut String, v: &Value, o: &RenderOptions) {
    let s = &v["system"];
    out.push_str("System status\n");
    let _ = writeln!(
        out,
        "  CPU:             {}",
        metric(&s["cpu_usage_percent"], Kind::Percent)
    );
    let _ = writeln!(
        out,
        "  Memory used:     {}",
        metric(&s["memory_used_bytes"], Kind::Bytes)
    );
    let _ = writeln!(
        out,
        "  Memory total:    {}",
        metric(&s["memory_total_bytes"], Kind::Bytes)
    );
    let _ = writeln!(
        out,
        "  Memory pressure: {}",
        metric(&s["memory_pressure"], Kind::Text)
    );
    let _ = writeln!(
        out,
        "  Swap used:       {}",
        metric(&s["swap_used_bytes"], Kind::Bytes)
    );
    let _ = writeln!(
        out,
        "  Swap total:      {}",
        metric(&s["swap_total_bytes"], Kind::Bytes)
    );
    let _ = writeln!(
        out,
        "  Purgeable:       {}",
        match v["purgeable_bytes"].as_u64() {
            Some(n) => bytes(n),
            None => unavailable("purgeable space not reported"),
        }
    );
    let _ = writeln!(
        out,
        "  Snapshots:       {}",
        if v["snapshots"].is_object() {
            metric(&v["snapshots"], Kind::Text)
        } else {
            unavailable("not reported")
        }
    );
    let disks = items(&s["disks"]);
    let _ = writeln!(out, "Disks ({})", disks.len());
    bounded(out, disks, o.max_rows, |out, d| {
        let cap = d["capability"].as_str().unwrap_or("");
        let _ = writeln!(
            out,
            "  {}  total {}  available {}{}",
            text(&d["mount_point"]),
            opt_bytes(
                &d["total_bytes"],
                if cap.is_empty() { "not reported" } else { cap }
            ),
            opt_bytes(
                &d["available_bytes"],
                if cap.is_empty() { "not reported" } else { cap }
            ),
            if d["removable"].as_bool() == Some(true) {
                "  removable"
            } else {
                ""
            },
        );
    });
}

fn finding_line(out: &mut String, f: &Value) {
    let _ = writeln!(
        out,
        "  {}  rule {}  {}",
        text(&f["id"]),
        text(&f["rule_id"]),
        text(&f["path"]),
    );
    let _ = writeln!(
        out,
        "      liveness {}  risk {}  route {}  evidence sufficient {}",
        text(&f["liveness"]),
        text(&f["risk"]),
        text(&f["route"]),
        match f["eligible"].as_bool() {
            Some(b) => b.to_string(),
            None => "unknown".into(),
        },
    );
    let _ = writeln!(
        out,
        "      logical {}  attributed {}",
        opt_bytes(&f["logical_bytes"], "not measured"),
        opt_bytes(
            &f["attributed_bytes"],
            "not measured or coverage incomplete"
        ),
    );
    let reclaim = match (
        f["deletion_estimate_lower"].as_u64(),
        f["deletion_estimate_upper"].as_u64(),
    ) {
        (Some(l), Some(u)) => format!("{} to {} (estimate)", bytes(l), bytes(u)),
        (Some(l), None) => format!("at least {}, upper bound unknown (estimate)", bytes(l)),
        (None, _) => "unknown (no estimate)".into(),
    };
    let _ = writeln!(out, "      reclaim {reclaim}");
    out.push_str("      actions: disabled until feasibility gates pass\n");
    for reason in strings(&f["reasons"]) {
        let _ = writeln!(out, "      - {reason}");
    }
}

fn scan(out: &mut String, v: &Value, o: &RenderOptions) {
    let snap = &v["snapshot"];
    let report = &snap["report"];
    let acct = &report["accounting"];
    out.push_str("Scan result\n");
    let _ = writeln!(out, "  Snapshot: {}", text(&snap["id"]));
    match v["saved_to"].as_str() {
        Some(p) => {
            let _ = writeln!(out, "  Saved to: {}", esc(p));
        }
        None => out.push_str("  Saved: no (use --save to keep local metadata history)\n"),
    }
    let roots = strings(&report["roots"]);
    let _ = writeln!(
        out,
        "  Roots: {}",
        if roots.is_empty() {
            "none reported".into()
        } else {
            roots.join(", ")
        }
    );
    let entries = items(&report["entries"]);
    let _ = writeln!(out, "  Entries scanned: {}", entries.len());

    let incomplete = acct["incomplete"].as_bool();
    let note = if incomplete == Some(true) {
        " (lower bound; coverage incomplete)"
    } else {
        ""
    };
    let _ = writeln!(
        out,
        "  Logical size:         {}{note}",
        opt_bytes(&acct["logical_bytes"], "not reported")
    );
    let _ = writeln!(
        out,
        "  Attributed allocation: {}{note}",
        opt_bytes(&acct["attributed_allocation_bytes"], "not reported")
    );
    // Attribution is not reclaim: reclaim stays unknown unless bounded.
    let reclaim = &acct["reclaim"];
    if reclaim["state"].as_str() == Some("Bounded") {
        let upper = reclaim["upper_bytes"].as_u64();
        let _ = writeln!(
            out,
            "  Reclaimable (estimate): {} to {}",
            opt_bytes(&reclaim["lower_bytes"], "not reported"),
            upper.map(bytes).unwrap_or_else(|| "unknown".into()),
        );
    } else {
        out.push_str("  Reclaimable: unknown (attribution is not reclaim; nothing is estimated as freeable)\n");
    }
    for r in strings(&reclaim["reasons"]) {
        let _ = writeln!(out, "    - {r}");
    }

    match incomplete {
        Some(true) => out.push_str("  Coverage: INCOMPLETE\n"),
        Some(false) => out.push_str("  Coverage: complete\n"),
        None => out.push_str("  Coverage: unknown\n"),
    }
    let reasons = items(&report["incomplete_reasons"]);
    bounded(out, reasons, o.max_rows, |out, r| {
        let _ = writeln!(out, "    - {}", text(r));
    });
    let errors = items(&report["inspection_errors"]);
    if !errors.is_empty() {
        let _ = writeln!(out, "  Inspection errors ({})", errors.len());
        bounded(out, errors, o.max_rows, |out, e| {
            let _ = writeln!(
                out,
                "    {} [{}]: {}",
                text(&e["path"]),
                text(&e["operation"]),
                text(&e["message"])
            );
        });
    }
    let links = items(&report["skipped_links"]);
    if !links.is_empty() {
        let _ = writeln!(out, "  Skipped links ({})", links.len());
        bounded(out, links, o.max_rows, |out, l| {
            let _ = writeln!(out, "    {}: {}", text(&l["path"]), text(&l["reason"]));
        });
    }

    // Use scanner subtree totals; aggregate immediate parents only for legacy reports.
    let mut folders: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for f in items(&report["folders"]) {
        if let Some(path) = f["path"].as_str() {
            folders.insert(
                path.to_string(),
                (
                    f["logical_bytes"].as_u64().unwrap_or(0),
                    f["attributed_allocation_bytes"].as_u64().unwrap_or(0),
                ),
            );
        }
    }
    if folders.is_empty() {
        for e in entries {
            let Some(path) = e["path"].as_str() else {
                continue;
            };
            let parent = Path::new(path)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| path.to_string());
            let slot = folders.entry(parent).or_default();
            slot.0 = slot
                .0
                .saturating_add(e["logical_bytes"].as_u64().unwrap_or(0));
            slot.1 = slot
                .1
                .saturating_add(e["attributed_allocation_bytes"].as_u64().unwrap_or(0));
        }
    }
    let mut ranked: Vec<_> = folders.into_iter().collect();
    ranked.sort_by(|a, b| b.1.1.cmp(&a.1.1).then_with(|| a.0.cmp(&b.0)));
    let _ = writeln!(
        out,
        "Largest folders by attributed allocation ({} total)",
        ranked.len()
    );
    if ranked.is_empty() {
        out.push_str("  none\n");
    }
    bounded(out, &ranked, o.max_rows, |out, (path, (logical, alloc))| {
        let _ = writeln!(
            out,
            "  {:>10}  logical {:>10}  {}",
            bytes(*alloc),
            bytes(*logical),
            esc(path)
        );
    });

    let found = items(&snap["findings"]);
    let _ = writeln!(out, "Findings ({}, report only)", found.len());
    bounded(out, found, o.max_rows, finding_line);
}

fn findings(out: &mut String, v: &Value, o: &RenderOptions) {
    let _ = writeln!(
        out,
        "Findings (mode {}, snapshot {})",
        text(&v["mode"]),
        if v["snapshot_id"].is_null() {
            "none saved".into()
        } else {
            text(&v["snapshot_id"])
        }
    );
    let found = items(&v["findings"]);
    let _ = writeln!(out, "{} finding(s)", found.len());
    bounded(out, found, o.max_rows, finding_line);
    let ex = items(&v["explanations"]);
    let _ = writeln!(out, "Explanation rules ({})", ex.len());
    bounded(out, ex, o.max_rows, |out, r| {
        let _ = writeln!(out, "  {}  {}", text(&r["id"]), text(&r["name"]));
    });
    diagnostics(out, v, o);
}

fn rule_block(out: &mut String, r: &Value) {
    let _ = writeln!(
        out,
        "Rule {}  {} (v{})",
        text(&r["id"]),
        text(&r["name"]),
        text(&r["rule_version"])
    );
    let _ = writeln!(
        out,
        "  risk {}  route {}  report only {}",
        text(&r["risk"]),
        text(&r["route"]),
        match r["report_only"].as_bool() {
            Some(b) => b.to_string(),
            None => "unknown".into(),
        }
    );
    if let Some(d) = r["route_detail"].as_str() {
        let _ = writeln!(out, "  route detail: {}", esc(d));
    }
    if let Some(d) = r["explanation"].as_str() {
        let _ = writeln!(out, "  {}", esc(d));
    }
    let patterns = strings(&r["path_patterns"]);
    if !patterns.is_empty() {
        let _ = writeln!(out, "  paths: {}", patterns.join(", "));
    }
    if let Some(d) = r["age_threshold_days"].as_u64() {
        let _ = writeln!(out, "  age threshold: {d} days");
    }
    for (label, key) in [
        ("liveness evidence", "liveness"),
        ("eligibility evidence", "eligibility"),
    ] {
        let list = strings(&r[key]);
        if !list.is_empty() {
            let _ = writeln!(out, "  {label}: {}", list.join(", "));
        }
    }
}

fn explain(out: &mut String, v: &Value, o: &RenderOptions) {
    if v.get("finding").is_some() {
        out.push_str("Finding\n");
        finding_line(out, &v["finding"]);
        if v["rule"].is_object() {
            rule_block(out, &v["rule"]);
        } else {
            out.push_str("Rule: unavailable (rule not present in current rule pack)\n");
        }
    } else {
        rule_block(out, v);
    }
    diagnostics(out, v, o);
}

fn history(out: &mut String, v: &Value, o: &RenderOptions) {
    let rows = items(&v["history"]);
    let _ = writeln!(out, "Snapshot history ({} snapshot(s))", rows.len());
    if rows.is_empty() {
        out.push_str("  none saved\n");
    }
    bounded(out, rows, o.max_rows, |out, r| {
        let _ = writeln!(
            out,
            "  {}  created_at {} (unix epoch)",
            text(&r["id"]),
            text(&r["created_at"])
        );
        let roots = strings(&r["roots"]);
        let _ = writeln!(
            out,
            "      roots: {}",
            if roots.is_empty() {
                "none".into()
            } else {
                roots.join(", ")
            }
        );
        let a = &r["accounting"];
        let _ = writeln!(
            out,
            "      attributed {}  logical {}  coverage {}  findings {}",
            opt_bytes(&a["attributed_allocation_bytes"], "not reported"),
            opt_bytes(&a["logical_bytes"], "not reported"),
            match a["incomplete"].as_bool() {
                Some(true) => "incomplete",
                Some(false) => "complete",
                None => "unknown",
            },
            text(&r["findings_count"]),
        );
        let growth = match as_i128(&r["attributed_growth_bytes"]) {
            Some(g) => signed_bytes(g),
            None => {
                let why = strings(&r["comparison"]["reasons"]);
                if r["comparison"].is_null() {
                    unavailable("first snapshot, nothing to compare")
                } else if why.is_empty() {
                    unavailable("not comparable")
                } else {
                    format!("unavailable (not comparable: {})", why.join("; "))
                }
            }
        };
        let _ = writeln!(out, "      attributed growth since previous: {growth}");
    });
    diagnostics(out, v, o);
}

fn procs(out: &mut String, v: &Value, o: &RenderOptions) {
    let list = items(&v["processes"]);
    let _ = writeln!(out, "Processes ({}, individual, not grouped)", list.len());
    let _ = writeln!(out, "  GPU: {}", unavailable(&reason_of(&v["gpu"])));
    if v["actions_enabled"].as_bool() != Some(true) {
        out.push_str("  Actions: disabled (read-only)\n");
    }
    bounded(out, list, o.max_rows, |out, p| {
        let id = &p["identity"];
        let pid = if id["pid"].is_null() {
            "unknown".to_string()
        } else {
            text(&id["pid"])
        };
        let start = if id["start_time"].is_null() {
            unavailable("start time not reported")
        } else {
            format!("{} (unix s)", text(&id["start_time"]))
        };
        let parent = p["parent_pid"]
            .as_u64()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "unknown".into());
        let cpu = metric(&p["cpu_usage_percent"], Kind::Percent);
        let _ = writeln!(
            out,
            "  PID {pid}  start {start}  {}  parent {parent}",
            text(&p["name"]),
        );
        let _ = writeln!(
            out,
            "      CPU {cpu}  RSS {}",
            metric(&p["memory"], Kind::Bytes)
        );
        if p["gpu_usage_percent"]["value"].as_f64().is_some() {
            let _ = writeln!(
                out,
                "      GPU {}",
                metric(&p["gpu_usage_percent"], Kind::Percent)
            );
        }
    });
    let groups = items(&v["process_groups"]["groups"]);
    if v["process_groups"].is_object() {
        let _ = writeln!(out, "Process groups ({})", groups.len());
        bounded(out, groups, o.max_rows, |out, g| {
            let _ = writeln!(out, "  {}", text(g));
        });
        for n in strings(&v["process_groups"]["notes"]) {
            let _ = writeln!(out, "  note: {n}");
        }
    }
    out.push_str("  (CPU % is measured over a sampling interval; PID alone is not a stable identity, start time is shown with it)\n");
}

fn usage(out: &mut String, v: &Value) {
    out.push_str("Provider usage\n");
    for (name, key) in [("Claude", "claude"), ("Codex", "codex")] {
        let m = &v[key];
        let shown = if m["value"].is_null() || !m.is_object() {
            unavailable(&reason_of(m))
        } else {
            let mut s = text(&m["value"]);
            if let Some(src) = m["source"].as_str() {
                let _ = write!(s, " (source {})", esc(src));
            }
            if let Some(at) = m["observed_at"].as_str() {
                let _ = write!(s, " observed {}", esc(at));
            }
            s
        };
        let _ = writeln!(out, "  {name}: {shown}");
    }
    if let Some(r) = v["reason"].as_str() {
        let _ = writeln!(out, "  {}", esc(r));
    }
}

/// Render a worker `Response` (`{version,id,status,data,truncated}` or
/// `{version,id,status:"error",error}`). Omitted counts are never hidden.
fn worker(out: &mut String, v: &Value, o: &RenderOptions) {
    let _ = writeln!(
        out,
        "Worker response (protocol {}, request {})",
        text(&v["version"]),
        text(&v["id"])
    );
    match v["status"].as_str() {
        Some("ok") => {
            let truncated = v["truncated"].as_bool();
            let _ = writeln!(
                out,
                "  status: ok  truncated: {}",
                match truncated {
                    Some(true) => "yes (some lists omit entries; see omitted counts)",
                    Some(false) => "no",
                    None => "unknown",
                }
            );
            let data = &v["data"];
            let mut omitted = Vec::new();
            collect_omitted(data, "data", &mut omitted);
            for (path, n) in omitted.iter().take(o.max_rows) {
                let _ = writeln!(out, "  omitted {n} at {}", esc(path));
            }
            if omitted.len() > o.max_rows {
                let _ = writeln!(out, "  … {} more not shown", omitted.len() - o.max_rows);
            }
            if data["system"].is_object() {
                status(out, data, o);
            } else if data["processes"].is_array() {
                procs(out, data, o);
            } else if data["snapshot"].is_object() {
                scan(out, data, o);
            } else {
                let _ = writeln!(out, "  data: {}", text(data));
            }
        }
        Some("error") => {
            let _ = writeln!(
                out,
                "  status: error  code {}  {}",
                text(&v["error"]["code"]),
                text(&v["error"]["message"])
            );
        }
        _ => out.push_str("  status: unknown (unrecognized response)\n"),
    }
}

/// Find numeric fields named `omitted*` greater than zero anywhere in `v`.
fn collect_omitted(v: &Value, path: &str, found: &mut Vec<(String, u64)>) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                let here = format!("{path}.{k}");
                if k.starts_with("omitted")
                    && let Some(n) = child.as_u64()
                {
                    if n > 0 {
                        found.push((here, n));
                    }
                } else {
                    collect_omitted(child, &here, found);
                }
            }
        }
        Value::Array(list) => {
            for (i, child) in list.iter().enumerate().take(64) {
                collect_omitted(child, &format!("{path}[{i}]"), found);
            }
        }
        _ => {}
    }
}
