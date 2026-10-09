//! `pulse usage`: the provider readings the Pulse notch has published, read
//! back without the notch's help. The notch (Mac and Windows) writes its
//! accounts, with each limit window's used fraction, to `notch-state.json` in
//! Pulse's state folder. This reads that file only: it never reads a provider
//! credential, session or log, and never asks a provider anything.
//!
//! The notch rewrites the file when a reading changes, so the file's modified
//! time says when the numbers last changed, not that they are current. Whether
//! the notch is running is checked separately and a reading from a notch that
//! is not running is marked `stale`.

use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const FILE: &str = "notch-state.json";

fn state_file() -> Option<PathBuf> {
    #[cfg(windows)]
    let root = std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("Pulse"));
    #[cfg(not(windows))]
    let root = std::env::var_os("HOME")
        .map(|p| PathBuf::from(p).join("Library/Application Support/Pulse"));
    Some(root?.join(FILE))
}

/// Epoch seconds as `YYYY-MM-DDTHH:MM:SSZ` (Howard Hinnant's civil-from-days).
fn rfc3339(epoch: u64) -> String {
    let days = (epoch / 86_400) as i64;
    let rest = epoch % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

/// Whether the notch process is running: Some(true) when one is seen, Some(false)
/// when no process of that name exists, None when it cannot be told (a process of
/// that name whose program file cannot be read). The `pulse` command line tool has
/// the same name on case-insensitive systems, so this process and anything in a
/// `Helpers` folder are not counted.
fn notch_running() -> Option<bool> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet),
    );
    let me = std::process::id();
    let mut unknown = false;
    for process in system.processes().values() {
        if process.pid().as_u32() == me {
            continue;
        }
        let name = process.name().to_string_lossy().to_lowercase();
        if name != "pulse" && name != "pulse.exe" {
            continue;
        }
        match process.exe() {
            Some(exe) => {
                let in_helpers = exe
                    .parent()
                    .and_then(|p| p.file_name())
                    .is_some_and(|n| n.eq_ignore_ascii_case("Helpers"));
                if !in_helpers {
                    return Some(true);
                }
            }
            None => unknown = true,
        }
    }
    if unknown { None } else { Some(false) }
}

fn gone(reason: &str) -> Value {
    let provider = json!({"value": null, "state": "unavailable", "source": null,
                          "observed_at": null, "reason": reason});
    json!({"claude": provider, "codex": provider, "reason": reason})
}

fn limits(account: &Value) -> Vec<Value> {
    account["limits"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let used = row["usedFraction"].as_f64().filter(|f| f.is_finite())?;
                    let mut out = json!({
                        "label": row["label"].as_str().unwrap_or("limit"),
                        "used_fraction": used,
                        "used_percent": (used * 1000.0).round() / 10.0,
                    });
                    if let Some(seconds) = row["seconds"].as_f64() {
                        out["window_seconds"] = json!(seconds);
                    }
                    Some(out)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn provider(snapshot: &Value, id: &str, observed: &Value, running: Option<bool>) -> Value {
    let Some(account) = snapshot["accounts"]
        .as_array()
        .and_then(|rows| rows.iter().find(|a| a["id"] == id))
    else {
        return json!({"value": null, "state": "unavailable", "source": FILE,
                      "observed_at": observed,
                      "reason": "the notch's snapshot has no such account"});
    };
    let rows = limits(account);
    let summary = account["summary"].as_str().unwrap_or("");
    let (mut state, reason) = if account["connected"].as_bool() == Some(false) {
        ("hidden", "the provider is switched off in the notch")
    } else if account["needsRenewal"].as_bool() == Some(true) {
        (
            "needs_sign_in",
            "sign-in expired; use the provider's app once to refresh",
        )
    } else if account["refusedAccess"].as_bool() == Some(true) {
        (
            "access_denied",
            "the notch was refused access to the provider's sign-in",
        )
    } else if rows.is_empty() {
        (
            "no_reading",
            "the notch has no limit reading for this provider yet",
        )
    } else {
        ("ok", "")
    };
    let mut reason = reason.to_string();
    if running == Some(false) {
        // What follows is the last thing a notch wrote before it closed.
        state = "stale";
        reason = "the notch is not running; this is its last published reading".into();
    }
    let value = if rows.is_empty() {
        Value::Null
    } else {
        json!({"limits": rows, "plan": account["plan"]})
    };
    let mut out = json!({"value": value, "state": state, "source": FILE,
                         "observed_at": observed, "summary": summary});
    if !reason.is_empty() {
        out["reason"] = json!(reason);
    }
    out
}

/// The notch's published readings for Claude and Codex, as JSON.
pub fn read() -> Value {
    let Some(path) = state_file() else {
        return gone("this user's Pulse state folder is unknown; the notch isn't running");
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return gone(
                "the Pulse notch isn't running (it has not published notch-state.json); open Pulse and try again",
            );
        }
        Err(e) => return gone(&format!("couldn't read the notch's snapshot: {e}")),
    };
    let snapshot: Value = match serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) {
        Ok(v) if v["product"] == "Pulse" => v,
        _ => return gone("the notch's snapshot isn't readable (not a Pulse notch-state file)"),
    };
    let modified = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    let age = modified.and_then(|m| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|now| now.as_secs().saturating_sub(m))
    });
    let observed = modified.map_or(Value::Null, |m| json!(rfc3339(m)));
    let running = notch_running();
    let mut out = json!({
        "claude": provider(&snapshot, "claude", &observed, running),
        "codex": provider(&snapshot, "codex", &observed, running),
        "snapshot": {
            "source": FILE,
            "published_at": observed,
            "age_seconds": age,
            "notch_version": snapshot["version"],
            "platform": snapshot["platform"],
            "notch_running": running,
        },
    });
    if running == Some(false) {
        out["reason"] = json!(
            "the Pulse notch isn't running; readings are from its last published snapshot (the notch rewrites it when a reading changes, so published_at is when they last changed)"
        );
    }
    out
}
