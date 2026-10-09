use pulse_core::presentation::{RenderOptions, View, render};
use pulse_core::{
    FilesystemProvider, ScanOptions, StdFilesystemProvider, rules, scan_paths, scan_with_provider,
    store,
};
use serde_json::{Value, json};
mod claude_cmd;
#[cfg(feature = "localsend")]
mod send_cmd;

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

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
impl From<pulse_core::ipc::IpcError> for CliError {
    fn from(error: pulse_core::ipc::IpcError) -> Self {
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
fn comparison_value(c: &pulse_core::history::Comparison) -> Value {
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
            "Pulse — system inspection; `apps uninstall` moves to Trash\n\nstatus [--json]\nscan <path…> [--max-depth N] [--max-entries N] [--save] [--state-dir PATH] [--exclude-state PATH] [--json]\nfindings [--rule ID] [--state-dir PATH] [--json]\nexplain <finding-id|rule-id> [--state-dir PATH] [--json]\nhistory [--state-dir PATH] [--json]\nprocs [--sort cpu|ram|gpu] [--groups] [--json]\nmonitor [--json]\nfind <query> [--ext EXT] [--kind file|directory] [--min-size N] [--max-size N] [--offset N] [--limit N] [--state-dir PATH] [--json]\nbrowse [--folder PATH|--inspect PATH|--largest files|folders] [--offset N] [--limit N] [--state-dir PATH] [--json]\nexport [SNAPSHOT-ID] [--state-dir PATH] [--json]\nduplicates <path…> [--min-size N] [--max-files N] [--max-read-bytes N] [--seconds N] [--json]\nworker serve [--endpoint E] [--idle-seconds 1-600]\nworker request status|procs [--groups]|scan <path…> [--max-depth N] [--max-entries N] [--endpoint E] [--json]\nusage [--json]\nsend <file|folder…> --to <alias> [--json]  (nearby device, LocalSend protocol; it must accept)\nsend --list [--json]\nclaude accounts|known|backups [--json]\nclaude auto [--json]  (sync every account on disk, minus excluded ones, when Claude is closed)\nclaude include|exclude <account-id> [--json]\nclaude sync --dry-run|--apply [--json]  (merges Code-session metadata across Claude Desktop accounts; Claude must be closed)\nclaude restore <backup-id> [--force] [--json]\napps list [--json]\napps updates [--json]\napps detail <app-path|bundle-id> [--json]\napps uninstall <app-path|bundle-id> [--include <item-path>]... [--only-preselected] [--json]\n\nScans never read file contents. duplicates explicitly reads local file contents under bounded limits. --save opts into local metadata history.\napps uninstall quits the app, moves the preselected items (plus any --include) to the Trash with re-validation, and prints the result as JSON; exit 1 if the app itself was not moved. Cleanup & process actions await feasibility & safety gates."
        );
        return Ok(());
    }
    let command = arguments.remove(0);
    if command == "worker" {
        return worker(arguments, machine);
    }
    if command == "claude" {
        return claude_cmd::run(arguments, machine).map_err(|(message, code)| CliError {
            body: Some(match code {
                Some(code) => json!({"error": message, "code": code}),
                None => json!({"error": message}),
            }),
            exit: 2,
        });
    }
    if command == "send" {
        #[cfg(feature = "localsend")]
        return send_cmd::run(arguments, machine).map_err(CliError::from);
        #[cfg(not(feature = "localsend"))]
        return Err("this build of pulse has no nearby sharing".into());
    }
    let state_override = take_option(&mut arguments, "--state-dir")?.map(PathBuf::from);
    #[cfg(target_os = "macos")]
    if state_override.is_none() {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "HOME is unset".to_string())?;
        pulse_core::state_migration::migrate_mac_state(&home).map_err(|e| e.to_string())?;
    }
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
                json!({"schema_version":1,"system":pulse_core::system_status(),"snapshots":{"capability":"unavailable","reason":"snapshot provider pending"},"purgeable_bytes":null}),
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
            let exclude_state = take_option(&mut arguments, "--exclude-state")?
                .map(|path| resolve_path(&path))
                .transpose()?;
            if exclude_state.is_some() && !save {
                return Err("--exclude-state requires --save".into());
            }
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
            let state_directory = if save { Some(directory()?) } else { None };
            let excluded_state = if let Some(excluded) = exclude_state {
                let state_directory = state_directory
                    .as_deref()
                    .ok_or("--exclude-state requires --save")?;
                let canonical_state = canonical_nearest_existing(state_directory)?;
                let canonical_excluded = canonical_nearest_existing(&excluded)?;
                if canonical_state != canonical_excluded {
                    return Err("--exclude-state must match --state-dir".into());
                }
                let inside_root = paths.iter().any(|root| {
                    canonical_nearest_existing(root)
                        .map(|root| {
                            canonical_excluded == root || canonical_excluded.starts_with(root)
                        })
                        .unwrap_or(false)
                });
                if !inside_root {
                    return Err("--exclude-state must be inside selected scan roots".into());
                }
                Some(canonical_excluded)
            } else {
                if let Some(state_directory) = state_directory.as_deref() {
                    ensure_state_directory_outside_roots(state_directory, &paths)?;
                }
                None
            };
            let options = ScanOptions {
                max_depth,
                max_entries,
                ..Default::default()
            };
            let scan_roots = excluded_state
                .as_deref()
                .map(|_| {
                    paths
                        .iter()
                        .map(|path| canonical_nearest_existing(path))
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?
                .unwrap_or_else(|| paths.clone());
            let mut report = if let Some(excluded) = excluded_state.as_deref() {
                scan_with_provider(
                    &ExcludingProvider {
                        inner: StdFilesystemProvider,
                        excluded: excluded.to_path_buf(),
                    },
                    &scan_roots,
                    &options,
                )
            } else {
                scan_paths(&scan_roots, &options)
            };
            if let Some(excluded) = excluded_state.as_deref() {
                report.accounting.incomplete = true;
                let reason = format!(
                    "excluded state directory not scanned: {}",
                    excluded.display()
                );
                report.incomplete_reasons.push(reason.clone());
                report.accounting.reclaim.upper_bytes = None;
                report.accounting.reclaim.state = Some(pulse_core::ReclaimState::Unknown);
                report.accounting.reclaim.reasons.push(reason);
                for folder in &mut report.folders {
                    folder.incomplete = true;
                }
            }
            let mut totals = std::collections::BTreeMap::<PathBuf, (u64, u64)>::new();
            for entry in &report.entries {
                for ancestor in entry.path.ancestors() {
                    let total = totals.entry(ancestor.to_path_buf()).or_default();
                    total.0 = total.0.saturating_add(entry.logical_bytes);
                    total.1 = total.1.saturating_add(entry.attributed_allocation_bytes);
                }
            }
            let chrome_running = pulse_core::procs()
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
            let saved_to = state_directory
                .as_deref()
                .map(|directory| store::save(directory, &snapshot).map_err(|e| e.to_string()))
                .transpose()?;
            if let Some(state_directory) = state_directory.as_deref() {
                let mut activity =
                    pulse_core::activity::DurableActivityLedger::open(state_directory)
                        .map_err(|e| e.to_string())?;
                activity
                    .record(pulse_core::activity::ActivityEvent {
                        id: snapshot.id.clone(),
                        occurred_at: snapshot.created_at,
                        kind: pulse_core::activity::ActivityKind::Scan {
                            logical_bytes: snapshot.report.accounting.logical_bytes,
                            attributed_bytes: snapshot
                                .report
                                .accounting
                                .attributed_allocation_bytes,
                        },
                    })
                    .map_err(|e| e.to_string())?;
            }
            emit(
                json!({"snapshot":snapshot,"saved_to":saved_to}),
                machine,
                View::Scan,
            );
        }
        "findings" => {
            let rule = take_option(&mut arguments, "--rule")?;
            require_empty(&arguments)?;
            let (history, diagnostics, notes, _) = load_history(&directory()?)?;
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
                let (history, diagnostics, notes, _) = load_history(&directory()?)?;
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
            let (history, diagnostics, notes, _) = load_history(&directory()?)?;
            let rows: Vec<_> = history.iter().enumerate().map(|(index, snapshot)| {
                let comparison = index.checked_sub(1).map(|previous| pulse_core::history::compare(&history[previous], snapshot));
                let folder_comparison = index.checked_sub(1).map(|previous| {
                    pulse_core::folder_growth::compare_folders(&history[previous], snapshot, 100)
                });
                json!({"id":snapshot.id,"created_at":snapshot.created_at,"roots":snapshot.report.roots,"accounting":snapshot.report.accounting,"attributed_growth_bytes":comparison.as_ref().and_then(|c| c.attributed_growth_bytes).map(wide),"comparison":comparison.as_ref().map(comparison_value),"folder_comparison":folder_comparison,"findings_count":snapshot.findings.len()})
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
            let mut procs = pulse_core::procs();
            match sort.as_str() {
                "ram" => procs.sort_by_key(|p| std::cmp::Reverse(p.memory.value)),
                "cpu" => procs.sort_by(|a, b| b.cpu_usage_percent.total_cmp(&a.cpu_usage_percent)),
                "gpu" => return Err("per-process GPU is unavailable in current provider".into()),
                _ => return Err("sort must be cpu, ram or gpu".into()),
            }
            let mut value = json!({"processes":procs,"grouping":"individual_processes","gpu":{"capability":"unavailable"},"actions_enabled":false});
            if groups {
                value["process_groups"] = json!(pulse_core::processes::group(&procs));
                value["procs_schema"] = json!(2);
            }
            emit(value, machine, View::Procs);
        }
        "monitor" => {
            require_empty(&arguments)?;
            let extended = pulse_core::monitor::sample_extended();
            emit_inspection(
                json!({
                    "schema_version": 1,
                    "extended": extended,
                    "modules": {"monitor": extended},
                    "actions_enabled": false,
                }),
                machine,
            );
        }
        "find" | "browse" | "export" => {
            let offset = take_number(&mut arguments, "--offset", 0)?;
            let limit = take_number(&mut arguments, "--limit", 100)?;
            pulse_core::storage_browser::validate_page_bounds(offset, limit)
                .map_err(|e| e.to_string())?;
            let requested_snapshot_id = if command == "export" {
                if arguments.len() > 1 {
                    return Err("export accepts at most one snapshot ID".into());
                }
                arguments.pop()
            } else {
                None
            };
            let (history, diagnostics, notes, history_skips) = load_history(&directory()?)?;
            let snapshot_index = if let Some(id) = requested_snapshot_id.as_deref() {
                history
                    .iter()
                    .position(|snapshot| snapshot.id == id)
                    .ok_or("snapshot ID is not present in local history")?
            } else {
                history
                    .len()
                    .checked_sub(1)
                    .ok_or("no saved scan; run scan <path> --save first")?
            };
            let snapshot = &history[snapshot_index];
            let report = &snapshot.report;
            let payload = match command.as_str() {
                "find" => {
                    let extension = take_option(&mut arguments, "--ext")?;
                    let kind_text = take_option(&mut arguments, "--kind")?;
                    let kind = match kind_text.as_deref() {
                        None => None,
                        Some("file") => Some(pulse_core::EntryKind::File),
                        Some("directory") => Some(pulse_core::EntryKind::Directory),
                        _ => return Err("kind must be file or directory".into()),
                    };
                    let min_size = take_u64_option(&mut arguments, "--min-size")?;
                    let max_size = take_u64_option(&mut arguments, "--max-size")?;
                    if arguments.len() != 1 || arguments[0].starts_with('-') {
                        return Err("find requires one filename query".into());
                    }
                    let request = pulse_core::storage_browser::SearchRequest {
                        query: arguments.remove(0),
                        kind,
                        extension,
                        min_size,
                        max_size,
                        offset,
                        limit,
                    };
                    json!(
                        pulse_core::storage_browser::search(report, &request)
                            .map_err(|e| e.to_string())?
                    )
                }
                "browse" => {
                    let folder = take_option(&mut arguments, "--folder")?
                        .map(|path| resolve_path(&path))
                        .transpose()?;
                    let inspect = take_option(&mut arguments, "--inspect")?
                        .map(|path| resolve_path(&path))
                        .transpose()?;
                    let largest = take_option(&mut arguments, "--largest")?;
                    require_empty(&arguments)?;
                    if usize::from(folder.is_some())
                        + usize::from(inspect.is_some())
                        + usize::from(largest.is_some())
                        > 1
                    {
                        return Err("choose one browse mode".into());
                    }
                    if let Some(path) = inspect {
                        json!(
                            pulse_core::storage_browser::inspect_path(report, &path)
                                .map_err(|e| e.to_string())?
                        )
                    } else if let Some(path) = folder {
                        json!(
                            pulse_core::storage_browser::drilldown_children(
                                report, &path, offset, limit
                            )
                            .map_err(|e| e.to_string())?
                        )
                    } else {
                        if offset != 0 {
                            return Err("offset requires --folder".into());
                        }
                        match largest.as_deref().unwrap_or("folders") {
                            "files" => json!({
                                "items": pulse_core::storage_browser::largest_files(report, limit)
                                    .map_err(|e| e.to_string())?,
                                "limit": limit,
                                "incomplete": report.accounting.incomplete,
                            }),
                            "folders" => json!({
                                "items": pulse_core::storage_browser::largest_folders(report, limit)
                                    .map_err(|e| e.to_string())?,
                                "limit": limit,
                                "incomplete": report.accounting.incomplete,
                            }),
                            _ => return Err("largest must be files or folders".into()),
                        }
                    }
                }
                _ => {
                    require_empty(&arguments)?;
                    let folder_growth = snapshot_index.checked_sub(1).map(|previous| {
                        pulse_core::folder_growth::compare_folders(
                            &history[previous],
                            snapshot,
                            limit,
                        )
                    });
                    let export = pulse_core::dashboard_export::DashboardExport::from_snapshot(
                        snapshot.clone(),
                        history
                            .iter()
                            .map(pulse_core::dashboard_export::SnapshotSummary::from_snapshot)
                            .collect(),
                        history_skips,
                        load_activity_projection(&directory()?, snapshot.created_at)?,
                        folder_growth,
                        pulse_core::monitor::sample_extended(),
                    );
                    emit_inspection(
                        serde_json::to_value(export).map_err(|e| e.to_string())?,
                        machine,
                    );
                    return Ok(());
                }
            };
            emit_inspection(
                json!({
                    "schema_version": 1,
                    "operation": command,
                    "snapshot_id": snapshot.id,
                    "sampled_at": snapshot.created_at,
                    "data": payload,
                    "history_diagnostics": diagnostics,
                    "capability_notes": notes,
                }),
                machine,
            );
        }
        "duplicates" => {
            let min_bytes = take_u64_option(&mut arguments, "--min-size")?
                .unwrap_or(pulse_core::duplicates::DEFAULT_MIN_DUPLICATE_BYTES);
            let max_files = take_number(&mut arguments, "--max-files", 100_000)?;
            let max_total_read_bytes = take_u64_option(&mut arguments, "--max-read-bytes")?
                .unwrap_or(pulse_core::duplicates::DEFAULT_MAX_TOTAL_READ_BYTES);
            let seconds = take_number(&mut arguments, "--seconds", 30)?;
            if max_files == 0
                || max_files > 100_000
                || max_total_read_bytes == 0
                || max_total_read_bytes > (8u64 << 30)
                || seconds == 0
                || seconds > 120
            {
                return Err(
                    "duplicate limits: files 1–100000; read bytes 1–8GiB; seconds 1–120".into(),
                );
            }
            if arguments.is_empty() || arguments.iter().any(|path| path.starts_with('-')) {
                return Err("duplicates requires explicit scan paths".into());
            }
            let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
            let roots: Vec<_> = arguments
                .iter()
                .map(|path| {
                    let path = PathBuf::from(path);
                    if path.is_absolute() {
                        path
                    } else {
                        cwd.join(path)
                    }
                })
                .collect();
            let report = scan_paths(
                &roots,
                &ScanOptions {
                    max_entries: max_files,
                    ..Default::default()
                },
            );
            let paths: Vec<_> = report
                .entries
                .iter()
                .filter(|entry| {
                    entry.metadata.kind == pulse_core::EntryKind::File
                        && !entry.metadata.is_placeholder
                        && entry.metadata.metadata_complete
                })
                .map(|entry| entry.path.clone())
                .collect();
            let duplicates = pulse_core::duplicates::find_duplicates(
                &paths,
                &pulse_core::duplicates::DuplicateOptions {
                    min_bytes,
                    max_files,
                    max_total_read_bytes,
                    deadline: std::time::Duration::from_secs(seconds as u64),
                },
            );
            emit_inspection(
                json!({
                    "schema_version": 1,
                    "duplicates": duplicates,
                    "scan_incomplete": report.accounting.incomplete,
                    "scan_diagnostics": report.incomplete_reasons,
                    "actions_enabled": false,
                }),
                machine,
            );
        }
        "usage" => {
            require_empty(&arguments)?;
            emit(
                json!({"claude":{"value":null,"state":"unavailable","source":null,"observed_at":null},"codex":{"value":null,"state":"unavailable","source":null,"observed_at":null},"reason":"notch provider readings are available in native app; CLI unavailable; credentials are not inspected by CLI"}),
                machine,
                View::Usage,
            );
        }
        "apps" => return apps(arguments, machine),
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
fn emit_inspection(value: Value, machine: bool) {
    if machine {
        println!("{value}");
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).expect("JSON value serialization")
        );
    }
}
fn take_u64_option(args: &mut Vec<String>, flag: &str) -> Result<Option<u64>, String> {
    take_option(args, flag)?
        .map(|value| value.parse().map_err(|_| format!("invalid {flag}")))
        .transpose()
}
fn resolve_path(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path))
    }
}
/// Canonicalize a path even when its leaf does not exist yet. Existing
/// ancestors are resolved first so `root/../root/state` cannot evade scope
/// checks through lexical aliases.
fn canonical_nearest_existing(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        resolve_path(&path.to_string_lossy())?
    };
    let mut cursor = absolute.as_path();
    let mut missing = Vec::<OsString>::new();
    loop {
        match fs::symlink_metadata(cursor) {
            Ok(_) => {
                let mut canonical = fs::canonicalize(cursor).map_err(|e| e.to_string())?;
                for component in missing.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = cursor
                    .file_name()
                    .ok_or("cannot normalize path without existing ancestor")?;
                missing.push(name.to_os_string());
                cursor = cursor
                    .parent()
                    .ok_or("cannot normalize path without existing ancestor")?;
            }
            Err(error) => return Err(format!("cannot inspect path: {error}")),
        }
    }
}

fn ensure_state_directory_outside_roots(
    state_directory: &Path,
    roots: &[PathBuf],
) -> Result<(), String> {
    let state = canonical_nearest_existing(state_directory)?;
    for root in roots {
        let root = canonical_nearest_existing(root)?;
        if state == root || state.starts_with(&root) {
            return Err(format!(
                "--state-dir must be outside selected scan roots (state: {}; root: {})",
                state.display(),
                root.display()
            ));
        }
    }
    Ok(())
}

/// Provider boundary used only by an explicit saved scan. The excluded state
/// subtree is never inspected or enumerated, while its omission is disclosed
/// in the resulting report by the caller above.
struct ExcludingProvider<P> {
    inner: P,
    excluded: PathBuf,
}

impl<P: FilesystemProvider> ExcludingProvider<P> {
    fn is_excluded(&self, path: &Path) -> bool {
        path == self.excluded || path.starts_with(&self.excluded)
    }

    fn reject(&self, path: &Path) -> Result<(), pulse_core::FsError> {
        if self.is_excluded(path) {
            Err(pulse_core::FsError::new(format!(
                "excluded state directory: {}",
                path.display()
            )))
        } else {
            Ok(())
        }
    }
}

impl<P: FilesystemProvider> FilesystemProvider for ExcludingProvider<P> {
    fn begin_scan(&self) {
        self.inner.begin_scan();
    }

    fn inspect(&self, path: &Path) -> Result<pulse_core::FileMetadata, pulse_core::FsError> {
        self.reject(path)?;
        self.inner.inspect(path)
    }

    fn inspect_detailed(
        &self,
        path: &Path,
    ) -> Result<(pulse_core::FileMetadata, Vec<String>), pulse_core::FsError> {
        self.reject(path)?;
        self.inner.inspect_detailed(path)
    }

    fn children(&self, path: &Path) -> Result<Vec<PathBuf>, pulse_core::FsError> {
        self.reject(path)?;
        Ok(self
            .inner
            .children(path)?
            .into_iter()
            .filter(|child| !self.is_excluded(child))
            .collect())
    }

    fn children_bounded(
        &self,
        path: &Path,
        limit: usize,
    ) -> Result<(Vec<PathBuf>, bool), pulse_core::FsError> {
        self.reject(path)?;
        let (children, truncated) = self.inner.children_bounded(path, limit)?;
        Ok((
            children
                .into_iter()
                .filter(|child| !self.is_excluded(child))
                .collect(),
            truncated,
        ))
    }

    fn children_with_files(
        &self,
        path: &Path,
        limit: usize,
    ) -> Result<pulse_core::scan::ChildrenWithFiles, pulse_core::FsError> {
        self.reject(path)?;
        let (children, truncated) = self.inner.children_with_files(path, limit)?;
        Ok((
            children
                .into_iter()
                .filter(|(child, _)| !self.is_excluded(child))
                .collect(),
            truncated,
        ))
    }

    fn volume_usage(
        &self,
        volume: &pulse_core::VolumeIdentity,
    ) -> Result<pulse_core::VolumeUsage, pulse_core::FsError> {
        self.inner.volume_usage(volume)
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
type LoadedHistory = (
    Vec<store::Snapshot>,
    Vec<Value>,
    Vec<String>,
    Vec<pulse_core::dashboard_export::HistorySkip>,
);

fn load_history(directory: &std::path::Path) -> Result<LoadedHistory, String> {
    let report = store::history_report(directory).map_err(|e| e.to_string())?;
    // Stdout JSON gains `history_diagnostics`; stderr keeps one event per file.
    let mut diagnostics = Vec::new();
    let mut skips = Vec::new();
    for skipped in report.skipped {
        let event = json!({"event":"snapshot_skipped","file":skipped.file,"reason":skipped.reason});
        eprintln!("{event}");
        diagnostics.push(json!({"file":event["file"],"reason":event["reason"]}));
        skips.push(pulse_core::dashboard_export::HistorySkip {
            file: event["file"].as_str().unwrap_or_default().to_owned(),
            reason: event["reason"].as_str().unwrap_or_default().to_owned(),
        });
    }
    Ok((
        report.snapshots,
        diagnostics,
        report.capability_notes,
        skips,
    ))
}

fn load_activity_projection(
    directory: &Path,
    timestamp: u64,
) -> Result<pulse_core::dashboard_export::ActivityProjection, String> {
    let activity_directory = directory.join("activity");
    match fs::symlink_metadata(&activity_directory) {
        Ok(metadata) if metadata.is_dir() => {
            let ledger = pulse_core::activity::DurableActivityLedger::open(directory)
                .map_err(|e| e.to_string())?;
            Ok(pulse_core::dashboard_export::ActivityProjection::from_ledger(&ledger, timestamp))
        }
        Ok(_) => Err("activity state path is not a directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(
            pulse_core::dashboard_export::ActivityProjection::empty(timestamp),
        ),
        Err(error) => Err(error.to_string()),
    }
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
        // Endpoint is PathBuf on unix and String on Windows, where this is a no-op.
        #[allow(clippy::useless_conversion)]
        Some(e) => Ok(e.into()),
        None => {
            #[cfg(unix)]
            let default = pulse_core::ipc::unix::default_endpoint();
            #[cfg(windows)]
            let default = pulse_core::ipc::windows::default_endpoint();
            Ok(default?)
        }
    }
}

#[cfg(any(unix, windows))]
fn worker(mut arguments: Vec<String>, machine: bool) -> Result<(), CliError> {
    use pulse_core::ipc::{self, Limits, Outcome, Request, Response};
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
            let mut handler = pulse_core::worker::Worker::bounded(
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
        s if s == pulse_core::worker::EXEC_OP_SUBCOMMAND => {
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
            match pulse_core::worker::exec_op_stdio(limits) {
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

#[cfg(unix)]
fn resolve_app(target: &str) -> Result<String, String> {
    use pulse_core::app_manager::list_apps;
    if target.contains('/') || target.ends_with(".app") {
        return Ok(resolve_path(target)?.to_string_lossy().into_owned());
    }
    let matches: Vec<_> = list_apps()
        .into_iter()
        .filter(|a| a.bundle_id.as_deref() == Some(target))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one.path.clone()),
        [] => Err(format!("no installed app has bundle id {target}")),
        many => Err(format!(
            "ambiguous bundle id {target}; pass one of these paths: {}",
            many.iter()
                .map(|a| a.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

#[cfg(not(unix))]
fn apps(_arguments: Vec<String>, _machine: bool) -> Result<(), CliError> {
    Err("apps is not available on this platform yet".into())
}

#[cfg(unix)]
fn apps(mut arguments: Vec<String>, machine: bool) -> Result<(), CliError> {
    use pulse_core::app_manager;
    if arguments.is_empty() {
        return Err("apps requires list, updates, detail or uninstall".into());
    }
    let sub = arguments.remove(0);
    match sub.as_str() {
        "list" => {
            require_empty(&arguments)?;
            let apps = app_manager::list_apps();
            emit_inspection(json!({"apps": apps}), machine);
        }
        "updates" => {
            require_empty(&arguments)?;
            let report = app_manager::check_updates(false, &|_: &app_manager::AppUpdate| {});
            if machine {
                let value = serde_json::to_value(&report).map_err(|e| e.to_string())?;
                println!("{value}");
            } else {
                for row in &report.apps {
                    match row.state.as_str() {
                        "available" => println!(
                            "{}: update available {} (installed {}, via {})",
                            row.name,
                            row.latest_version.as_deref().unwrap_or("?"),
                            row.installed_version.as_deref().unwrap_or("?"),
                            row.source
                        ),
                        "app_store" => {
                            println!("{}: App Store (open it there to update)", row.name)
                        }
                        _ => {}
                    }
                }
                let available = report
                    .apps
                    .iter()
                    .filter(|r| r.state == "available")
                    .count();
                println!(
                    "{available} update(s) available across {} apps",
                    report.apps.len()
                );
            }
        }
        "detail" => {
            if arguments.len() != 1 {
                return Err("apps detail requires exactly one <app-path|bundle-id>".into());
            }
            let path = resolve_app(&arguments[0])?;
            let detail = app_manager::app_detail(&path)?;
            emit_inspection(
                serde_json::to_value(detail).map_err(|e| e.to_string())?,
                machine,
            );
        }
        "uninstall" => {
            let mut includes = Vec::new();
            // Repeatable, so not `take_option` (which refuses repeats).
            while let Some(index) = arguments.iter().position(|a| a == "--include") {
                arguments.remove(index);
                if index >= arguments.len() || arguments[index].starts_with('-') {
                    return Err("--include requires a value".into());
                }
                includes.push(arguments.remove(index));
            }
            let only_preselected = arguments.iter().position(|a| a == "--only-preselected");
            if let Some(i) = only_preselected {
                arguments.remove(i);
                if !includes.is_empty() {
                    return Err("--only-preselected cannot be combined with --include".into());
                }
            }
            if arguments.len() != 1 {
                return Err("apps uninstall requires exactly one <app-path|bundle-id>".into());
            }
            let path = resolve_app(&arguments[0])?;
            let detail = app_manager::app_detail(&path)?;
            let mut items: Vec<String> = detail
                .items
                .iter()
                .filter(|i| i.preselected)
                .map(|i| i.path.clone())
                .collect();
            for include in includes {
                let include = resolve_path(&include)?.to_string_lossy().into_owned();
                if !detail.items.iter().any(|i| i.path == include) {
                    return Err(format!("not a related item of this app: {include}").into());
                }
                if !items.contains(&include) {
                    items.push(include);
                }
            }
            if items.is_empty() {
                return Err("nothing is selected to move".into());
            }
            let result = app_manager::uninstall(&path, detail.app.bundle_id.as_deref(), &items)?;
            let app_failed = result.failed.iter().any(|f| f.path == detail.app.path)
                || (!result.moved.iter().any(|m| m.path == detail.app.path)
                    && items.contains(&detail.app.path));
            let value = json!({
                "app": detail.app.name,
                "moved": result.moved,
                "failed": result.failed,
                "bytes_freed": result.moved_bytes,
                "activity_id": result.activity_id,
            });
            println!("{value}");
            if app_failed {
                return Err(CliError {
                    body: None,
                    exit: 1,
                });
            }
        }
        other => return Err(format!("unknown apps command: {other}").into()),
    }
    Ok(())
}
