//! Push a bridged message into a running Claude chat on this computer, and
//! listen for the chat's replies.
//!
//! Every Claude Code / Claude Desktop (Code tab) chat process publishes
//! `~/.claude/sessions/<pid>.json` (pid, sessionId, name, peerProtocol,
//! messagingSocketPath, procStart, pidDomain) and a 0600 key file
//! `~/.claude/sessions/<pid>.<hash>.key` holding `{peerToken, procStart,
//! pidDomain}`. The chat listens on a Unix stream socket (a named pipe on
//! Windows) and speaks newline-delimited JSON:
//!
//! Wire shape (captured from a real Claude Code 2.1.293 session on macOS). One
//! newline-terminated JSON line; the sender's identity lives inside the content
//! as a wrapper tag, not in top-level fields:
//!
//! `{"msgV":1,"msg_id":..,"type":"user","message":{"role":"user","content":
//! "<cross-session-message from=\"uds:<reply>\" from-session=\"..\"
//! from-name=\"<peer> via Pulse\" from-mode=\"bypass\">\n<text>\n</cross-session-message>"},
//! "priority":"next","from":"uds:<reply>"}`
//!
//! On macOS/Linux no auth line is sent. On native Windows the auth frame
//! `{"type":"auth","token":<peerToken>}` goes first, using the key file token.
//! The whole payload is prepared before connecting (the chat wants it within
//! 30 s). Outcomes are Delivered / Held / Refused; a real client is normally
//! silent, so no control frame within 3 s counts as Delivered when the write
//! succeeded. The chat may answer with `{"type":"control","action":
//! "peer_message_status"|"peer_message_hold"|..}`. The key is read but never
//! logged or put in an error.
//!
//! Replies: the chat answers to `from`, a socket this process listens on
//! (`ReplyHub`). One socket per remote peer, so a reply knows who it is for.
//! Empty connections (a liveness probe) are tolerated. On Windows the reply
//! listener is not implemented: no `from` is sent, so a chat replies with
//! `pulse bridge send`.

use super::{BridgeError, Envelope, LocalSession, Receipt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

/// The only `peerProtocol` this code speaks.
pub const PEER_PROTOCOL: u64 = 1;
/// How long to wait for the chat's accept / hold / refuse.
const ACK_WINDOW: Duration = Duration::from_secs(3);
#[cfg_attr(not(unix), allow(dead_code))]
const MAX_REPLY_BYTES: u64 = 1024 * 1024;

/// The few places this file touches the types `bridge/mod.rs` owns. If those
/// types differ, adapt here only.
///
/// `bridge/mod.rs` provides `Receipt::{delivered,held,refused,unsupported}`
/// and `BridgeError::{SessionGone,AuthRejected,Io}`; the envelope's text is
/// `body`; `LocalSession::pid` is optional.
pub(super) mod shim {
    use super::*;

    pub fn delivered(reason: &str) -> Receipt {
        Receipt::delivered(reason)
    }
    pub fn held(reason: &str) -> Receipt {
        Receipt::held(reason)
    }
    pub fn refused(reason: &str) -> Receipt {
        Receipt::refused(reason)
    }
    pub fn unsupported(reason: &str) -> Receipt {
        Receipt::unsupported(reason)
    }
    pub fn gone(reason: &str) -> BridgeError {
        BridgeError::SessionGone(reason.to_string())
    }
    pub fn auth_rejected(reason: &str) -> BridgeError {
        BridgeError::AuthRejected(reason.to_string())
    }
    pub fn io(reason: &str) -> BridgeError {
        BridgeError::Io(reason.to_string())
    }
    pub fn env_id(env: &Envelope) -> &str {
        &env.id
    }
    pub fn env_text(env: &Envelope) -> &str {
        &env.body
    }
    /// "<chat title> on <device alias>".
    pub fn env_from_name(env: &Envelope) -> &str {
        &env.from.name
    }
    /// Names one remote chat; replies to it come back to `<device>:<session>`.
    pub fn env_peer_key(env: &Envelope) -> String {
        format!("{}:{}", env.from.device, env.from.session)
    }
    pub fn session_pid(session: &LocalSession) -> u32 {
        session.pid.unwrap_or(0)
    }
}

fn home() -> PathBuf {
    #[cfg(windows)]
    let variable = "USERPROFILE";
    #[cfg(not(windows))]
    let variable = "HOME";
    std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `~/.claude/sessions`, or `$CLAUDE_CONFIG_DIR/sessions` when that is set.
pub fn sessions_dir() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("sessions"),
        None => home().join(".claude").join("sessions"),
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Is this process still running, and (on macOS, where the key file records
/// the start time `ps` prints) is it the same process that wrote the files?
fn process_matches(pid: u32, recorded_start: Option<&str>, domain: Option<&str>) -> bool {
    let mut system = sysinfo::System::new();
    let target = sysinfo::Pid::from_u32(pid);
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[target]), true);
    if system.process(target).is_none() {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        if domain.is_none_or(|d| d == "darwin")
            && let Some(recorded) = recorded_start.filter(|r| !r.is_empty())
        {
            let out = std::process::Command::new("/bin/ps")
                .args(["-o", "lstart=", "-p", &pid.to_string()])
                .output();
            return match out {
                Ok(out) if out.status.success() => {
                    squash(&String::from_utf8_lossy(&out.stdout)) == squash(recorded)
                }
                _ => false,
            };
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (recorded_start, domain);
    }
    true
}

struct SessionFiles {
    socket: String,
    #[cfg_attr(not(windows), allow(dead_code))]
    token: String,
}

fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

/// Validate a session's files and read what is needed to talk to it.
fn open_session(pid: u32) -> Result<Result<SessionFiles, String>, BridgeError> {
    let dir = sessions_dir();
    let info = read_json(&dir.join(format!("{pid}.json")))
        .ok_or_else(|| shim::gone("This chat is no longer running."))?;
    let protocol = info["peerProtocol"].as_u64().unwrap_or(0);
    if protocol != PEER_PROTOCOL {
        return Ok(Err(format!(
            "This chat speaks messaging protocol {protocol}; Pulse speaks {PEER_PROTOCOL}."
        )));
    }
    let socket = info["messagingSocketPath"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| shim::gone("This chat has no messaging socket."))?
        .to_string();
    // The key file name carries a hash Pulse can't predict: take the one for this pid.
    let prefix = format!("{pid}.");
    let key_file = std::fs::read_dir(&dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".key"))
        })
        .ok_or_else(|| shim::gone("This chat's key file is missing."))?;
    let key =
        read_json(&key_file).ok_or_else(|| shim::gone("This chat's key file is unreadable."))?;
    let token = key["peerToken"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| shim::gone("This chat's key file has no token."))?
        .to_string();
    let start = key["procStart"].as_str().or(info["procStart"].as_str());
    if !process_matches(pid, start, key["pidDomain"].as_str()) {
        return Err(shim::gone(
            "This chat has closed (its process is gone or was replaced).",
        ));
    }
    Ok(Ok(SessionFiles { socket, token }))
}

type Writer = Box<dyn Write + Send>;
type Reader = Box<dyn Read + Send>;

#[cfg(unix)]
fn connect(path: &str) -> std::io::Result<(Writer, Reader)> {
    use std::os::unix::net::UnixStream;
    let stream = UnixStream::connect(path)?;
    stream.set_write_timeout(Some(ACK_WINDOW))?;
    let reader = stream.try_clone()?;
    Ok((Box::new(stream), Box::new(reader)))
}

#[cfg(windows)]
fn connect(path: &str) -> std::io::Result<(Writer, Reader)> {
    // A named pipe opens like a file. Both frames are written before anything
    // is read, because a synchronous pipe serialises I/O on its handles.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let reader = file.try_clone()?;
    Ok((Box::new(file), Box::new(reader)))
}

#[cfg(not(any(unix, windows)))]
fn connect(_path: &str) -> std::io::Result<(Writer, Reader)> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no local messaging on this system",
    ))
}

enum Verdict {
    Delivered(String),
    Held(String),
    Refused(String),
    AuthRejected(String),
    Protocol(String),
}

fn reason_of(frame: &Value) -> String {
    ["reason", "message", "detail", "error"]
        .iter()
        .find_map(|k| frame[*k].as_str().filter(|s| !s.is_empty()))
        .unwrap_or("")
        .chars()
        .take(300)
        .collect()
}

/// Read one control frame as an accept / hold / refuse, or None if it says nothing final.
fn classify(frame: &Value) -> Option<Verdict> {
    let kind = frame["type"].as_str().unwrap_or("");
    let action = frame["action"].as_str().unwrap_or("").to_ascii_lowercase();
    let reason = reason_of(frame);
    let lower = reason.to_ascii_lowercase();
    if kind == "error" || action.contains("auth") {
        if lower.contains("protocol") {
            return Some(Verdict::Protocol(reason));
        }
        return Some(Verdict::AuthRejected(reason));
    }
    if kind != "control" {
        return None;
    }
    if action == "peer_message_hold" || action.contains("hold") {
        return Some(Verdict::Held(reason));
    }
    if action.contains("refus") || action.contains("reject") {
        return Some(Verdict::Refused(reason));
    }
    if action == "peer_message_status" {
        let status = ["status", "state", "result"]
            .iter()
            .find_map(|k| frame[*k].as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        return match status.as_str() {
            "delivered" | "accepted" | "ok" | "queued" | "injected" | "received" => {
                Some(Verdict::Delivered(reason))
            }
            "held" | "hold" => Some(Verdict::Held(reason)),
            "refused" | "rejected" | "declined" | "denied" | "blocked" => {
                Some(Verdict::Refused(reason))
            }
            _ => None,
        };
    }
    None
}

const TAG_OPEN: &str = "<cross-session-message";
const TAG_CLOSE: &str = "</cross-session-message>";

/// An attribute value that can't break out of its quotes.
fn attr_safe(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '"' => '\'',
            '<' | '>' | '\n' | '\r' => ' ',
            c => c,
        })
        .collect()
}

/// The content a chat sees: the text inside the wrapper tag the real client
/// uses, with any wrapper tag inside the text neutralised.
fn wrap_content(from: &str, session: &str, name: &str, text: &str) -> String {
    let text = text
        .replace(TAG_OPEN, "&lt;cross-session-message")
        .replace(TAG_CLOSE, "&lt;/cross-session-message>");
    format!(
        "{TAG_OPEN} from=\"{}\" from-session=\"{}\" from-name=\"{}\" from-mode=\"bypass\">\n{text}\n{TAG_CLOSE}",
        attr_safe(from),
        attr_safe(session),
        attr_safe(name),
    )
}

/// Attributes and body of a `<cross-session-message ...>` wrapper, if `content` is one.
#[cfg_attr(not(unix), allow(dead_code))]
fn parse_wrapper(content: &str) -> Option<(HashMap<String, String>, String)> {
    let rest = content.trim().strip_prefix(TAG_OPEN)?;
    if !rest.starts_with(char::is_whitespace) && !rest.starts_with('>') {
        return None;
    }
    let mut attrs = HashMap::new();
    let mut chars = rest.char_indices().peekable();
    let body_start = loop {
        let (i, c) = chars.next()?;
        if c == '>' {
            break i + 1;
        }
        if c.is_whitespace() {
            continue;
        }
        let mut key = String::from(c);
        loop {
            match chars.next()? {
                (_, '=') => break,
                (_, k) => key.push(k),
            }
        }
        if chars.next()?.1 != '"' {
            return None;
        }
        let mut value = String::new();
        loop {
            match chars.next()? {
                (_, '"') => break,
                (_, v) => value.push(v),
            }
        }
        attrs.insert(key.trim().to_string(), value);
    };
    let inner = &rest[body_start..];
    let end = inner.rfind(TAG_CLOSE).unwrap_or(inner.len());
    Some((attrs, inner[..end].trim().to_string()))
}

/// Deliver one envelope to a chat on this computer and report what the chat
/// did with it. `Ok(Receipt::unsupported)` when the chat speaks another
/// protocol version; `Err` when it is gone or turned Pulse away.
pub fn deliver(session: &LocalSession, env: &Envelope) -> Result<Receipt, BridgeError> {
    deliver_via(session, env, None)
}

/// `deliver`, with `reply_socket` (the sending chat's own messaging socket)
/// as the address the chat replies to instead of a reply-hub socket.
pub fn deliver_via(
    session: &LocalSession,
    env: &Envelope,
    reply_socket: Option<&str>,
) -> Result<Receipt, BridgeError> {
    let pid = shim::session_pid(session);
    let files = match open_session(pid)? {
        Ok(files) => files,
        Err(reason) => return Ok(shim::unsupported(&reason)),
    };
    let peer_key = shim::env_peer_key(env);
    let peer_key = peer_key.as_str();
    let from = reply_socket
        .map(str::to_string)
        .or_else(|| current_hub().and_then(|hub| hub.address_for(peer_key)));
    let short: String = peer_key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect();
    let sender = format!("{} via Pulse", shim::env_from_name(env));
    let session_id = format!("pulse-{short}");
    let reply_from = from.map(|address| format!("uds:{address}"));
    let content = wrap_content(
        reply_from.as_deref().unwrap_or(""),
        &session_id,
        &sender,
        shim::env_text(env),
    );
    let mut user = json!({
        "msgV": 1,
        "msg_id": shim::env_id(env),
        "type": "user",
        "message": {"role": "user", "content": content},
        "priority": "next",
    });
    if let Some(address) = reply_from {
        user["from"] = json!(address);
    }
    let mut frames = Vec::new();
    if cfg!(windows) {
        frames.push(json!({"type": "auth", "token": files.token}));
    }
    frames.push(user);
    let mut lines = Vec::new();
    for frame in frames {
        let mut line = serde_json::to_string(&frame).map_err(|e| shim::io(&e.to_string()))?;
        line.push('\n');
        lines.push(line);
    }
    let (mut writer, reader) = connect(&files.socket).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound
        | std::io::ErrorKind::ConnectionRefused
        | std::io::ErrorKind::PermissionDenied => {
            shim::gone("This chat's messaging socket is not accepting connections.")
        }
        _ => shim::io(&format!("Couldn't reach the chat: {}", e.kind())),
    })?;

    for line in lines {
        writer
            .write_all(line.as_bytes())
            .and_then(|_| writer.flush())
            .map_err(|_| shim::gone("The chat closed the connection while Pulse was sending."))?;
    }

    let (tx, rx) = mpsc::channel::<Value>();
    // Ends when the chat closes the connection; a stuck chat leaves one idle thread behind.
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else { break };
            if let Ok(frame) = serde_json::from_str::<Value>(&line)
                && tx.send(frame).is_err()
            {
                break;
            }
        }
    });
    let deadline = Instant::now() + ACK_WINDOW;
    let mut heard_anything = false;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(frame) => {
                heard_anything = true;
                match classify(&frame) {
                    Some(Verdict::Delivered(r)) => return Ok(shim::delivered(&r)),
                    Some(Verdict::Held(r)) => return Ok(shim::held(&r)),
                    Some(Verdict::Refused(r)) => {
                        let r = if r.is_empty() {
                            "refused by that session's settings".to_string()
                        } else {
                            r
                        };
                        return Ok(shim::refused(&r));
                    }
                    Some(Verdict::Protocol(r)) => return Ok(shim::unsupported(&r)),
                    Some(Verdict::AuthRejected(r)) => return Err(shim::auth_rejected(&r)),
                    None => {}
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Ok(shim::delivered(
                    "Sent; the chat sent no control frame (silence is normal).",
                ));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return if heard_anything || !cfg!(windows) {
                    Ok(shim::delivered(
                        "Sent; the chat closed the connection without a verdict.",
                    ))
                } else {
                    Err(shim::auth_rejected(
                        "The chat closed the connection right after the handshake.",
                    ))
                };
            }
        }
    }
}

// ---- replies from chats ----------------------------------------------------------

/// A chat's reply, arrived on one remote peer's reply socket.
#[derive(Clone, Debug)]
pub struct ReplyMessage {
    /// The remote peer this reply goes to (the `peer_key` the address was made for).
    pub peer_key: String,
    pub from_session_id: String,
    pub from_name: String,
    pub text: String,
    pub msg_id: String,
}

type OnReply = Arc<dyn Fn(ReplyMessage) + Send + Sync>;
type IsKnown = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Listens on one Unix socket per remote peer and turns frames from known
/// local chats into `ReplyMessage`s. Unix only; on Windows it never listens.
#[cfg_attr(not(unix), allow(dead_code))]
pub struct ReplyHub {
    on_reply: OnReply,
    is_known: IsKnown,
    stop: Arc<AtomicBool>,
    paths: Mutex<HashMap<String, String>>,
}

static HUB: OnceLock<Mutex<Option<Arc<ReplyHub>>>> = OnceLock::new();

fn hub_slot() -> &'static Mutex<Option<Arc<ReplyHub>>> {
    HUB.get_or_init(|| Mutex::new(None))
}

/// The hub `deliver` takes reply addresses from. The hub sets it while sharing runs.
pub fn set_reply_hub(hub: Option<Arc<ReplyHub>>) {
    if let Ok(mut slot) = hub_slot().lock() {
        if let Some(old) = slot.take() {
            old.stop();
        }
        *slot = hub;
    }
}

fn current_hub() -> Option<Arc<ReplyHub>> {
    hub_slot().lock().ok().and_then(|s| s.clone())
}

fn fnv(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Where the reply socket for one remote peer lives: a short path in a private
/// per-user directory (Unix socket paths are limited to about 100 bytes).
pub fn reply_socket_path(peer_key: &str) -> PathBuf {
    #[cfg(unix)]
    let base = {
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        PathBuf::from(format!("/tmp/pulse-bridge-{uid}"))
    };
    #[cfg(not(unix))]
    let base = std::env::temp_dir().join("pulse-bridge");
    base.join(format!("{}.sock", fnv(peer_key)))
}

impl ReplyHub {
    /// `is_known` says whether a `from_session_id` is a chat on this computer.
    pub fn new(on_reply: OnReply, is_known: IsKnown) -> Arc<ReplyHub> {
        Arc::new(ReplyHub {
            on_reply,
            is_known,
            stop: Arc::new(AtomicBool::new(false)),
            paths: Mutex::new(HashMap::new()),
        })
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// The socket path replies from this peer's messages should go to, listening
    /// on it first. None when this system can't listen.
    pub fn address_for(self: &Arc<Self>, peer_key: &str) -> Option<String> {
        #[cfg(unix)]
        {
            let mut paths = self.paths.lock().ok()?;
            if let Some(path) = paths.get(peer_key) {
                return Some(path.clone());
            }
            let path = reply_socket_path(peer_key);
            self.listen(peer_key.to_string(), &path).ok()?;
            let text = path.to_string_lossy().into_owned();
            paths.insert(peer_key.to_string(), text.clone());
            Some(text)
        }
        #[cfg(not(unix))]
        {
            let _ = peer_key;
            None
        }
    }

    #[cfg(unix)]
    fn listen(self: &Arc<Self>, peer_key: String, path: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let hub = Arc::clone(self);
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            while !hub.stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let hub = Arc::clone(&hub);
                        let key = peer_key.clone();
                        std::thread::spawn(move || hub.handle(stream, &key));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(500)),
                }
            }
            let _ = std::fs::remove_file(&path);
        });
        Ok(())
    }

    #[cfg(unix)]
    fn handle(&self, stream: std::os::unix::net::UnixStream, peer_key: &str) {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        for line in BufReader::new(stream.take(MAX_REPLY_BYTES)).lines() {
            let Ok(line) = line else { break };
            let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            // The auth frame, if any, is ignored: what counts is that the sender is a known local chat.
            if frame["type"] != "user" {
                continue;
            }
            let Some(raw) = content_text(&frame["message"]["content"]) else {
                continue;
            };
            let (attrs, text) = parse_wrapper(&raw).unwrap_or_default();
            let text = if attrs.is_empty() { raw } else { text };
            let pick = |attr: &str, field: &str| {
                attrs
                    .get(attr)
                    .filter(|v| !v.is_empty())
                    .cloned()
                    .or_else(|| frame[field].as_str().map(str::to_string))
                    .unwrap_or_default()
            };
            let from_session_id = pick("from-session", "from_session_id");
            if from_session_id.is_empty() || !(self.is_known)(&from_session_id) || text.is_empty() {
                continue;
            }
            (self.on_reply)(ReplyMessage {
                peer_key: peer_key.to_string(),
                from_session_id,
                from_name: pick("from-name", "from_name"),
                text,
                msg_id: frame["msg_id"].as_str().unwrap_or("").to_string(),
            });
        }
    }
}

/// A message's content as plain text: a string, or the text blocks of an array.
#[cfg_attr(not(unix), allow(dead_code))]
fn content_text(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    (!text.trim().is_empty()).then_some(text)
}
