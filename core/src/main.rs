use cockpit_core::{ScanOptions, rules, scan_paths, store};
use serde_json::{Value, json};
use std::{path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{}", json!({"error": message}));
            ExitCode::from(2)
        }
    }
}
fn emit(value: Value, machine: bool) {
    if machine {
        println!("{value}");
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).expect("serializable result")
        );
    }
}
fn run(mut arguments: Vec<String>) -> Result<(), String> {
    let machine = arguments.iter().any(|a| a == "--json");
    arguments.retain(|a| a != "--json");
    if arguments.is_empty() || ["help", "--help", "-h"].contains(&arguments[0].as_str()) {
        println!(
            "Cockpit — read-only system inspection\n\nstatus [--json]\nscan <path…> [--max-depth N] [--max-entries N] [--save] [--state-dir PATH] [--json]\nfindings [--rule ID] [--state-dir PATH] [--json]\nexplain <finding-id|rule-id> [--state-dir PATH] [--json]\nhistory [--state-dir PATH] [--json]\nprocs [--sort cpu|ram|gpu] [--json]\nusage [--json]\n\nScans never read file contents. --save opts into local metadata history.\nCleanup, uninstall & process actions await feasibility & safety gates."
        );
        return Ok(());
    }
    let command = arguments.remove(0);
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
            emit(json!({"snapshot":snapshot,"saved_to":saved_to}), machine);
        }
        "findings" => {
            let rule = take_option(&mut arguments, "--rule")?;
            require_empty(&arguments)?;
            let history = store::history(&directory()?).map_err(|e| e.to_string())?;
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
                json!({"snapshot_id":history.last().map(|s| &s.id),"findings":findings,"explanations":explanations,"mode":"report_only"}),
                machine,
            );
        }
        "explain" => {
            if arguments.len() != 1 {
                return Err("explain requires one finding or rule ID".into());
            }
            if let Some(rule) = pack.rules.iter().find(|r| r.id == arguments[0]) {
                emit(json!(rule), machine);
            } else {
                let history = store::history(&directory()?).map_err(|e| e.to_string())?;
                let finding = history
                    .iter()
                    .rev()
                    .flat_map(|s| &s.findings)
                    .find(|f| f.id == arguments[0])
                    .ok_or("finding not present in local history")?;
                let rule = pack.rules.iter().find(|r| r.id == finding.rule_id);
                emit(json!({"finding":finding,"rule":rule}), machine);
            }
        }
        "history" => {
            require_empty(&arguments)?;
            let history = store::history(&directory()?).map_err(|e| e.to_string())?;
            let mut previous = None;
            let rows: Vec<_> = history.iter().map(|s| {
                let bytes = s.report.accounting.attributed_allocation_bytes;
                let scope = serde_json::to_string(&(&s.report.roots, s.report.volume_usage.iter().map(|v| &v.volume).collect::<Vec<_>>())).unwrap_or_default();
                let change = previous.as_ref().and_then(|(old_scope, old_bytes)| if old_scope == &scope { Some(i128::from(bytes)-i128::from(*old_bytes)) } else { None });
                previous = Some((scope,bytes));
                json!({"id":s.id,"created_at":s.created_at,"roots":s.report.roots,"accounting":s.report.accounting,"attributed_growth_bytes":change,"findings_count":s.findings.len()})
            }).collect();
            emit(json!({"history":rows}), machine);
        }
        "procs" => {
            let sort = take_option(&mut arguments, "--sort")?.unwrap_or_else(|| "ram".into());
            require_empty(&arguments)?;
            let mut procs = cockpit_core::procs();
            match sort.as_str() {
                "ram" => procs.sort_by_key(|p| std::cmp::Reverse(p.memory.value)),
                "cpu" => procs.sort_by(|a, b| b.cpu_usage_percent.total_cmp(&a.cpu_usage_percent)),
                "gpu" => return Err("per-process GPU is unavailable in current provider".into()),
                _ => return Err("sort must be cpu, ram or gpu".into()),
            }
            emit(
                json!({"processes":procs,"grouping":"individual_processes","gpu":{"capability":"unavailable"},"actions_enabled":false}),
                machine,
            );
        }
        "usage" => {
            require_empty(&arguments)?;
            emit(
                json!({"claude":{"value":null,"state":"unavailable","source":null,"observed_at":null},"codex":{"value":null,"state":"unavailable","source":null,"observed_at":null},"reason":"pill usage-reader integration pending; credentials are not inspected by CLI"}),
                machine,
            );
        }
        "plan" | "apply" | "quit" | "force-quit" | "uninstall-plan" => {
            return Err("mutation is disabled until feasibility & safety gates pass".into());
        }
        _ => return Err(format!("unknown command: {command}")),
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
