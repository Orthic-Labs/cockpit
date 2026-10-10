//! `pulse bridge install`: put the Pulse bridge skill where Claude and Codex
//! load skills, so a chat knows how to message chats on other computers.
//!
//! * Claude: `<CLAUDE_CONFIG_DIR or ~/.claude>/skills/pulse-bridge/SKILL.md`
//! * Codex: `<CODEX_HOME or ~/.codex>/skills/pulse-bridge/SKILL.md`
//!
//! The same file ships as the plugin `plugins/pulse-bridge` in the repo. A
//! target whose config folder does not exist is skipped. Writes are atomic
//! (uniquely named temporary file renamed into place) and idempotent. Each target keeps
//! `.pulse-bridge.json` (version, content hash, install time). A file that differs from both
//! that record and the new content is first copied to `SKILL.md.bak-<timestamp>`; uninstall
//! removes only a file whose hash matches the record and otherwise leaves it in place.

use super::deliver_claude::sessions_dir;
use super::deliver_codex::codex_home;
use serde::Serialize;
use std::path::{Path, PathBuf};

const SKILL: &str = include_str!("../../../plugins/pulse-bridge/skills/pulse-bridge/SKILL.md");
const LEGACY_BACKUP: &str = "SKILL.md.pulse-bak";
const MANIFEST: &str = ".pulse-bridge.json";

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
    /// changed, unchanged, left (modified, kept), absent, skipped or failed.
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
            skill: skills_dir(&root).join("pulse-bridge").join("SKILL.md"),
            root,
        });
    }
    if all || options.codex {
        let root = codex_home();
        list.push(Target {
            name: "codex",
            skill: skills_dir(&root).join("pulse-bridge").join("SKILL.md"),
            root,
        });
    }
    list
}

/// `<root>/skills`, resolved through a junction or symlink when it is one: Windows refuses
/// to create files across an "untrusted mount point" (a `.codex\skills` junction to
/// `.claude\skills` is common), so the real folder is written instead.
fn skills_dir(root: &Path) -> PathBuf {
    let dir = root.join("skills");
    match std::fs::symlink_metadata(&dir) {
        Ok(m) if m.file_type().is_symlink() || m.file_type().is_dir() => {
            std::fs::canonicalize(&dir).unwrap_or(dir)
        }
        _ => dir,
    }
}

/// FNV-1a 64 of the text, as 16 hex digits (an ownership check, not a security boundary).
fn hash(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        h = (h ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

fn manifest_path(t: &Target) -> PathBuf {
    t.skill.with_file_name(MANIFEST)
}

/// The content hash recorded at the last install, if any.
fn recorded_hash(t: &Target) -> Option<String> {
    let text = std::fs::read_to_string(manifest_path(t)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value["hash"].as_str().map(String::from)
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("no folder")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let temp = dir.join(format!(
        "{name}.pulse-tmp-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::write(&temp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&temp, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        e.to_string()
    })
}

fn write_manifest(t: &Target) -> Result<(), String> {
    let record = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "hash": hash(SKILL),
        "installedAt": now_ms(),
    });
    write_atomic(&manifest_path(t), &record.to_string())
}

/// A backup name that does not exist yet.
fn backup_path(t: &Target) -> PathBuf {
    let stamp = now_ms();
    let first = t.skill.with_file_name(format!("SKILL.md.bak-{stamp}"));
    if !first.exists() {
        return first;
    }
    (1..)
        .map(|n| t.skill.with_file_name(format!("SKILL.md.bak-{stamp}-{n}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

fn install_one(t: &Target, dry: bool) -> Result<(&'static str, String), String> {
    if !t.root.is_dir() {
        return Ok(("skipped", format!("{} isn't set up here", t.root.display())));
    }
    let existing = std::fs::read_to_string(&t.skill).ok();
    if existing.as_deref() == Some(SKILL) {
        if !dry && recorded_hash(t).as_deref() != Some(&hash(SKILL)) {
            write_manifest(t)?;
        }
        return Ok(("unchanged", "already installed".into()));
    }
    if dry {
        return Ok(("changed", "dry run: nothing written".into()));
    }
    let mut note = String::from("restart open chats to see it");
    if let Some(old) = &existing {
        let h = hash(old);
        if recorded_hash(t).as_deref() != Some(&h) && h != hash(SKILL) {
            let backup = backup_path(t);
            std::fs::copy(&t.skill, &backup).map_err(|e| e.to_string())?;
            let name = backup
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            note = format!("kept the previous file as {name}; {note}");
        }
    }
    write_atomic(&t.skill, SKILL)?;
    write_manifest(t)?;
    Ok(("changed", note))
}

fn uninstall_one(t: &Target, dry: bool) -> Result<(&'static str, String), String> {
    let Some(existing) = std::fs::read_to_string(&t.skill).ok() else {
        return Ok(("absent", "the Pulse skill isn't installed".into()));
    };
    let h = hash(&existing);
    let owned = match recorded_hash(t) {
        Some(recorded) => recorded == h,
        None => existing == SKILL,
    };
    if !owned {
        return Ok(("left", "left in place: modified".into()));
    }
    if dry {
        return Ok(("changed", "dry run: nothing changed".into()));
    }
    let legacy = t.skill.with_file_name(LEGACY_BACKUP);
    if legacy.exists() {
        std::fs::rename(&legacy, &t.skill).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(manifest_path(t));
        return Ok(("changed", "restored the previous file".into()));
    }
    std::fs::remove_file(&t.skill).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(manifest_path(t));
    if let Some(dir) = t.skill.parent() {
        let _ = std::fs::remove_dir(dir);
    }
    Ok(("changed", "removed".into()))
}

/// Install or remove the skill for the chosen targets (both when none chosen). A failing
/// target is reported as `failed` and the others still run.
pub fn apply(uninstall: bool, options: &Options) -> Result<Report, String> {
    let mut changes = Vec::new();
    for t in targets(options) {
        let result = if uninstall {
            uninstall_one(&t, options.dry_run)
        } else {
            install_one(&t, options.dry_run)
        };
        let (action, note) = match result {
            Ok((action, note)) => (action, note),
            Err(e) => ("failed", e),
        };
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
    let failed = report
        .changes
        .iter()
        .filter(|c| c.action == "failed")
        .count();
    if failed > 0 {
        return Err(format!("{failed} target(s) failed; see above"));
    }
    Ok(())
}
