//! `pulse claude …`: Claude Desktop account/session sync. Logic and format
//! notes live in `pulse_core::claude_sync` and docs/claude-account-switch.md.

use std::path::PathBuf;

use pulse_core::claude_sync::{self as sync, SyncError};
use serde_json::{Value, json};

/// A CLI failure: message plus a stable machine code when there is one.
pub type Failure = (String, Option<&'static str>);

fn fail(error: SyncError) -> Failure {
    (error.to_string(), Some(error.code()))
}

fn plain(message: impl Into<String>) -> Failure {
    (message.into(), None)
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    match args.iter().position(|s| s == flag) {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    }
}

fn take_option(args: &mut Vec<String>, flag: &str) -> Result<Option<String>, Failure> {
    let Some(i) = args.iter().position(|s| s == flag) else {
        return Ok(None);
    };
    args.remove(i);
    if i >= args.len() || args[i].starts_with('-') {
        return Err(plain(format!("{flag} requires a value")));
    }
    Ok(Some(args.remove(i)))
}

fn emit(value: &Value, machine: bool) {
    if machine {
        println!("{value}");
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(value).expect("JSON value serialization")
        );
    }
}

pub fn run(mut args: Vec<String>, machine: bool) -> Result<(), Failure> {
    if args.is_empty() {
        return Err(plain(
            "claude requires accounts, known, auto, include, exclude, sync, backups or restore",
        ));
    }
    let sub = args.remove(0);
    // `--root` and `--backups` point the command at a copy of the layout
    // (QA and fixtures); by default they are Claude's real folders.
    let root = take_option(&mut args, "--root")?.map(PathBuf::from);
    let backups_dir = take_option(&mut args, "--backups")?.map(PathBuf::from);
    let registry_override = take_option(&mut args, "--registry")?.map(PathBuf::from);
    let root = || root.clone().map(Ok).unwrap_or_else(sync::default_root);
    let backups_dir = || {
        backups_dir
            .clone()
            .map(Ok)
            .unwrap_or_else(sync::default_backups)
    };
    match sub.as_str() {
        "accounts" => {
            no_more(&args)?;
            let accounts = sync::accounts(&root().map_err(fail)?).map_err(fail)?;
            emit(&json!({"accounts": accounts}), machine);
        }
        "backups" => {
            no_more(&args)?;
            let list = sync::backups(&backups_dir().map_err(fail)?);
            emit(&json!({"backups": list}), machine);
        }
        "auto" => {
            no_more(&args)?;
            let registry = registry_override
                .clone()
                .map(Ok)
                .unwrap_or_else(sync::registry_path)
                .map_err(fail)?;
            let result = sync::auto_sync(
                &root().map_err(fail)?,
                &backups_dir().map_err(fail)?,
                &registry,
                &sync::claude_running,
                sync::now_ms(),
            )
            .map_err(fail)?;
            emit(&json!({"auto": result}), machine);
        }
        "known" => {
            no_more(&args)?;
            let registry = registry_override
                .clone()
                .map(Ok)
                .unwrap_or_else(sync::registry_path)
                .map_err(fail)?;
            emit(
                &json!({"registry": sync::load_registry(&registry)}),
                machine,
            );
        }
        "include" | "exclude" => {
            let id = args
                .first()
                .cloned()
                .ok_or_else(|| plain("an account id is required"))?;
            args.remove(0);
            no_more(&args)?;
            let registry = registry_override
                .clone()
                .map(Ok)
                .unwrap_or_else(sync::registry_path)
                .map_err(fail)?;
            let updated = sync::set_included(&registry, &id, sub == "include").map_err(fail)?;
            emit(&json!({"registry": updated}), machine);
        }
        "sync" => {
            let dry = take_flag(&mut args, "--dry-run");
            let apply = take_flag(&mut args, "--apply");
            no_more(&args)?;
            if dry == apply {
                return Err(plain("sync needs exactly one of --dry-run or --apply"));
            }
            let root = root().map_err(fail)?;
            // Every account on disk, minus any explicitly excluded in the
            // registry. No registry entry is needed for an account to sync.
            let registry = registry_override
                .clone()
                .map(Ok)
                .unwrap_or_else(sync::registry_path)
                .map_err(fail)?;
            let only = sync::load_registry(&registry)
                .sync_set(&sync::accounts(&root).map_err(fail)?);
            if dry {
                let plan = sync::plan_for(&root, Some(&only)).map_err(fail)?;
                emit(&json!({"dry_run": true, "plan": plan}), machine);
            } else {
                let result = sync::apply_for(
                    &root,
                    &backups_dir().map_err(fail)?,
                    &sync::claude_running,
                    sync::now_ms(),
                    Some(&only),
                )
                .map_err(fail)?;
                emit(&json!({"dry_run": false, "result": result}), machine);
            }
        }
        "restore" => {
            let force = take_flag(&mut args, "--force");
            let Some(ts) = args.first().cloned() else {
                return Err(plain("restore requires a backup id (see `claude backups`)"));
            };
            args.remove(0);
            no_more(&args)?;
            let result = sync::restore(
                &backups_dir().map_err(fail)?,
                &ts,
                force,
                &sync::claude_running,
            )
            .map_err(fail)?;
            emit(&json!({"restored": result}), machine);
        }
        other => return Err(plain(format!("unknown claude command {other}"))),
    }
    Ok(())
}

fn no_more(args: &[String]) -> Result<(), Failure> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(plain(format!("unexpected arguments: {}", args.join(" "))))
    }
}
