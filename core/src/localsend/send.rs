//! Sending: `prepare-upload` to a peer, then each accepted file as an upload.

use super::net::{self, Wire};
use super::proto::{
    self, DeviceInfo, FileMeta, PrepareUploadRequest, PrepareUploadResponse, PulseExtra,
};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{IpAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Where to send: a device found on the network.
#[derive(Clone, Debug)]
pub struct Peer {
    pub ip: IpAddr,
    pub port: u16,
    pub https: bool,
    pub fingerprint: String,
    pub alias: String,
}

/// What the user chose to send.
#[derive(Clone, Debug)]
pub enum SendItem {
    Path(PathBuf),
    Text(String),
}

#[derive(Clone, Debug)]
enum Source {
    File(PathBuf),
    Text(String),
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: String,
    /// Name as the receiver sees it; folders keep their structure ("Dir/a.txt").
    pub name: String,
    pub size: u64,
    pub mime: String,
    source: Source,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Waiting for the other device to accept.
    Waiting,
    Sending,
    /// A message (text only) is on the other device's screen. No longer
    /// reported early: a message waits for the other side's answer like a file.
    Delivered,
}

#[derive(Clone, Debug)]
pub struct Progress {
    pub phase: Phase,
    pub done: u64,
    pub total: u64,
    pub files_done: usize,
    pub files_total: usize,
    pub current: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Declined,
    Cancelled,
}

const MAX_ENTRIES: usize = 20_000;

/// The files behind the user's choice. Folders are walked (symbolic links
/// inside them are skipped); a chosen path that is a link is followed.
pub fn expand(items: &[SendItem]) -> Result<Vec<Entry>, String> {
    let mut entries: Vec<Entry> = Vec::new();
    for item in items {
        match item {
            SendItem::Text(text) => {
                if text.is_empty() {
                    continue;
                }
                entries.push(Entry {
                    id: format!("f{}", entries.len()),
                    name: format!("{}.txt", proto::random_hex(4)),
                    size: text.len() as u64,
                    mime: "text/plain".to_string(),
                    source: Source::Text(text.clone()),
                });
            }
            SendItem::Path(path) => {
                let metadata = std::fs::metadata(path)
                    .map_err(|e| format!("Can't read {}: {e}", path.display()))?;
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "file".to_string());
                if metadata.is_dir() {
                    walk(path, &name, &mut entries)?;
                } else if metadata.is_file() {
                    push_file(&mut entries, path, name, metadata.len());
                } else {
                    return Err(format!("{} isn't a file or folder.", path.display()));
                }
            }
        }
        if entries.len() > MAX_ENTRIES {
            return Err(format!("That's more than {MAX_ENTRIES} files."));
        }
    }
    if entries.is_empty() {
        return Err("There is nothing to send.".to_string());
    }
    Ok(entries)
}

fn push_file(entries: &mut Vec<Entry>, path: &Path, name: String, size: u64) {
    let mime = proto::mime_for(&name).to_string();
    entries.push(Entry {
        id: format!("f{}", entries.len()),
        name,
        size,
        mime,
        source: Source::File(path.to_path_buf()),
    });
}

fn walk(directory: &Path, prefix: &str, entries: &mut Vec<Entry>) -> Result<(), String> {
    let mut stack = vec![(directory.to_path_buf(), prefix.to_string())];
    while let Some((dir, relative)) = stack.pop() {
        let listing =
            std::fs::read_dir(&dir).map_err(|e| format!("Can't read {}: {e}", dir.display()))?;
        for child in listing.flatten() {
            let Ok(kind) = child.file_type() else {
                continue;
            };
            let name = format!("{relative}/{}", child.file_name().to_string_lossy());
            if kind.is_symlink() {
                continue;
            } else if kind.is_dir() {
                stack.push((child.path(), name));
            } else if kind.is_file() {
                let size = child.metadata().map(|m| m.len()).unwrap_or(0);
                push_file(entries, &child.path(), name, size);
            }
            if entries.len() > MAX_ENTRIES {
                return Err(format!("That's more than {MAX_ENTRIES} files."));
            }
        }
    }
    Ok(())
}

fn exchange(
    peer: &Peer,
    target: &str,
    json: Option<&[u8]>,
    timeout: Duration,
    register: &dyn Fn(Option<TcpStream>),
    sent: &mut dyn FnMut(),
) -> Result<net::Reply, String> {
    let mut wire = net::connect(peer.ip, peer.port, peer.https, &peer.fingerprint, timeout)
        .map_err(|e| format!("Couldn't reach {}: {e}", peer.alias))?;
    register(wire.tcp().try_clone().ok());
    let payload = json.unwrap_or(&[]);
    let host = format!("{}:{}", peer.ip, peer.port);
    let result = net::call(
        &mut wire,
        "POST",
        &host,
        target,
        json.map(|_| "application/json"),
        payload.len() as u64,
        &mut |w| {
            w.write_all(payload)?;
            w.flush()?;
            sent();
            Ok(())
        },
    );
    wire.finish();
    register(None);
    result.map_err(|e| format!("Lost the connection to {}: {e}", peer.alias))
}

fn query(session: &str, file: &str, token: &str) -> String {
    format!(
        "{}/upload?sessionId={}&fileId={}&token={}",
        proto::API,
        proto::url_encode(session),
        proto::url_encode(file),
        proto::url_encode(token)
    )
}

/// Ask the peer to accept `entries`, and send them once it does. Blocks.
/// `register` is told each connection's socket so another thread can break a
/// wait by shutting it down; `on` hears progress after every chunk.
pub fn deliver(
    me: &DeviceInfo,
    peer: &Peer,
    entries: &[Entry],
    clipboard: bool,
    cancel: &AtomicBool,
    register: &dyn Fn(Option<TcpStream>),
    on: &mut dyn FnMut(Progress),
) -> Result<Outcome, String> {
    let mut progress = Progress {
        phase: Phase::Waiting,
        done: 0,
        total: entries.iter().map(|e| e.size).sum(),
        files_done: 0,
        files_total: entries.len(),
        current: None,
    };
    on(progress.clone());

    let mut files = BTreeMap::new();
    for entry in entries {
        files.insert(
            entry.id.clone(),
            FileMeta {
                id: entry.id.clone(),
                file_name: entry.name.clone(),
                size: entry.size,
                file_type: entry.mime.clone(),
                sha256: None,
                preview: match &entry.source {
                    Source::Text(text) => Some(text.clone()),
                    Source::File(_) => None,
                },
                metadata: None,
            },
        );
    }
    let mut info = me.clone();
    info.announce = None;
    let pulse = clipboard.then_some(PulseExtra { clipboard: true });
    let body = serde_json::to_vec(&PrepareUploadRequest { info, files, pulse })
        .map_err(|e| e.to_string())?;
    let prepare = format!("{}/prepare-upload", proto::API);
    // The other person may take a while to say yes. A message counts as sent
    // only once they have accepted it (or allow this device), like a file.
    let reply = exchange(
        peer,
        &prepare,
        Some(&body),
        Duration::from_secs(190),
        register,
        &mut || {},
    );
    if cancel.load(Ordering::Relaxed) {
        return Ok(Outcome::Cancelled);
    }
    let reply = reply?;
    let session = match reply.status {
        200 => serde_json::from_slice::<PrepareUploadResponse>(&reply.body)
            .map_err(|_| format!("{} sent an answer Pulse doesn't understand.", peer.alias))?,
        204 => return Ok(Outcome::Done),
        403 => return Ok(Outcome::Declined),
        401 => {
            return Err(format!(
                "{} needs a PIN, which Pulse doesn't support yet.",
                peer.alias
            ));
        }
        409 => return Err(format!("{} is busy with another transfer.", peer.alias)),
        429 => {
            return Err(format!(
                "{} says there are too many requests. Try again shortly.",
                peer.alias
            ));
        }
        other => return Err(format!("{} answered {other}.", peer.alias)),
    };
    // A partial accept sends only the files it wants.
    progress.files_total = entries
        .iter()
        .filter(|e| session.files.contains_key(&e.id))
        .count();
    progress.total = entries
        .iter()
        .filter(|e| session.files.contains_key(&e.id))
        .map(|e| e.size)
        .sum();
    progress.phase = Phase::Sending;
    on(progress.clone());

    for entry in entries {
        let Some(token) = session.files.get(&entry.id) else {
            continue;
        };
        if cancel.load(Ordering::Relaxed) {
            send_cancel(peer, &session.session_id);
            return Ok(Outcome::Cancelled);
        }
        progress.current = Some(entry.name.clone());
        on(progress.clone());
        let target = query(&session.session_id, &entry.id, token);
        let result = upload_one(peer, &target, entry, cancel, register, &mut progress, on);
        match result {
            Ok(()) => {
                progress.files_done += 1;
                on(progress.clone());
            }
            Err(error) => {
                if cancel.load(Ordering::Relaxed) {
                    send_cancel(peer, &session.session_id);
                    return Ok(Outcome::Cancelled);
                }
                return Err(error);
            }
        }
    }
    Ok(Outcome::Done)
}

fn upload_one(
    peer: &Peer,
    target: &str,
    entry: &Entry,
    cancel: &AtomicBool,
    register: &dyn Fn(Option<TcpStream>),
    progress: &mut Progress,
    on: &mut dyn FnMut(Progress),
) -> Result<(), String> {
    let mut wire = net::connect(
        peer.ip,
        peer.port,
        peer.https,
        &peer.fingerprint,
        Duration::from_secs(120),
    )
    .map_err(|e| format!("Couldn't reach {}: {e}", peer.alias))?;
    register(wire.tcp().try_clone().ok());
    let host = format!("{}:{}", peer.ip, peer.port);
    let result = net::call(
        &mut wire,
        "POST",
        &host,
        target,
        Some("application/octet-stream"),
        entry.size,
        &mut |w: &mut Wire| match &entry.source {
            Source::Text(text) => {
                w.write_all(text.as_bytes())?;
                progress.done += text.len() as u64;
                on(progress.clone());
                Ok(())
            }
            Source::File(path) => {
                let mut file = File::open(path)?;
                let mut buffer = vec![0u8; 64 * 1024];
                let mut remaining = entry.size;
                while remaining > 0 {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
                    }
                    let want = remaining.min(buffer.len() as u64) as usize;
                    let n = file.read(&mut buffer[..want])?;
                    if n == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "the file got shorter",
                        ));
                    }
                    w.write_all(&buffer[..n])?;
                    remaining -= n as u64;
                    progress.done += n as u64;
                    on(progress.clone());
                }
                Ok(())
            }
        },
    );
    wire.finish();
    register(None);
    let reply = result.map_err(|e| format!("Sending {} stopped: {e}", entry.name))?;
    match reply.status {
        200 | 204 => Ok(()),
        403 => Err(format!("{} refused {}.", peer.alias, entry.name)),
        422 => Err(format!(
            "{} reported {} arrived damaged.",
            peer.alias, entry.name
        )),
        other => Err(format!(
            "{} answered {other} for {}.",
            peer.alias, entry.name
        )),
    }
}

/// Tell the peer the session is over. Best effort.
fn send_cancel(peer: &Peer, session: &str) {
    let target = format!(
        "{}/cancel?sessionId={}",
        proto::API,
        proto::url_encode(session)
    );
    let _ = net::request_json(
        peer.ip,
        peer.port,
        peer.https,
        &peer.fingerprint,
        "POST",
        &target,
        None,
        Duration::from_secs(5),
    );
}
