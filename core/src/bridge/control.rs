//! A tiny local control channel from the `pulse bridge` CLI to the running
//! Pulse hub, which owns the sharing service (and its port).
//!
//! Files in the user's own Pulse state folder, so only that user can use it:
//! the CLI drops `control/requests/<id>.json` (`{id, op, args, ts}`), the hub
//! claims it (deletes the file) and writes `control/replies/<id>.json`, which
//! the CLI reads and removes. Same on macOS and Windows; no sockets or pipes.
//!
//! The only operation today is `pair` (args `{device}`): pairing needs the
//! hub's sharing service. `peers`, `send`, `status` and `inbox` need no
//! service and work from the store, with the hub relaying (see `send_text`).

use super::envelope::now_ms;
use super::store::Store;
use serde_json::{Value, json};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A request nobody claimed in this long is dropped.
const REQUEST_TTL_MS: u64 = 300_000;
/// The hub must claim a request within this long, else it is not listening.
const CLAIM_WITHIN: Duration = Duration::from_secs(10);
/// The relay heartbeat (written every ~2 s while the hub's bridge runs) must be this fresh.
const HUB_FRESH_MS: u64 = 30_000;

#[derive(Debug, Clone)]
pub struct Request {
    pub id: String,
    pub op: String,
    pub args: Value,
}

fn requests_dir(store: &Store) -> PathBuf {
    store.root().join("control").join("requests")
}

fn replies_dir(store: &Store) -> PathBuf {
    store.root().join("control").join("replies")
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(path);
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    let temp = path.with_extension("tmp");
    fs::write(&temp, bytes)?;
    fs::rename(&temp, path)
}

/// Whether the Pulse hub's bridge is running (its relay heartbeat is fresh).
pub fn hub_running(store: &Store) -> bool {
    store
        .relay_status()
        .is_some_and(|s| now_ms().saturating_sub(s.ts) < HUB_FRESH_MS)
}

/// Ask the hub to run `op` and wait up to `wait` for its reply.
pub fn call(store: &Store, op: &str, args: Value, wait: Duration) -> Result<Value, String> {
    if !hub_running(store) {
        return Err("Start Pulse (the hub runs nearby sharing) and try again.".to_string());
    }
    let id = format!("{}-{:x}", std::process::id(), now_ms());
    let request = requests_dir(store).join(format!("{id}.json"));
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
            let _ = fs::remove_file(&request);
            return Err("Pulse did not answer in time.".to_string());
        }
        if elapsed >= CLAIM_WITHIN && request.exists() {
            let _ = fs::remove_file(&request);
            return Err("Pulse is not responding; is the hub running?".to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Hub side: claim every waiting request (removing the files).
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
    let mut out = Vec::new();
    for file in files {
        let value: Option<Value> = fs::read(&file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        let _ = fs::remove_file(&file);
        let Some(value) = value else { continue };
        let fresh = value["ts"]
            .as_u64()
            .is_some_and(|ts| now_ms().saturating_sub(ts) < REQUEST_TTL_MS);
        let id = value["id"].as_str().unwrap_or("");
        let safe = !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if fresh && safe {
            out.push(Request {
                id: id.to_string(),
                op: value["op"].as_str().unwrap_or("").to_string(),
                args: value["args"].clone(),
            });
        }
    }
    out
}

/// Hub side: answer a claimed request.
pub fn reply(store: &Store, id: &str, value: &Value) {
    let path = replies_dir(store).join(format!("{id}.json"));
    let _ = write_atomic(&path, value.to_string().as_bytes());
}
