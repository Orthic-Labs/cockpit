//! App icons on Windows, as PNG.
//!
//! A source is a program/icon/library file or a PNG (Store packages). PNG sources
//! are used as they are. For the rest, one hidden Windows PowerShell run per batch
//! asks `System.Drawing.Icon.ExtractAssociatedIcon` (the shell's own icon for the
//! file, the same one Explorer shows) and saves each as a PNG. Results are cached
//! as `<hash>.png` in the folder the caller gives, keyed by the source path, size
//! and modification time, so an updated program gets a fresh icon and nothing is
//! extracted twice. An app with no source, or a file the shell cannot read, has no
//! icon here and the page keeps its neutral one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use super::run_capture;

const EXTRACT_TIMEOUT: Duration = Duration::from_secs(120);
/// Icons larger than this are not read (a PNG source is arbitrary package data).
const MAX_PNG_BYTES: u64 = 512 * 1024;

const SCRIPT: &str = concat!(
    "$ErrorActionPreference='SilentlyContinue';",
    "Add-Type -AssemblyName System.Drawing;",
    "foreach($l in [IO.File]::ReadAllLines($env:PULSE_ICON_LIST)){",
    "$f=$l.Split([char]9);",
    "try{$i=[System.Drawing.Icon]::ExtractAssociatedIcon($f[1]);",
    "if($i){$b=$i.ToBitmap();$b.Save($f[0],[System.Drawing.Imaging.ImageFormat]::Png);$b.Dispose();$i.Dispose()}}catch{}}",
);

fn cache_name(source: &str) -> String {
    let meta = std::fs::metadata(source).ok();
    let size = meta.as_ref().map_or(0, std::fs::Metadata::len);
    let modified = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    // FNV-1a 64.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in format!("{}|{size}|{modified}", source.to_lowercase()).bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}.png")
}

fn is_png(source: &str) -> bool {
    Path::new(source)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("png"))
}

/// PNG bytes for every `(key, source)` that has an icon. With `extract` false only
/// PNG sources and already-cached icons are returned, so it is cheap; with it true
/// the missing ones are extracted first (one PowerShell run).
pub fn load(sources: &[(String, String)], dir: &Path, extract: bool) -> HashMap<String, Vec<u8>> {
    let mut out = HashMap::new();
    let mut pending: Vec<(String, String, PathBuf)> = Vec::new();
    for (key, source) in sources {
        if is_png(source) {
            if std::fs::metadata(source).is_ok_and(|m| m.is_file() && m.len() <= MAX_PNG_BYTES)
                && let Ok(bytes) = std::fs::read(source)
            {
                out.insert(key.clone(), bytes);
            }
            continue;
        }
        let target = dir.join(cache_name(source));
        match std::fs::read(&target) {
            Ok(bytes) if !bytes.is_empty() => {
                out.insert(key.clone(), bytes);
            }
            _ => pending.push((key.clone(), source.clone(), target)),
        }
    }
    if !extract || pending.is_empty() || std::fs::create_dir_all(dir).is_err() {
        return out;
    }
    // Several apps may share one program; extract it once.
    let mut list = String::new();
    let mut listed = std::collections::HashSet::new();
    for (_, source, target) in &pending {
        if listed.insert(target.clone()) && !source.contains(['\t', '\n', '\r']) {
            list.push_str(&format!("{}\t{}\n", target.display(), source));
        }
    }
    let list_file = dir.join("pending.tsv");
    if std::fs::write(&list_file, list).is_err() {
        return out;
    }
    let mut command = Command::new(super::powershell());
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .env("PULSE_ICON_LIST", &list_file);
    let _ = run_capture(
        command,
        EXTRACT_TIMEOUT,
        "PowerShell",
        "Windows PowerShell is not available.",
    );
    let _ = std::fs::remove_file(&list_file);
    for (key, _, target) in pending {
        if let Ok(bytes) = std::fs::read(&target)
            && !bytes.is_empty()
        {
            out.insert(key, bytes);
        }
    }
    out
}
