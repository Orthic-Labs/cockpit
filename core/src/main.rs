use cockpit_core::presentation::{RenderOptions, View, render};
use cockpit_core::{ScanOptions, rules, scan_paths, store};
use serde_json::{Value, json};
use std::{path::PathBuf, process::ExitCode};

/// CLI failure: optional JSON body for stderr and the process exit code.
struct CliError {
    body: Option<Value>,
    exit: u8,
}
impl From<String> for CliError {
    fn from(message: String) -> Self {
        Self {
            body: Some(json!({"error": message})),
            exit: 2,
        }
    }
}
impl From<&str> for CliError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}
impl From<cockpit_core::ipc::IpcError> for CliError {
    fn from(error: cockpit_core::ipc::IpcError) -> Self {
        Self {
            body: Some(json!({"error": error.message, "code": error.code})),
            exit: 2,
        }
    }
}

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(CliError { body, exit }) => {
            if let Some(body) = body {
                eprintln!("{body}");
            }
            ExitCode::from(exit)
        }
    }
}

/// i128 growth as a JSON number when it fits i64/u64, else a decimal string
/// (serde_json without arbitrary precision cannot hold wider numbers).
fn wide(n: i128) -> Value {
    if let Ok(v) = i64::try_from(n) {
        json!(v)
    } else if let Ok(v) = u64::try_from(n) {
        json!(v)
    } else {
        Value::String(n.to_string())
    }
}
fn comparison_value(c: &cockpit_core::history::Comparison) -> Value {
    json!({
        "comparable": c.comparable,
        "attributed_growth_bytes": c.attributed_growth_bytes.map(wide),
        "logical_growth_bytes": c.logical_growth_bytes.map(wide),
        "reasons": c.reasons,
    })
}
fn emit(value: Value, machine: bool, view: View) {
    if machine {
        println!("{value}");
    } else {
        print!("{}", render(view, &value, &RenderOptions::default()));
    }
}
fn run(mut arguments: Vec<String>) -> Result<(), CliError> {
    let machine = arguments.iter().any(|a| a == "--json");
    arguments.retain(|a| a != "--json");
    if arguments.is_empty() || ["help", "--help", "-h"].contains(&arguments[0].as_str()) {
        println!(
            "Cockpit — read-only system inspection\n\nstatus [--json]\nscan <path…> [--max-depth N] [--max-entries N] [--save] [--state-dir PATH] [--json]\nfindings [--rule ID] [--state-dir PATH] [--json]\nexplain <finding-id|rule-id> [--state-dir PATH] [--json]\nhistory [--state-dir PATH] [--json]\nprocs [--sort cpu|ram|gpu] [--groups] [--json]\nworker serve [--endpoint E] [--idle-seconds 1-600]\nworker request status|procs [--groups]|scan <path…> [--max-depth N] [--max-entries N] [--endpoint E] [--json]\nusage [--json]\n\nScans never read file contents. --save opts into local metadata history.\nCleanup, uninstall & process actions await feasibility & safety gates."
        );
        return Ok(());
    }
    let command = arguments.remove(0);
    if command == "worker" {
        return worker(arguments, machine);
    }
    let state_override = take_option(&mut arguments, "--state-dir")?.map(PathBuf::from);
    let directory = || {
        state_override
            .clone()
            .map(Ok)
            .unwrap_or_else(store::default_directory)
            .map_err(|e| e.to_string())
    };
    let pack: rules::RulePack = serde_json::from_str(include_str!("../../rules/initial.json"))
        .map_err(|e| e.to_string())?;
    match command.as_str() {
        "status" => {
            require_empty(&arguments)?;
            emit(
                json!({"schema_version":1,"system":cockpit_core::system_status(),"snapshots":{"capability":"unavailable","reason":"snapshot provider pending"},"purgeable_bytes":null}),
                machine,
                View::Status,
            );
        }
        "scan" => {
            let max_depth = take_number(&mut arguments, "--max-depth", 64)?;
            let max_entries = take_number(&mut arguments, "--max-entries", 100_000)?;
            if max_depth > 128 || max_entries > 1_000_000 || max_entries == 0 {
                return Err("scan limits: depth ≤ 128; entries 1–1000000".into());
            }
            let save = take_flag(&mut arguments, "--save");
            if arguments.is_empty() {
                return Err("scan requires explicit paths".into());
            }
            if arguments.iter().any(|p| p.starts_with('-')) {
                return Err("unknown scan option".into());
            }
            let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
            let paths: Vec<_> = arguments
                .iter()
                .map(|p| {
                    let p = PathBuf::from(p);
                    if p.is_absolute() { p } else { cwd.join(p) }
                })
                .collect();
            let report = scan_paths(
                &paths,
                &ScanOptions {
                    max_depth,
                    max_entries,
                    ..Default::default()
                },
            );
            let mut totals = std::collections::BTreeMap::<PathBuf, (u64, u64)>::new();
            for entry in &report.entries {
                for ancestor in entry.path.ancestors() {
                    let total = totals.entry(ancestor.to_path_buf()).or_default();
                    total.0 = total.0.saturating_add(entry.logical_bytes);
                    total.1 = total.1.saturating_add(entry.attributed_allocation_bytes);
                }
            }
            let chrome_running = cockpit_core::procs()
                .iter()
                .any(|p| p.name.to_ascii_lowercase().contains("chrome"));
            let rows: Vec<_> = report
                .entries
                .iter()
                .map(|entry| {
                    let (logical, allocation) =
                        totals.get(&entry.path).copied().unwrap_or_default();
                    rules::ScanMetadata {
                        path: entry.path.to_string_lossy().into_owned(),
                        volume_id: Some(entry.metadata.volume.id.clone()),
                        volume_mounted: Some(true),
                        path_state: rules::PathState::Present,
                        logical_bytes: Some(logical),
                        attributed_bytes: if report.accounting.incomplete {
                            None
                        } else {
                            Some(allocation)
                        },
                        evidence: rules::ScanEvidence {
                            inspection_complete: Some(
                                !report.accounting.incomplete && entry.metadata.metadata_complete,
                            ),
                            cloud_placeholder: Some(entry.metadata.is_placeholder),
                            chrome_family_running: if chrome_running { Some(true) } else { None },
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                })
                .collect();
            let findings = rules::evaluate_all(&pack.rules, &rows);
            let snapshot = store::Snapshot::new(report, findings);
            let saved_to = if save {
                Some(store::save(&directory()?, &snapshot).map_err(|e| e.to_string())?)
            } else {
                None
            };
            emit(
                json!({"snapshot":snapshot,"saved_to":saved_to}),
                machine,
                View::Scan,
            );
        }
        "findings" => {
            let rule = take_option(&mut arguments, "--rule")?;
            require_empty(&arguments)?;
            let (history, diagnostics, notes) = load_history(&directory()?)?;
            let findings: Vec<_> = history
                .last()
                .map(|s| {
                    s.findings
                        .iter()
                        .filter(|f| rule.as_ref().is_none_or(|r| &f.rule_id == r))
                        .collect()
                })
                .unwrap_or_default();
            let explanations: Vec<_> = pack
                .rules
                .iter()
                .filter(|r| {
                    r.risk == rules::Risk::Explanation && rule.as_ref().is_none_or(|id| &r.id == id)
                })
                .collect();
            emit(
                json!({"snapshot_id":history.last().map(|s| &s.id),"findings":findings,"explanations":explanations,"mode":"report_only","history_diagnostics":diagnostics,"capability_notes":notes}),
                machine,
                View::Findings,
            );
        }
        "explain" => {
            if arguments.len() != 1 {
                return Err("explain requires one finding or rule ID".into());
            }
            if let Some(rule) = pack.rules.iter().find(|r| r.id == arguments[0]) {
                emit(json!(rule), machine, View::Explain);
            } else {
                let (history, diagnostics, notes) = load_history(&directory()?)?;
                let finding = history
                    .iter()
                    .rev()
                    .flat_map(|s| &s.findings)
                    .find(|f| f.id == arguments[0])
                    .ok_or("finding not present in local history")?;
                let rule = pack.rules.iter().find(|r| r.id == finding.rule_id);
                emit(
                    json!({"finding":finding,"rule":rule,"history_diagnostics":diagnostics,"capability_notes":notes}),
                    machine,
                    View::Explain,
                );
            }
        }
        "history" => {
            require_empty(&arguments)?;
            let (history, diagnostics, notes) = load_history(&directory()?)?;
            let rows: Vec<_> = history.iter().enumerate().map(|(index, snapshot)| {
                let comparison = index.checked_sub(1).map(|previous| cockpit_core::history::compare(&history[previous], snapshot));
                json!({"id":snapshot.id,"created_at":snapshot.created_at,"roots":snapshot.report.roots,"accounting":snapshot.report.accounting,"attributed_growth_bytes":comparison.as_ref().and_then(|c| c.attributed_growth_bytes).map(wide),"comparison":comparison.as_ref().map(comparison_value),"findings_count":snapshot.findings.len()})
            }).collect();
            emit(
                json!({"history":rows,"history_diagnostics":diagnostics,"capability_notes":notes}),
                machine,
                View::History,
            );
        }
        "procs" => {
            let sort = take_option(&mut arguments, "--sort")?.unwrap_or_else(|| "ram".into());
            let groups = take_flag(&mut arguments, "--groups");
            require_empty(&arguments)?;
            let mut procs = cockpit_core::procs();
            match sort.as_str() {
                "ram" => procs.sort_by_key(|p| std::cmp::Reverse(p.memory.value)),
                "cpu" => procs.sort_by(|a, b| b.cpu_usage_percent.total_cmp(&a.cpu_usage_percent)),
                "gpu" => return Err("per-process GPU is unavailable in current provider".into()),
                _ => return Err("sort must be cpu, ram or gpu".into()),
            }
            let mut value = json!({"processes":procs,"grouping":"individual_processes","gpu":{"capability":"unavailable"},"actions_enabled":false});
            if groups {
                value["process_groups"] = json!(cockpit_core::processes::group(&procs));
                value["procs_schema"] = json!(2);
            }
            emit(value, machine, View::Procs);
        }
        "usage" => {
            require_empty(&arguments)?;
            emit(
                json!({"claude":{"value":null,"state":"unavailable","source":null,"observed_at":null},"codex":{"value":null,"state":"unavailable","source":null,"observed_at":null},"reason":"pill usage-reader integration pending; credentials are not inspected by CLI"}),
                machine,
                View::Usage,
            );
        }
        "plan" | "apply" | "quit" | "force-quit" | "uninstall-plan" => {
            return Err("mutation is disabled until feasibility & safety gates pass".into());
        }
        _ => return Err(format!("unknown command: {command}").into()),
    }
    Ok(())
}
fn take_option(args: &mut Vec<String>, flag: &str) -> Result<Option<String>, String> {
    if let Some(index) = args.iter().position(|s| s == flag) {
        args.remove(index);
        if index >= args.len() || args[index].starts_with('-') {
            return Err(format!("{flag} requires a value"));
        }
        let value = args.remove(index);
        if args.iter().any(|s| s == flag) {
            return Err(format!("duplicate {flag}"));
        }
        Ok(Some(value))
    } else {
        Ok(None)
    }
}
fn take_number(args: &mut Vec<String>, flag: &str, default: usize) -> Result<usize, String> {
    take_option(args, flag)?.map_or(Ok(default), |s| {
        s.parse().map_err(|_| format!("invalid {flag}"))
    })
}
fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    if let Some(index) = args.iter().position(|s| s == flag) {
        args.remove(index);
        true
    } else {
        false
    }
}
fn require_empty(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(format!("unexpected arguments: {}", args.join(" ")))
    }
}

/// Snapshots, per-file diagnostics and platform capability notes for the
/// store. `capability_notes` discloses platform limits that affect the
/// privacy of the store itself (for example Windows ACL inheritance); it
/// is surfaced verbatim alongside `history_diagnostics`.
type LoadedHistory = (Vec<store::Snapshot>, Vec<Value>, Vec<String>);

fn load_history(directory: &std::path::Path) -> Result<LoadedHistory, String> {
    let report = store::history_report(directory).map_err(|e| e.to_string())?;
    // Stdout JSON gains `history_diagnostics`; stderr keeps one event per file.
    let mut diagnostics = Vec::new();
    for skipped in report.skipped {
        let event = json!({"event":"snapshot_skipped","file":skipped.file,"reason":skipped.reason});
        eprintln!("{event}");
        diagnostics.push(json!({"file":event["file"],"reason":event["reason"]}));
    }
    Ok((report.snapshots, diagnostics, report.capability_notes))
}

fn limits_for_scan(arguments: &mut Vec<String>) -> Result<(usize, usize), String> {
    let max_depth = take_number(arguments, "--max-depth", 64)?;
    let max_entries = take_number(arguments, "--max-entries", 100_000)?;
    if max_depth > 128 || max_entries > 1_000_000 || max_entries == 0 {
        return Err("scan limits: depth ≤ 128; entries 1–1000000".into());
    }
    Ok((max_depth, max_entries))
}

#[cfg(unix)]
type Endpoint = PathBuf;
#[cfg(windows)]
type Endpoint = String;

#[cfg(any(unix, windows))]
fn endpoint_from(option: Option<String>) -> Result<Endpoint, CliError> {
    match option {
        Some(e) => Ok(e.into()),
        None => {
            #[cfg(unix)]
            let default = cockpit_core::ipc::unix::default_endpoint();
            #[cfg(windows)]
            let default = cockpit_core::ipc::windows::default_endpoint();
            Ok(default?)
        }
    }
}

#[cfg(any(unix, windows))]
fn worker(mut arguments: Vec<String>, machine: bool) -> Result<(), CliError> {
    use cockpit_core::ipc::{self, Limits, Outcome, Request, Response};
    if arguments.is_empty() {
        return Err("worker requires serve or request".into());
    }
    let sub = arguments.remove(0);
    let endpoint_option = take_option(&mut arguments, "--endpoint")?;
    match sub.as_str() {
        "serve" => {
            let idle = take_option(&mut arguments, "--idle-seconds")?;
            require_empty(&arguments)?;
            let mut limits = Limits::default();
            if let Some(idle) = idle {
                let seconds: u64 = idle
                    .parse()
                    .map_err(|_| "invalid --idle-seconds".to_string())?;
                if !(1..=600).contains(&seconds) {
                    return Err("--idle-seconds must be 1–600".into());
                }
                limits.idle_exit = std::time::Duration::from_secs(seconds);
            }
            if !limits.viable() {
                return Err(CliError {
                    body: Some(json!({
                        "error": "limits cannot carry the mandatory request/response exchange",
                        "code": ipc::ErrorCode::Internal,
                    })),
                    exit: 2,
                });
            }
            let endpoint = endpoint_from(endpoint_option)?;
            // Each operation runs in a killable `worker exec-op` child of
            // this same binary, bounded by `limits.op_deadline`.
            let mut handler = cockpit_core::worker::Worker::bounded(
                limits,
                Box::new(|event| {
                    if let Ok(line) = serde_json::to_string(event) {
                        eprintln!("{line}");
                    }
                }),
                std::env::current_exe().map_err(|e| e.to_string())?,
            );
            let shutdown = std::sync::atomic::AtomicBool::new(false);
            #[cfg(unix)]
            let exit = ipc::unix::serve(&endpoint, &limits, &mut handler, &shutdown)?;
            #[cfg(windows)]
            let exit = ipc::windows::serve(&endpoint, &limits, &mut handler, &shutdown)?;
            eprintln!("{}", json!({"event":"worker_exit","reason":exit}));
            Ok(())
        }
        "request" => {
            let (max_depth, max_entries) = if arguments.first().map(String::as_str) == Some("scan")
            {
                limits_for_scan(&mut arguments)?
            } else {
                (64, 100_000)
            };
            let groups = take_flag(&mut arguments, "--groups");
            if arguments.is_empty() {
                return Err("worker request requires status, procs or scan".into());
            }
            let op = arguments.remove(0);
            let (op, args) = match op.as_str() {
                "status" => {
                    require_empty(&arguments)?;
                    ("status", json!({}))
                }
                "procs" => {
                    require_empty(&arguments)?;
                    ("processes", json!({"grouped": groups}))
                }
                "scan" => {
                    if arguments.is_empty() {
                        return Err("scan requires explicit paths".into());
                    }
                    if arguments.iter().any(|p| p.starts_with('-')) {
                        return Err("unknown scan option".into());
                    }
                    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
                    let roots: Vec<PathBuf> = arguments
                        .iter()
                        .map(|p| {
                            let p = PathBuf::from(p);
                            if p.is_absolute() { p } else { cwd.join(p) }
                        })
                        .collect();
                    (
                        "scan",
                        json!({"roots":roots,"max_depth":max_depth,"max_entries":max_entries}),
                    )
                }
                other => return Err(format!("unknown worker request: {other}").into()),
            };
            if groups && op != "processes" {
                return Err("--groups applies only to procs".into());
            }
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let id = format!("cli-{}-{nanos}", std::process::id());
            let request = Request {
                version: ipc::PROTOCOL_VERSION,
                id: id.clone(),
                op: op.to_string(),
                args,
            };
            let body = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
            let endpoint = endpoint_from(endpoint_option)?;
            let limits = Limits::default();
            #[cfg(unix)]
            let reply = ipc::unix::request(&endpoint, &body, &limits)?;
            #[cfg(windows)]
            let reply = ipc::windows::request(&endpoint, &body, &limits)?;
            let response: Response = serde_json::from_slice(&reply)
                .map_err(|e| format!("malformed worker response: {e}"))?;
            if response.version != ipc::PROTOCOL_VERSION {
                return Err(format!(
                    "worker response version {} unsupported; expected {}",
                    response.version,
                    ipc::PROTOCOL_VERSION
                )
                .into());
            }
            if response.id.as_deref() != Some(id.as_str()) {
                return Err("worker response id does not match request".into());
            }
            let failed = matches!(response.outcome, Outcome::Error { .. });
            let value = serde_json::to_value(&response).map_err(|e| e.to_string())?;
            emit(value, machine, View::Worker);
            if failed {
                return Err(CliError {
                    body: None,
                    exit: 1,
                });
            }
            Ok(())
        }
        // Hidden internal verb spawned by `Worker::bounded`: one bounded
        // Request on stdin, one bounded Response on stdout, leaf worker.
        // Not listed in help; documented as internal in docs/runtime.md.
        s if s == cockpit_core::worker::EXEC_OP_SUBCOMMAND => {
            let mut limits = Limits::default();
            limits.max_request_bytes = take_number(
                &mut arguments,
                "--max-request-bytes",
                limits.max_request_bytes,
            )?;
            limits.max_response_bytes = take_number(
                &mut arguments,
                "--max-response-bytes",
                limits.max_response_bytes,
            )?;
            require_empty(&arguments)?;
            if !limits.viable() {
                return Err(CliError {
                    body: Some(json!({
                        "error": "limits cannot carry the mandatory request/response exchange",
                        "code": ipc::ErrorCode::Internal,
                    })),
                    exit: 2,
                });
            }
            match cockpit_core::worker::exec_op_stdio(limits) {
                0 => Ok(()),
                code => Err(CliError {
                    body: None,
                    exit: code,
                }),
            }
        }
        _ => Err(format!("unknown worker command: {sub}").into()),
    }
}

#[cfg(not(any(unix, windows)))]
fn worker(_arguments: Vec<String>, _machine: bool) -> Result<(), CliError> {
    Err("worker is unsupported on this platform".into())
}
