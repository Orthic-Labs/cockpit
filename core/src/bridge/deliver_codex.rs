//! Push a bridged message into a Codex thread on this computer, as far as the
//! bundled Codex CLI allows.
//!
//! Prototype: `codex queue --thread <id> --message <text>` ("Queue a message
//! for an existing session", present in the Codex CLI bundled with the ChatGPT
//! app). It runs with a 5 s limit and no terminal or console window. If it
//! can't be found, fails or times out the receipt is `unsupported` with the
//! reason, and the message simply stays in the MCP inbox for the thread to pick
//! up through `bridge_inbox`.
//!
//! Where the CLI is looked for:
//! * macOS: `/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex`
//!   (verified), then `/opt/homebrew/bin/codex`, `/usr/local/bin/codex`, `codex` on PATH.
//! * Windows (not verified on a real install; best guesses): `codex.exe` /
//!   `codex.cmd` on PATH, `%LOCALAPPDATA%\Programs\ChatGPT\resources\codex-cli\bin\codex.exe`,
//!   `%LOCALAPPDATA%\Programs\Codex\resources\codex-cli\bin\codex.exe`,
//!   `%LOCALAPPDATA%\OpenAI\Codex\bin\codex.exe`. A packaged (WindowsApps)
//!   install is not readable and is not searched.
//!
//! Thread discovery: `~/.codex/session_index.jsonl` (one `{id, thread_name,
//! updated_at}` per line, read-only). It lists threads, not which are open, so
//! `list_threads` is only a hint; the bridge roster should list a Codex thread
//! as reachable only when it has called the Pulse MCP (`bridge_whoami`).

use super::deliver_claude::shim;
use super::{BridgeError, Envelope, LocalSession, Receipt};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const QUEUE_TIMEOUT: Duration = Duration::from_secs(5);
const INDEX_TAIL_BYTES: u64 = 512 * 1024;

fn home() -> PathBuf {
    #[cfg(windows)]
    let variable = "USERPROFILE";
    #[cfg(not(windows))]
    let variable = "HOME";
    std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `~/.codex`, or `$CODEX_HOME`.
pub fn codex_home() -> PathBuf {
    match std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home().join(".codex"),
    }
}

/// The Codex CLI, if this computer has one.
pub fn codex_binary() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        candidates.push(PathBuf::from(
            "/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex",
        ));
        candidates.push(PathBuf::from("/opt/homebrew/bin/codex"));
        candidates.push(PathBuf::from("/usr/local/bin/codex"));
    }
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            candidates.push(local.join(r"Programs\ChatGPT\resources\codex-cli\bin\codex.exe"));
            candidates.push(local.join(r"Programs\Codex\resources\codex-cli\bin\codex.exe"));
            candidates.push(local.join(r"OpenAI\Codex\bin\codex.exe"));
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let names: &[&str] = if cfg!(windows) {
                &["codex.exe", "codex.cmd"]
            } else {
                &["codex"]
            };
            for name in names {
                candidates.push(dir.join(name));
            }
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// A Codex thread listed in the session index.
#[derive(Clone, Debug)]
pub struct CodexThread {
    pub id: String,
    pub name: String,
    pub updated_at: String,
}

/// The most recently updated threads (newest first, at most `limit`).
pub fn list_threads(limit: usize) -> Vec<CodexThread> {
    let path = codex_home().join("session_index.jsonl");
    let Ok(mut file) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    let length = file.metadata().map(|m| m.len()).unwrap_or(0);
    if length > INDEX_TAIL_BYTES {
        use std::io::{Seek, SeekFrom};
        if file
            .seek(SeekFrom::Start(length - INDEX_TAIL_BYTES))
            .is_err()
        {
            return Vec::new();
        }
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut latest: std::collections::HashMap<String, CodexThread> =
        std::collections::HashMap::new();
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(id) = entry["id"].as_str().filter(|i| !i.is_empty()) else {
            continue;
        };
        let thread = CodexThread {
            id: id.to_string(),
            name: entry["thread_name"].as_str().unwrap_or("").to_string(),
            updated_at: entry["updated_at"].as_str().unwrap_or("").to_string(),
        };
        let newer = latest
            .get(id)
            .is_none_or(|old| thread.updated_at >= old.updated_at);
        if newer {
            latest.insert(id.to_string(), thread);
        }
    }
    let mut threads: Vec<CodexThread> = latest.into_values().collect();
    threads.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    threads.truncate(limit);
    threads
}

fn run_queue(binary: &Path, thread: &str, message: &str) -> Result<(), String> {
    let mut command = Command::new(binary);
    command
        .args(["queue", "--thread", thread, "--message", message])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Couldn't start Codex: {}", e.kind()))?;
    let deadline = Instant::now() + QUEUE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let mut detail = String::new();
                if let Some(mut err) = child.stderr.take() {
                    let _ = err.by_ref().take(2000).read_to_string(&mut detail);
                }
                let detail: String = detail
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(200)
                    .collect();
                return Err(if detail.is_empty() {
                    format!("Codex couldn't queue the message ({status}).")
                } else {
                    format!("Codex couldn't queue the message: {detail}")
                });
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Codex didn't answer within 5 seconds.".to_string());
            }
            Err(e) => return Err(format!("Couldn't watch Codex: {}", e.kind())),
        }
    }
}

/// Try to queue the message into the Codex thread. Never an `Err` for "can't
/// push": that is `Receipt::unsupported`, and the message stays in the inbox.
pub fn deliver(session: &LocalSession, env: &Envelope) -> Result<Receipt, BridgeError> {
    let Some(binary) = codex_binary() else {
        return Ok(shim::unsupported(
            "Codex isn't installed here, so the message waits in the inbox.",
        ));
    };
    let thread = session.id.as_str();
    if thread.is_empty() {
        return Ok(shim::unsupported(
            "This Codex chat has no thread id, so the message waits in the inbox.",
        ));
    }
    let text = format!(
        "[Message from {} via Pulse]\n{}",
        shim::env_from_name(env),
        shim::env_text(env)
    );
    Ok(match run_queue(&binary, thread, &text) {
        Ok(()) => shim::delivered("Queued in the Codex thread."),
        Err(reason) => shim::unsupported(&format!("{reason} The message waits in the inbox.")),
    })
}
