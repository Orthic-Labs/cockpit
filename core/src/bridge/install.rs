//! `pulse bridge install`: put the Pulse bridge skill where Claude and Codex
//! load skills, so a chat knows how to message chats on other computers.
//!
//! * Claude: `<CLAUDE_CONFIG_DIR or ~/.claude>/skills/pulse-bridge/SKILL.md`
//! * Codex: `<CODEX_HOME or ~/.codex>/skills/pulse-bridge/SKILL.md`
//!
//! The same file ships as the plugin `plugins/pulse-bridge` in the repo. A
//! target whose config folder does not exist is skipped. Writes are atomic
//! (temporary file renamed into place), idempotent, and a different file
//! already there is kept once as `SKILL.md.pulse-bak`; uninstall reverses it.

use super::deliver_claude::sessions_dir;
use super::deliver_codex::codex_home;
use serde::Serialize;
use std::path::{Path, PathBuf};

const SKILL: &str = include_str!("../../../plugins/pulse-bridge/skills/pulse-bridge/SKILL.md");
const BACKUP: &str = "SKILL.md.pulse-bak";

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub claude: bool,
    pub codex: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub target: String,
    pub path: String,
    /// installed, unchanged, removed, absent or skipped.
    pub action: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub dry_run: bool,
    pub changes: Vec<Change>,
}

struct Target {
    name: &'static str,
    /// The config folder that must exist (`~/.claude`, `~/.codex`).
    root: PathBuf,
    skill: PathBuf,
}

fn targets(options: &Options) -> Vec<Target> {
    let all = !options.claude && !options.codex;
    let mut list = Vec::new();
    if all || options.claude {
        // `sessions_dir()` is `<claude config>/sessions`.
        let root = sessions_dir()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        list.push(Target {
            name: "claude",
            skill: root.join("skills").join("pulse-bridge").join("SKILL.md"),
            root,
        });
    }
    if all || options.codex {
        let root = codex_home();
        list.push(Target {
            name: "codex",
            skill: root.join("skills").join("pulse-bridge").join("SKILL.md"),
            root,
        });
    }
    list
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("no folder")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let temp = dir.join("SKILL.md.pulse-tmp");
    std::fs::write(&temp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&temp, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        e.to_string()
    })
}

fn install_one(t: &Target, dry: bool) -> Result<(&'static str, String), String> {
    if !t.root.is_dir() {
        return Ok(("skipped", format!("{} isn't set up here", t.root.display())));
    }
    let existing = std::fs::read_to_string(&t.skill).ok();
    if existing.as_deref() == Some(SKILL) {
        return Ok(("unchanged", "already installed".into()));
    }
    if dry {
        return Ok(("installed", "dry run: nothing written".into()));
    }
    let mut note = String::from("restart open chats to see it");
    if existing.is_some() {
        let backup = t.skill.with_file_name(BACKUP);
        if !backup.exists() {
            std::fs::copy(&t.skill, &backup).map_err(|e| e.to_string())?;
            note = format!("kept the previous file as {BACKUP}; {note}");
        }
    }
    write_atomic(&t.skill, SKILL)?;
    Ok(("installed", note))
}

fn uninstall_one(t: &Target, dry: bool) -> Result<(&'static str, String), String> {
    let existing = std::fs::read_to_string(&t.skill).ok();
    let backup = t.skill.with_file_name(BACKUP);
    if existing.as_deref() != Some(SKILL) {
        return Ok(("absent", "the Pulse skill isn't installed".into()));
    }
    if dry {
        return Ok(("removed", "dry run: nothing changed".into()));
    }
    if backup.exists() {
        std::fs::rename(&backup, &t.skill).map_err(|e| e.to_string())?;
        return Ok(("removed", "restored the previous file".into()));
    }
    std::fs::remove_file(&t.skill).map_err(|e| e.to_string())?;
    if let Some(dir) = t.skill.parent() {
        let _ = std::fs::remove_dir(dir);
    }
    Ok(("removed", String::new()))
}

/// Install or remove the skill for the chosen targets (both when none chosen).
pub fn apply(uninstall: bool, options: &Options) -> Result<Report, String> {
    let mut changes = Vec::new();
    for t in targets(options) {
        let (action, note) = if uninstall {
            uninstall_one(&t, options.dry_run)
        } else {
            install_one(&t, options.dry_run)
        }
        .map_err(|e| format!("{}: {e}", t.name))?;
        changes.push(Change {
            target: t.name.to_string(),
            path: t.skill.display().to_string(),
            action: action.to_string(),
            note,
        });
    }
    Ok(Report {
        dry_run: options.dry_run,
        changes,
    })
}

/// `pulse bridge install|uninstall [--claude] [--codex] [--dry-run]`.
pub fn run(subcommand: &str, args: Vec<String>, machine: bool) -> Result<(), String> {
    let mut options = Options::default();
    for arg in &args {
        match arg.as_str() {
            "--claude" => options.claude = true,
            "--codex" => options.codex = true,
            "--dry-run" => options.dry_run = true,
            other => return Err(format!("unknown option: {other}")),
        }
    }
    let report = apply(subcommand == "uninstall", &options)?;
    if machine {
        println!("{}", serde_json::json!(report));
    } else {
        for c in &report.changes {
            println!("{}: {} {} {}", c.target, c.action, c.path, c.note);
        }
    }
    Ok(())
}
