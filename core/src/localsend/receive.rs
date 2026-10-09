//! Receiving: the HTTPS routes `register`, `info`, `prepare-upload`, `upload`
//! and `cancel`. Nothing is written until the user accepts, and every written
//! name goes through `proto::sanitize_relative` and `proto::reserve_unique`.

use super::net::{self, Request, Wire};
use super::proto::{self, DeviceInfo, FileMeta, PrepareUploadRequest, PrepareUploadResponse};
use super::{
    Event, Incoming, IncomingFile, Inner, Pending, Session, SessionFile, Transfer, lock, now_ms,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

type Answer = (u16, Option<Vec<u8>>);

const ANSWER_WAIT: Duration = Duration::from_secs(60);
const SESSION_IDLE: Duration = Duration::from_secs(120);

fn json<T: Serialize>(status: u16, value: &T) -> Answer {
    (status, serde_json::to_vec(value).ok())
}

fn message(status: u16, text: &str) -> Answer {
    (
        status,
        Some(
            serde_json::json!({ "message": text })
                .to_string()
                .into_bytes(),
        ),
    )
}

pub(crate) fn handle_connection(inner: &Arc<Inner>, socket: TcpStream, address: SocketAddr) {
    let _ = socket.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = socket.set_write_timeout(Some(Duration::from_secs(30)));
    let Ok(wire) = net::accept_tls(&inner.tls, socket) else {
        return;
    };
    let mut reader = BufReader::new(wire);
    let Ok(request) = net::read_request(&mut reader) else {
        return;
    };
    let (status, body) = route(inner, &mut reader, &request, address.ip());
    let wire = reader.get_mut();
    let _ = net::write_response(wire, status, body.as_deref());
    wire.finish();
}

/// Read and drop a request body so an early answer is not lost to a reset.
fn discard(reader: &mut BufReader<Wire>, request: &Request) {
    let _ = net::read_body(reader, &request.headers, 8 * 1024 * 1024, &mut |_| Ok(()));
}

fn route(
    inner: &Arc<Inner>,
    reader: &mut BufReader<Wire>,
    request: &Request,
    ip: IpAddr,
) -> Answer {
    let path = request.path.trim_end_matches('/');
    let Some(endpoint) = path.strip_prefix(proto::API) else {
        discard(reader, request);
        return message(404, "Not found");
    };
    match (request.method.as_str(), endpoint) {
        ("GET", "/info") => {
            let mut me = inner.me.clone();
            me.announce = None;
            json(200, &me)
        }
        ("POST", "/register") => register(inner, reader, request, ip),
        ("POST", "/prepare-upload") => prepare_upload(inner, reader, request, ip),
        ("POST", "/upload") => upload(inner, reader, request, ip),
        ("POST", "/cancel") => {
            discard(reader, request);
            cancel_session(inner, request, ip)
        }
        _ => {
            discard(reader, request);
            message(404, "Not found")
        }
    }
}

fn read_json_body(reader: &mut BufReader<Wire>, request: &Request, max: u64) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    net::read_body(reader, &request.headers, max, &mut |chunk| {
        body.extend_from_slice(chunk);
        Ok(())
    })
    .ok()?;
    Some(body)
}

fn register(
    inner: &Arc<Inner>,
    reader: &mut BufReader<Wire>,
    request: &Request,
    ip: IpAddr,
) -> Answer {
    let Some(body) = read_json_body(reader, request, 64 * 1024) else {
        return message(400, "Invalid body");
    };
    let Ok(info) = serde_json::from_slice::<DeviceInfo>(&body) else {
        return message(400, "Invalid body");
    };
    inner.upsert(&info, ip);
    let mut me = inner.me.clone();
    me.announce = None;
    json(200, &me)
}

// ---- prepare-upload --------------------------------------------------------

fn prepare_upload(
    inner: &Arc<Inner>,
    reader: &mut BufReader<Wire>,
    request: &Request,
    ip: IpAddr,
) -> Answer {
    let Some(body) = read_json_body(reader, request, 16 * 1024 * 1024) else {
        return message(400, "Invalid body");
    };
    let Ok(parsed) = serde_json::from_slice::<PrepareUploadRequest>(&body) else {
        return message(400, "Invalid body");
    };
    if parsed.files.is_empty() || parsed.files.len() > 20_000 {
        return message(400, "Invalid body");
    }

    // A Pulse bridge message is verified and handed to the host; it never
    // waits behind a file transfer, is never shown, and never touches disk.
    if proto::is_bridge(&parsed.files) {
        return bridge_in(inner, &parsed);
    }
    let is_pair = proto::is_pair(&parsed.files);
    let pair_offer = if is_pair {
        parse_pair_offer(&parsed.files)
    } else {
        None
    };
    if is_pair && pair_offer.is_none() {
        return message(400, "Invalid body");
    }

    // One transfer at a time; an abandoned one is cleared.
    {
        let mut sessions = lock(&inner.sessions);
        let stale: Vec<String> = sessions
            .iter()
            .filter(|(_, s)| s.activity.elapsed() > SESSION_IDLE)
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            if let Some(session) = sessions.remove(&id) {
                session.cancel.store(true, Ordering::Relaxed);
                inner.end_transfer(
                    &session.transfer_id,
                    "failed",
                    Some("The sender stopped responding.".into()),
                );
            }
        }
        if !sessions.is_empty() || !lock(&inner.pending).is_empty() {
            return message(409, "Blocked by another session");
        }
    }

    let config = lock(&inner.cfg).clone();
    let total: u64 = parsed.files.values().map(|f| f.size).sum();
    let is_message = proto::is_message(&parsed.files);
    let known = inner.is_known(&parsed.info.fingerprint, ip);
    let alias: String = parsed
        .info
        .alias
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    let request_id = proto::random_hex(8);

    let incoming = Incoming {
        id: request_id.clone(),
        from: if alias.is_empty() {
            "A device".to_string()
        } else {
            alias.clone()
        },
        device_model: parsed.info.device_model.clone(),
        fingerprint: parsed.info.fingerprint.clone(),
        ip: ip.to_string(),
        file_count: parsed.files.len(),
        total_bytes: total,
        is_message,
        preview: if is_message {
            parsed
                .files
                .values()
                .next()
                .and_then(|f| f.preview.clone())
                .map(|p| p.chars().take(300).collect())
        } else {
            None
        },
        files: parsed
            .files
            .values()
            .take(8)
            .map(|f| IncomingFile {
                name: proto::sanitize_relative(&f.file_name).join("/"),
                size: f.size,
            })
            .collect(),
        known,
    };

    // A message needs no yes: it is shown, as the LocalSend app does.
    // A pairing offer always asks, even from a known device.
    let accepted = if is_message || (!is_pair && config.accept_known && known) {
        true
    } else {
        let pending = Arc::new(Pending {
            incoming: incoming.clone(),
            decision: Mutex::new(None),
            changed: Condvar::new(),
        });
        lock(&inner.pending).insert(request_id.clone(), pending.clone());
        inner.emit(Event::Incoming(incoming.clone()));
        let answer = {
            let guard = lock(&pending.decision);
            let (guard, _) = pending
                .changed
                .wait_timeout_while(guard, ANSWER_WAIT, |d| {
                    d.is_none() && !inner.stop.load(Ordering::Relaxed)
                })
                .unwrap_or_else(|p| p.into_inner());
            *guard
        };
        lock(&inner.pending).remove(&request_id);
        inner.emit(Event::IncomingResolved(request_id.clone()));
        answer == Some(true)
    };
    if !accepted {
        return message(403, "Rejected");
    }

    // The user said yes to pairing: keep the offered key next to the fingerprint.
    if let Some(offer) = pair_offer {
        inner.store_pair(&parsed.info.fingerprint, &incoming.from, &offer);
        inner.emit(Event::Changed);
        return (204, None);
    }

    let transfer_id = proto::random_hex(8);
    inner.add_transfer(Transfer {
        id: transfer_id.clone(),
        direction: "receive".into(),
        peer: incoming.from.clone(),
        peer_fingerprint: parsed.info.fingerprint.clone(),
        state: "active".into(),
        total_bytes: total,
        done_bytes: 0,
        files_total: parsed.files.len(),
        files_done: 0,
        current: None,
        saved_to: None,
        saved_files: Vec::new(),
        error: None,
        message: None,
        started: now_ms(),
        finished: None,
    });

    // A text message arrives in the request itself: hand it to the notch,
    // which shows it with Copy. Nothing is written to disk.
    if is_message {
        let text = incoming.preview.clone().unwrap_or_default();
        let full: String = parsed
            .files
            .values()
            .next()
            .and_then(|f| f.preview.clone())
            .unwrap_or(text)
            .chars()
            .take(64 * 1024)
            .collect();
        inner.update_transfer(&transfer_id, true, |t| t.message = Some(full));
        inner.end_transfer(&transfer_id, "done", None);
        return (204, None);
    }

    let mut files = HashMap::new();
    let mut tokens = std::collections::BTreeMap::new();
    for (key, meta) in parsed.files {
        let token = proto::random_hex(16);
        tokens.insert(key.clone(), token.clone());
        files.insert(
            key,
            SessionFile {
                meta,
                token,
                done: false,
            },
        );
    }
    let session_id = proto::random_hex(16);
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    lock(&inner.sessions).insert(
        session_id.clone(),
        Session {
            transfer_id,
            peer_ip: ip,
            peer_fingerprint: parsed.info.fingerprint.clone(),
            remaining: files.len(),
            files,
            cancel,
            activity: Instant::now(),
            saved: Vec::new(),
            save_dir: config.save_dir,
        },
    );
    json(
        200,
        &PrepareUploadResponse {
            session_id,
            files: tokens,
        },
    )
}

// ---- Pulse bridge ----------------------------------------------------------

/// The 32-byte key a pairing offer carries, as 64 hex characters.
fn parse_pair_offer(files: &std::collections::BTreeMap<String, FileMeta>) -> Option<String> {
    let preview = files.values().next()?.preview.as_ref()?;
    let value: serde_json::Value = serde_json::from_str(preview).ok()?;
    let offer = value.get("offer")?.as_str()?;
    (offer.len() == 64 && crate::bridge::envelope::from_hex(offer).is_some())
        .then(|| offer.to_ascii_lowercase())
}

/// A bridge envelope: known paired device, matching identity, valid signature,
/// fresh clock and nonce. 204 when taken; the host hears `Event::Bridge`.
fn bridge_in(inner: &Arc<Inner>, parsed: &PrepareUploadRequest) -> Answer {
    use crate::bridge::envelope::{self, Envelope};
    let fingerprint = parsed.info.fingerprint.as_str();
    let Some(key) = inner.pair_key(fingerprint) else {
        return message(403, "Unknown device");
    };
    let Some(text) = parsed
        .files
        .values()
        .next()
        .and_then(|f| f.preview.as_ref())
    else {
        return message(400, "Invalid body");
    };
    let env = match Envelope::from_json(text) {
        Ok(env) => env,
        Err(_) => return message(400, "Invalid body"),
    };
    if env.from.device != fingerprint || env.to.device != inner.me.fingerprint {
        return message(403, "Rejected: wrong device");
    }
    let checked = {
        let mut replay = lock(&inner.replay);
        envelope::accept(&env, Some(key.as_slice()), envelope::now_ms(), &mut replay)
    };
    match checked {
        Ok(()) => {
            inner.emit(Event::Bridge(env));
            (204, None)
        }
        Err(e) => message(403, &format!("Rejected: {e}")),
    }
}

// ---- upload ----------------------------------------------------------------

fn same_token(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

struct Claimed {
    meta: FileMeta,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    transfer_id: String,
    save_dir: PathBuf,
}

fn upload(
    inner: &Arc<Inner>,
    reader: &mut BufReader<Wire>,
    request: &Request,
    ip: IpAddr,
) -> Answer {
    let (Some(session_id), Some(file_id), Some(token)) = (
        request.param("sessionId"),
        request.param("fileId"),
        request.param("token"),
    ) else {
        discard(reader, request);
        return message(400, "Missing parameters");
    };

    let claim: Result<Option<Claimed>, Answer> = {
        let mut sessions = lock(&inner.sessions);
        match sessions.get_mut(session_id) {
            Some(session) if session.peer_ip == ip => match session.files.get(file_id) {
                Some(file) if same_token(&file.token, token) => {
                    if file.done {
                        Ok(None)
                    } else {
                        session.activity = Instant::now();
                        Ok(Some(Claimed {
                            meta: file.meta.clone(),
                            cancel: session.cancel.clone(),
                            transfer_id: session.transfer_id.clone(),
                            save_dir: session.save_dir.clone(),
                        }))
                    }
                }
                _ => Err(message(403, "Invalid token or IP address")),
            },
            Some(_) => Err(message(403, "Invalid token or IP address")),
            None => Err(message(403, "Invalid token or IP address")),
        }
    };
    let claimed = match claim {
        Ok(Some(claimed)) => claimed,
        Ok(None) => {
            discard(reader, request);
            return (200, None);
        }
        Err(answer) => {
            discard(reader, request);
            return answer;
        }
    };

    let parts = proto::sanitize_relative(&claimed.meta.file_name);
    let final_path = match proto::reserve_unique(&claimed.save_dir, &parts) {
        Ok(path) => path,
        Err(e) => {
            discard(reader, request);
            inner.end_transfer(
                &claimed.transfer_id,
                "failed",
                Some(format!("Couldn't save the file: {e}")),
            );
            return message(500, "Unknown error by receiver");
        }
    };
    let part_path = partial_path(&final_path);
    let mut out = match File::create(&part_path) {
        Ok(file) => file,
        Err(e) => {
            let _ = std::fs::remove_file(&final_path);
            discard(reader, request);
            inner.end_transfer(
                &claimed.transfer_id,
                "failed",
                Some(format!("Couldn't save the file: {e}")),
            );
            return message(500, "Unknown error by receiver");
        }
    };

    let display_name = parts.join("/");
    let mut hasher = claimed.meta.sha256.as_ref().map(|_| Sha256::new());
    let allowed = claimed.meta.size.saturating_add(1024 * 1024);
    let streamed = net::read_body(reader, &request.headers, allowed, &mut |chunk| {
        if claimed.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        out.write_all(chunk)?;
        if let Some(h) = hasher.as_mut() {
            h.update(chunk);
        }
        inner.update_transfer(&claimed.transfer_id, false, |t| {
            t.done_bytes += chunk.len() as u64;
            t.current = Some(display_name.clone());
        });
        Ok(())
    });
    let flushed = out.flush();
    drop(out);

    let failure = match (&streamed, flushed) {
        (Err(e), _) => Some(if claimed.cancel.load(Ordering::Relaxed) {
            "cancelled".to_string()
        } else {
            format!("The transfer was interrupted ({e}).")
        }),
        (_, Err(e)) => Some(format!("Couldn't save the file: {e}")),
        _ => None,
    };
    if let Some(reason) = failure {
        let _ = std::fs::remove_file(&part_path);
        let _ = std::fs::remove_file(&final_path);
        if reason != "cancelled" {
            inner.end_transfer(&claimed.transfer_id, "failed", Some(reason));
        }
        return message(500, "Unknown error by receiver");
    }
    if let (Some(h), Some(expected)) = (hasher, claimed.meta.sha256.as_ref()) {
        let actual: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if !actual.eq_ignore_ascii_case(expected) {
            let _ = std::fs::remove_file(&part_path);
            let _ = std::fs::remove_file(&final_path);
            inner.end_transfer(
                &claimed.transfer_id,
                "failed",
                Some(format!("{display_name} arrived damaged.")),
            );
            return message(422, "Checksum mismatch");
        }
    }
    if let Err(e) = std::fs::rename(&part_path, &final_path) {
        let _ = std::fs::remove_file(&part_path);
        let _ = std::fs::remove_file(&final_path);
        inner.end_transfer(
            &claimed.transfer_id,
            "failed",
            Some(format!("Couldn't save the file: {e}")),
        );
        return message(500, "Unknown error by receiver");
    }
    finish_file(inner, session_id, file_id, final_path);
    (200, None)
}

fn partial_path(final_path: &Path) -> PathBuf {
    let name = final_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    final_path.with_file_name(format!(".{name}.pulse-part"))
}

fn finish_file(inner: &Arc<Inner>, session_id: &str, file_id: &str, path: PathBuf) {
    let finished = {
        let mut sessions = lock(&inner.sessions);
        let Some(session) = sessions.get_mut(session_id) else {
            return;
        };
        if let Some(file) = session.files.get_mut(file_id)
            && !file.done
        {
            file.done = true;
            session.remaining = session.remaining.saturating_sub(1);
            session.saved.push(path);
        }
        session.activity = Instant::now();
        let transfer_id = session.transfer_id.clone();
        let done = session.remaining == 0;
        let completed = done.then(|| sessions.remove(session_id)).flatten();
        (transfer_id, completed)
    };
    let (transfer_id, completed) = finished;
    inner.update_transfer(&transfer_id, true, |t| t.files_done += 1);
    if let Some(session) = completed {
        inner.trust(&session.peer_fingerprint);
        let folder = session.save_dir.to_string_lossy().into_owned();
        let saved: Vec<String> = session
            .saved
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        inner.update_transfer(&transfer_id, true, |t| {
            t.saved_to = Some(folder);
            t.saved_files = saved;
        });
        inner.end_transfer(&transfer_id, "done", None);
    }
}

// ---- cancel ----------------------------------------------------------------

fn cancel_session(inner: &Arc<Inner>, request: &Request, ip: IpAddr) -> Answer {
    let Some(session_id) = request.param("sessionId") else {
        return message(400, "Missing parameters");
    };
    let removed = {
        let mut sessions = lock(&inner.sessions);
        match sessions.get(session_id).map(|s| s.peer_ip == ip) {
            Some(true) => sessions.remove(session_id),
            Some(false) => return message(403, "Invalid token or IP address"),
            None => None,
        }
    };
    if let Some(session) = removed {
        session.cancel.store(true, Ordering::Relaxed);
        inner.end_transfer(&session.transfer_id, "cancelled", None);
    }
    (200, None)
}

/// The user cancelled a receive from this side.
pub(crate) fn cancel_receive(inner: &Arc<Inner>, transfer_id: &str) -> bool {
    let removed = {
        let mut sessions = lock(&inner.sessions);
        let key = sessions
            .iter()
            .find(|(_, s)| s.transfer_id == transfer_id)
            .map(|(k, _)| k.clone());
        key.and_then(|k| sessions.remove(&k))
    };
    match removed {
        Some(session) => {
            session.cancel.store(true, Ordering::Relaxed);
            inner.end_transfer(transfer_id, "cancelled", None);
            true
        }
        None => false,
    }
}
