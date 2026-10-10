//! A tiny local control channel from the `pulse bridge` CLI to the running
//! Pulse hub.
//!
//! Files in the user's own Pulse state folder, so only that user can use it:
//! the CLI drops `control/requests/<id>.json` (`{id, op, args, ts}`), the hub
//! claims it by renaming it into `control/claimed/` (it is not deleted until
//! the work is answered) and writes `control/replies/<id>.json` atomically,
//! which the CLI reads and removes. A claim the hub never answered (it died
//! mid-request) is moved back by `recover_stale_claims` after 60 s, so a request
//! is processed at least once. Same on macOS and Windows; no sockets or pipes.
//!
//! The only operation today is `deliver` (args `{session, envelope,
//! reply_socket?}`): on macOS a Claude chat accepts posts only from a
//! registered peer, which the hub is (see `hub`). `peers`, `send` to other
//! computers, `status` and `inbox` need no hub.

use super::envelope::now_ms;
use super::store::Store;
use crate::localsend::proto;
use serde_json::{Value, json};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// A request nobody answered in this long is dropped.
const REQUEST_TTL_MS: u64 = 300_000;
/// The hub must claim a request within this long, else it is not listening.
const CLAIM_WITHIN: Duration = Duration::from_secs(10);
/// A claimed request with no reply after this long is given back to the queue.
const CLAIM_STALE: Duration = Duration::from_secs(60);
/// Replies nobody collected are removed after this long.
const REPLY_KEEP: Duration = Duration::from_secs(300);
/// The hub's heartbeat (written every ~2 s while its bridge runs) must be this fresh.
const HUB_FRESH_MS: u64 = 30_000;
/// A request file larger than this is discarded unread.
const MAX_REQUEST_BYTES: u64 = 1024 * 1024;

/// Errors from `call` that say the outcome is not known start with this; the request
/// may still be (or have been) carried out. Callers must not report such a send as
/// held or refused.
pub const UNKNOWN_OUTCOME: &str = "Unknown outcome";

/// Whether a `call` error means the request's outcome is not known.
pub fn is_unknown_outcome(error: &str) -> bool {
    error.starts_with(UNKNOWN_OUTCOME)
}

#[derive(Debug, Clone)]
pub struct Request {
    pub id: String,
    pub op: String,
    pub args: Value,
}

fn control_dir(store: &Store) -> PathBuf {
    store.root().join("control")
}

fn requests_dir(store: &Store) -> PathBuf {
    control_dir(store).join("requests")
}

fn claimed_dir(store: &Store) -> PathBuf {
    control_dir(store).join("claimed")
}

fn replies_dir(store: &Store) -> PathBuf {
    control_dir(store).join("replies")
}

fn private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    fs::create_dir_all(dir)?;
    Ok(())
}

/// Write `bytes` to `path` through a uniquely named temporary file, so a reader never
/// sees half a file and two writers never share a temporary.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(path);
    private_dir(dir)?;
    if let Some(parent) = dir.parent() {
        let _ = private_dir(parent);
    }
    let temp = path.with_extension(format!("tmp-{}", proto::random_hex(4)));
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let written = opts.open(&temp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    fs::rename(&temp, path).inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })
}

/// Whether the Pulse hub's bridge is running (its relay heartbeat is fresh).
pub fn hub_running(store: &Store) -> bool {
    store
        .relay_status()
        .is_some_and(|s| now_ms().saturating_sub(s.ts) < HUB_FRESH_MS)
}

/// Ask the hub to run `op` and wait up to `wait` for its reply. An `Err` that
/// `is_unknown_outcome` means the request may still have been carried out.
pub fn call(store: &Store, op: &str, args: Value, wait: Duration) -> Result<Value, String> {
    if !hub_running(store) {
        return Err("Start Pulse and try again.".to_string());
    }
    let id = format!(
        "{}-{:x}-{}",
        std::process::id(),
        now_ms(),
        proto::random_hex(3)
    );
    let request = requests_dir(store).join(format!("{id}.json"));
    let claimed = claimed_dir(store).join(format!("{id}.json"));
    let reply = replies_dir(store).join(format!("{id}.json"));
    let body = json!({"id": id, "op": op, "args": args, "ts": now_ms()});
    write_atomic(&request, body.to_string().as_bytes()).map_err(|e| e.to_string())?;
    let started = Instant::now();
    loop {
        if let Ok(bytes) = fs::read(&reply)
            && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
        {
            let _ = fs::remove_file(&reply);
            return Ok(value);
        }
        let elapsed = started.elapsed();
        if elapsed >= wait {
            // Never claimed: withdraw it. Claimed: the hub may still be working on it.
            let _ = fs::remove_file(&request);
            return Err(format!(
                "{UNKNOWN_OUTCOME}: Pulse did not answer within {} s; the request {}",
                wait.as_secs(),
                if claimed.exists() {
                    "may still be carried out."
                } else {
                    "was withdrawn before the hub took it."
                }
            ));
        }
        if elapsed >= CLAIM_WITHIN && request.exists() {
            let withdrawn = fs::remove_file(&request).is_ok();
            return Err(format!(
                "{UNKNOWN_OUTCOME}: the Pulse hub did not pick the request up within {} s ({}).",
                CLAIM_WITHIN.as_secs(),
                if withdrawn {
                    "it was withdrawn, so nothing was done"
                } else {
                    "the hub took it at the last moment and may still carry it out"
                }
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Hub side: claim every waiting request by renaming it into `claimed/` (the claim
/// stays until `reply` answers it, or `recover_stale_claims` gives it back).
pub fn take_requests(store: &Store) -> Vec<Request> {
    let Ok(listing) = fs::read_dir(requests_dir(store)) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = listing
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let claims = claimed_dir(store);
    if !files.is_empty() && private_dir(&claims).is_err() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for file in files {
        let Some(name) = file.file_name() else {
            continue;
        };
        let claim = claims.join(name);
        // The rename is the claim: only one claimer wins, and the CLI's own
        // withdrawal (a delete) loses cleanly to it or beats it.
        if fs::rename(&file, &claim).is_err() {
            continue;
        }
        if let Ok(opened) = fs::OpenOptions::new().write(true).open(&claim) {
            let _ = opened.set_modified(SystemTime::now());
        }
        let oversize = fs::metadata(&claim).is_ok_and(|m| m.len() > MAX_REQUEST_BYTES);
        let value: Option<Value> = if oversize {
            None
        } else {
            fs::read(&claim)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
        };
        let id = value
            .as_ref()
            .and_then(|v| v["id"].as_str())
            .unwrap_or("")
            .to_string();
        let stem_matches = claim
            .file_stem()
            .is_some_and(|s| s.to_string_lossy() == id.as_str());
        let safe = !id.is_empty()
            && stem_matches
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        let fresh = value.as_ref().is_some_and(|v| {
            v["ts"]
                .as_u64()
                .is_some_and(|ts| now_ms().saturating_sub(ts) < REQUEST_TTL_MS)
        });
        let Some(value) = value.filter(|_| safe && fresh) else {
            let _ = fs::remove_file(&claim);
            continue;
        };
        out.push(Request {
            id,
            op: value["op"].as_str().unwrap_or("").to_string(),
            args: value["args"].clone(),
        });
    }
    out
}

/// Hub side: answer a claimed request (the reply is written atomically, then the claim
/// is released).
pub fn reply(store: &Store, id: &str, value: &Value) {
    let path = replies_dir(store).join(format!("{id}.json"));
    let _ = write_atomic(&path, value.to_string().as_bytes());
    let _ = fs::remove_file(claimed_dir(store).join(format!("{id}.json")));
}

fn older_than(path: &Path, limit: Duration) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age > limit)
}

/// Hub side, once per tick: give back every request claimed more than 60 s ago that
/// has no reply (the hub that claimed it died or hung), drop claims that are answered
/// or past their time to live, and sweep replies nobody collected.
pub fn recover_stale_claims(store: &Store) {
    if let Ok(listing) = fs::read_dir(claimed_dir(store)) {
        for entry in listing.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|x| x != "json") || !older_than(&path, CLAIM_STALE) {
                continue;
            }
            let Some(name) = path.file_name() else {
                continue;
            };
            let answered = replies_dir(store).join(name).exists();
            let expired = fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .and_then(|v| v["ts"].as_u64())
                .is_none_or(|ts| now_ms().saturating_sub(ts) >= REQUEST_TTL_MS);
            if answered || expired {
                let _ = fs::remove_file(&path);
            } else {
                let _ = fs::rename(&path, requests_dir(store).join(name));
            }
        }
    }
    if let Ok(listing) = fs::read_dir(replies_dir(store)) {
        for entry in listing.flatten() {
            if older_than(&entry.path(), REPLY_KEEP) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}
