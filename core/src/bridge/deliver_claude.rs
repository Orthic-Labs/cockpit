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
//! from-name=\"<peer> via Pulse\" from-mode=\"bridge\"
//! provenance=\"agent-unverified\">\n<text>\n</cross-session-message>"},
//! "priority":"next","from":"uds:<reply>"}`
//!
//! On macOS/Linux no auth line is sent. On native Windows the auth frame
//! `{"type":"auth","token":<peerToken>}` goes first, using the key file token.
//! The whole payload is prepared before connecting (the chat wants it within
//! 30 s). Outcomes: an explicit chat verdict is Delivered, Queued (accepted or
//! held natively, consumption unknown) or Refused; a real client is normally
//! silent, so no control frame within 3 s, or a close without one, is only Sent
//! (transport ok, no verdict). The chat may answer with `{"type":"control","action":
//! "peer_message_status"|"peer_message_hold"|..}`. The key is read but never
//! logged or put in an error.
//!
//! Replies: the chat answers to `from`, a socket this process listens on
//! (`ReplyHub`). One socket per remote peer, so a reply knows who it is for.
//! Empty connections (a liveness probe) are tolerated. On Windows the reply
//! listener is one named pipe per peer, `\\.\pipe\LOCAL\pulse-bridge-<fnv>`,
//! whose DACL grants the current user and SYSTEM only. All Windows pipe I/O to a
//! chat is overlapped with deadlines (connect 3 s, write 3 s, ACK read
//! `ACK_WINDOW`); a stage that stalls is cancelled and reported by name. The
//! `pulse-bridge-noreply` placeholder `from` is advertised only when no reply route can
//! be made (no hub, or the bind failed); the receipt then says replies are unavailable.
//! ACK readers stop after 10 s or 64 KiB; reply connections after 30 s, at most 64 at once.

use super::{BridgeError, Envelope, LocalSession, Receipt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

/// The only `peerProtocol` this code speaks.
pub const PEER_PROTOCOL: u64 = 1;
/// How long to wait for the chat's accept / hold / refuse.
const ACK_WINDOW: Duration = Duration::from_secs(3);
/// Absolute limit on reading a chat's ACK frames, and the most bytes read.
const ACK_DEADLINE: Duration = Duration::from_secs(10);
const ACK_MAX_BYTES: usize = 64 * 1024;
/// Absolute limit on one reply connection, how many may be open at once.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
const REPLY_DEADLINE: Duration = Duration::from_secs(30);
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
const MAX_REPLY_CONNECTIONS: usize = 64;
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
const MAX_REPLY_BYTES: usize = 1024 * 1024;
/// How long `ReplyHub::stop` waits for its listener threads.
const STOP_JOIN: Duration = Duration::from_secs(2);

/// The few places this file touches the types `bridge/mod.rs` owns. If those
/// types differ, adapt here only.
///
/// `bridge/mod.rs` provides `Receipt::{delivered,held,refused,unsupported}`
/// and `BridgeError::{SessionGone,AuthRejected,Io}`; the envelope's text is
/// `body`; `LocalSession::pid` is optional.
pub(super) mod shim {
    use super::*;

    use crate::bridge::ReceiptState;

    fn receipt(state: ReceiptState, reason: &str) -> Receipt {
        Receipt {
            msg_id: String::new(),
            session: String::new(),
            state,
            detail: reason.to_string(),
        }
    }
    /// The chat confirmed it has the message.
    pub fn delivered(reason: &str) -> Receipt {
        receipt(ReceiptState::Delivered, reason)
    }
    /// The chat's native queue accepted it; whether it was consumed is unknown.
    pub fn queued(reason: &str) -> Receipt {
        receipt(ReceiptState::Queued, reason)
    }
    /// The transport succeeded; the chat gave no verdict.
    pub fn sent(reason: &str) -> Receipt {
        receipt(ReceiptState::Sent, reason)
    }
    /// The dispatch may or may not have happened.
    pub fn unknown(reason: &str) -> Receipt {
        receipt(ReceiptState::Unknown, reason)
    }
    pub fn refused(reason: &str) -> Receipt {
        receipt(ReceiptState::Refused, reason)
    }
    pub fn unsupported(reason: &str) -> Receipt {
        receipt(ReceiptState::Unsupported, reason)
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
    /// "<chat title> on <device alias>" (an unverified label).
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
    start_identity(pid, recorded_start, domain).unwrap_or(true)
}

/// Whether the running process `pid` started when the files say it did:
/// `Some(true)` it did, `Some(false)` another process has the pid, `None` the
/// start time was not recorded or can't be compared here.
pub(super) fn start_identity(
    pid: u32,
    recorded_start: Option<&str>,
    domain: Option<&str>,
) -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        if domain.is_none_or(|d| d == "darwin")
            && let Some(recorded) = recorded_start.filter(|r| !r.is_empty())
        {
            // Claude writes `procStart` in UTC (ctime shape); ps prints local time unless told.
            let out = std::process::Command::new("/bin/ps")
                .env("TZ", "UTC")
                .args(["-o", "lstart=", "-p", &pid.to_string()])
                .output();
            return Some(match out {
                Ok(out) if out.status.success() => {
                    squash(&String::from_utf8_lossy(&out.stdout)) == squash(recorded)
                }
                _ => false,
            });
        }
    }
    #[cfg(windows)]
    {
        // Claude records the process start; a reused pid has another creation time.
        // An identity that can't be read or compared is not live.
        // Claude writes `pidDomain` as `win32:<host>` on Windows.
        if domain.is_none_or(|d| d == "windows" || d.starts_with("win32"))
            && let Some(recorded) = recorded_start.filter(|r| !r.is_empty())
        {
            return Some(
                match (win::parse_start(recorded), win::creation_epoch_secs(pid)) {
                    (Some(want), Some(have)) => (want - have).abs() <= 2.0,
                    _ => false,
                },
            );
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (pid, recorded_start, domain);
    }
    None
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

/// A start time or domain as text, from a JSON string or integer.
pub(super) fn text_of(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_u64().map(|n| n.to_string()))
        .filter(|s| !s.is_empty())
}

/// What the roster saw of a chat: the delivery must find the same chat, started at
/// the same time, in the same pid domain.
struct Expected {
    session_id: String,
    start: Option<String>,
    domain: Option<String>,
}

fn expectation(session: &LocalSession) -> Expected {
    Expected {
        session_id: session.id.clone(),
        start: text_of(&session.raw["procStart"]),
        domain: text_of(&session.raw["pidDomain"]),
    }
}

/// The recorded process start in a key file (`procStart` on macOS, `procStartFt` on Windows).
fn key_start(key: &Value) -> Option<String> {
    text_of(&key["procStart"]).or_else(|| text_of(&key["procStartFt"]))
}

/// Validate a session's files against what the roster listed and read what is
/// needed to talk to it. A registry or key file that now describes another session id,
/// start time or pid domain is refused, so a reused pid never receives the message.
fn open_session(
    pid: u32,
    expected: &Expected,
) -> Result<Result<SessionFiles, String>, BridgeError> {
    let dir = sessions_dir();
    let info = read_json(&dir.join(format!("{pid}.json")))
        .ok_or_else(|| shim::gone("This chat is no longer running."))?;
    let changed = || shim::gone("That chat's process now belongs to a different session.");
    if info["sessionId"].as_str() != Some(expected.session_id.as_str()) {
        return Err(changed());
    }
    let info_start = text_of(&info["procStart"]);
    if let (Some(want), Some(have)) = (&expected.start, &info_start)
        && want != have
    {
        return Err(changed());
    }
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
    // The key file name carries a hash Pulse can't predict. Several generations can coexist
    // for one pid: take the one whose recorded start matches this session's, never just the
    // first filename.
    let start = expected.start.clone().or(info_start);
    let prefix = format!("{pid}.");
    let mut keys: Vec<(PathBuf, Value)> = std::fs::read_dir(&dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".key"))
        })
        .filter_map(|p| read_json(&p).map(|k| (p, k)))
        .collect();
    if keys.is_empty() {
        return Err(shim::gone("This chat's key file is missing or unreadable."));
    }
    let key = match &start {
        Some(start) => {
            keys.retain(|(_, k)| key_start(k).as_deref() == Some(start.as_str()));
            keys.into_iter().next()
        }
        None if keys.len() == 1 => keys.into_iter().next(),
        None => None,
    }
    .map(|(_, k)| k)
    .ok_or_else(|| shim::gone("This chat's key file is for another generation of the process."))?;
    let key_domain = text_of(&key["pidDomain"]);
    if let (Some(want), Some(have)) = (&expected.domain, &key_domain)
        && want != have
    {
        return Err(changed());
    }
    let token = key["peerToken"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| shim::gone("This chat's key file has no token."))?
        .to_string();
    let domain = expected.domain.clone().or(key_domain);
    if !process_matches(pid, start.as_deref(), domain.as_deref()) {
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
    // Short reads let the ACK reader check its absolute deadline.
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    let reader = stream.try_clone()?;
    Ok((Box::new(stream), Box::new(reader)))
}

#[cfg(windows)]
fn connect(path: &str) -> std::io::Result<(Writer, Reader)> {
    // Overlapped pipe I/O with a deadline per stage (see `win`).
    win::connect(path, ACK_WINDOW)
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
    /// Accepted or held by the chat's own queue; consumption is not confirmed.
    Queued(String),
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
        return Some(Verdict::Queued(format!(
            "held by the chat's own queue{}",
            if reason.is_empty() {
                String::new()
            } else {
                format!(": {reason}")
            }
        )));
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
            "delivered" | "injected" | "received" => Some(Verdict::Delivered(reason)),
            "accepted" | "ok" | "queued" => Some(Verdict::Queued(reason)),
            "held" | "hold" => Some(Verdict::Queued(format!(
                "held by the chat's own queue: {reason}"
            ))),
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
/// uses, with any wrapper tag inside the text neutralised. The sender attributes
/// are labels the sending side chose; `provenance` says they are not verified.
fn wrap_content(from: &str, session: &str, name: &str, text: &str) -> String {
    let text = text
        .replace(TAG_OPEN, "&lt;cross-session-message")
        .replace(TAG_CLOSE, "&lt;/cross-session-message>");
    format!(
        "{TAG_OPEN} from=\"{}\" from-session=\"{}\" from-name=\"{}\" from-mode=\"bridge\" \
         provenance=\"agent-unverified\">\n{text}\n{TAG_CLOSE}",
        attr_safe(from),
        attr_safe(session),
        attr_safe(name),
    )
}

/// Attributes and body of a `<cross-session-message ...>` wrapper, if `content` is one.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
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
    let files = match open_session(pid, &expectation(session))? {
        Ok(files) => files,
        Err(reason) => return Ok(shim::unsupported(&reason)),
    };
    let peer_key = shim::env_peer_key(env);
    let peer_key = peer_key.as_str();
    let route = match reply_socket {
        Some(address) => ReplyRoute::Listening(address.to_string()),
        None => reply_route(peer_key),
    };
    let short: String = peer_key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect();
    let sender = format!("{} via Pulse", shim::env_from_name(env));
    let session_id = format!("pulse-{short}");
    // A frame without a reply address is dropped by the chat without a word. When no
    // reply route exists, a stable placeholder is advertised so the message is still
    // deliverable; the receipt says replies are unavailable, and a chat that answers
    // the placeholder reaches nobody.
    let mut reply_note: Option<String> = None;
    let reply_from = Some(match route {
        ReplyRoute::Listening(address) => format!("uds:{address}"),
        ReplyRoute::Unavailable(why) => {
            eprintln!("pulse bridge: reply route unavailable ({why}); advertising a placeholder");
            reply_note = Some(format!("Replies are unavailable ({why})."));
            if cfg!(windows) {
                r"uds:\\.\pipe\pulse-bridge-noreply".to_string()
            } else {
                "uds:/tmp/pulse-bridge-noreply.sock".to_string()
            }
        }
    });
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
        std::io::ErrorKind::TimedOut => shim::io(&format!("Couldn't reach the chat: {e}")),
        _ => shim::io(&format!("Couldn't reach the chat: {}", e.kind())),
    })?;

    for line in lines {
        writer
            .write_all(line.as_bytes())
            .and_then(|_| writer.flush())
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::TimedOut => {
                    shim::io(&format!("The chat did not take the message: {e}"))
                }
                _ => shim::gone("The chat closed the connection while Pulse was sending."),
            })?;
    }

    let (tx, rx) = mpsc::channel::<Value>();
    // Ends at the chat's close, after 10 s or after 64 KiB, whichever comes first.
    std::thread::spawn(move || {
        read_lines(
            reader,
            Instant::now() + ACK_DEADLINE,
            ACK_MAX_BYTES,
            true,
            |line| match serde_json::from_str::<Value>(line.trim()) {
                Ok(frame) => tx.send(frame).is_ok(),
                Err(_) => true,
            },
        );
    });
    let deadline = Instant::now() + ACK_WINDOW;
    let mut heard_anything = false;
    let finish = |receipt: Receipt| -> Receipt {
        match &reply_note {
            Some(note) => Receipt {
                detail: format!("{} {note}", receipt.detail).trim().to_string(),
                ..receipt
            },
            None => receipt,
        }
    };
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(frame) => {
                heard_anything = true;
                match classify(&frame) {
                    Some(Verdict::Delivered(r)) => return Ok(finish(shim::delivered(&r))),
                    Some(Verdict::Queued(r)) => return Ok(finish(shim::queued(&r))),
                    Some(Verdict::Refused(r)) => {
                        let r = if r.is_empty() {
                            "refused by that session's settings".to_string()
                        } else {
                            r
                        };
                        return Ok(finish(shim::refused(&r)));
                    }
                    Some(Verdict::Protocol(r)) => return Ok(shim::unsupported(&r)),
                    Some(Verdict::AuthRejected(r)) => return Err(shim::auth_rejected(&r)),
                    None => {}
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Ok(finish(shim::sent(
                    "Sent; the chat gave no verdict (silence is normal), so it is unconfirmed.",
                )));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return if heard_anything || !cfg!(windows) {
                    Ok(finish(shim::sent(
                        "Sent; the chat closed the connection without a verdict.",
                    )))
                } else {
                    Err(shim::auth_rejected(
                        "The chat closed the connection right after the handshake.",
                    ))
                };
            }
        }
    }
}

/// Lines from `reader` until `deadline`, end of input, an error, `cap` bytes, or
/// `on_line` returning false. With `retry_idle`, a read timeout is not an end.
fn read_lines(
    mut reader: impl Read,
    deadline: Instant,
    cap: usize,
    retry_idle: bool,
    mut on_line: impl FnMut(&str) -> bool,
) {
    use std::io::ErrorKind::{Interrupted, TimedOut, WouldBlock};
    let mut chunk = [0u8; 4096];
    let mut pending: Vec<u8> = Vec::new();
    let mut total = 0usize;
    while Instant::now() < deadline && total < cap {
        let n = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == Interrupted => continue,
            Err(e) if retry_idle && matches!(e.kind(), TimedOut | WouldBlock) => continue,
            Err(_) => break,
        };
        total += n;
        pending.extend_from_slice(&chunk[..n]);
        while let Some(end) = pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            if !on_line(&String::from_utf8_lossy(&line)) {
                return;
            }
        }
    }
    if !pending.is_empty() {
        on_line(&String::from_utf8_lossy(&pending));
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
    /// The frame's own message id.
    pub msg_id: String,
    /// The id of the message being answered, when the frame names one (`in_reply_to`,
    /// `reply_to` or `replyTo`; Claude's peer protocol is not known to send any).
    pub in_reply_to: Option<String>,
}

type OnReply = Arc<dyn Fn(ReplyMessage) + Send + Sync>;
type IsKnown = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Listens on one Unix socket (a named pipe on Windows) per remote peer and
/// turns frames from known local chats into `ReplyMessage`s.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
pub struct ReplyHub {
    on_reply: OnReply,
    is_known: IsKnown,
    stop: Arc<AtomicBool>,
    paths: Mutex<HashMap<String, String>>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

/// Where a reply to a bridged message will arrive, or why it can't.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyRoute {
    /// A socket (named pipe on Windows) this process listens on.
    Listening(String),
    /// No reply route could be made; the reason.
    Unavailable(String),
}

/// Open reply connections across every hub, and the guard that counts one.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
static OPEN_REPLIES: AtomicUsize = AtomicUsize::new(0);

#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
struct ConnectionSlot;

#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
impl ConnectionSlot {
    /// A slot, or None (logged) when 64 connections are already open.
    fn take() -> Option<ConnectionSlot> {
        let taken = OPEN_REPLIES
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_REPLY_CONNECTIONS).then_some(n + 1)
            })
            .is_ok();
        if taken {
            Some(ConnectionSlot)
        } else {
            eprintln!("pulse bridge: too many open reply connections; refusing one");
            None
        }
    }
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        OPEN_REPLIES.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Wait up to `limit` for the threads to end; one still running is left behind.
fn join_bounded(handles: Vec<std::thread::JoinHandle<()>>, limit: Duration) {
    let deadline = Instant::now() + limit;
    for handle in handles {
        while !handle.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if handle.is_finished() {
            let _ = handle.join();
        }
    }
}

/// Which listener generation owns each Unix socket path, so a stopped listener never
/// unlinks the socket a newer one bound at the same path.
#[cfg(unix)]
static LISTENERS: Mutex<Vec<(PathBuf, u64)>> = Mutex::new(Vec::new());
#[cfg(unix)]
static LISTENER_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

static HUB: OnceLock<Mutex<Option<Arc<ReplyHub>>>> = OnceLock::new();

fn hub_slot() -> &'static Mutex<Option<Arc<ReplyHub>>> {
    HUB.get_or_init(|| Mutex::new(None))
}

/// The hub `deliver` takes reply addresses from. The hub sets it while sharing runs.
pub fn set_reply_hub(hub: Option<Arc<ReplyHub>>) {
    let old = match hub_slot().lock() {
        Ok(mut slot) => std::mem::replace(&mut *slot, hub),
        Err(_) => return,
    };
    // Stopped outside the lock: it waits for the old listeners' threads.
    if let Some(old) = old {
        old.stop();
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

/// The reply pipe for one remote peer on Windows. `LOCAL\` is the same session-local
/// pipe namespace Claude's own `cc-msg-<hash>` pipes use.
#[cfg(windows)]
pub fn reply_pipe_name(peer_key: &str) -> String {
    format!(r"\\.\pipe\LOCAL\pulse-bridge-{}", fnv(peer_key))
}

/// The address a reply from `peer_key` goes to, listening on it first; None
/// when no reply hub is running or this system can't listen.
pub fn reply_address(peer_key: &str) -> Option<String> {
    current_hub()?.address_for(peer_key)
}

/// `reply_address` that says why there is no route.
pub fn reply_route(peer_key: &str) -> ReplyRoute {
    match current_hub() {
        Some(hub) => hub.route_for(peer_key),
        None => ReplyRoute::Unavailable("no reply hub is running".to_string()),
    }
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
            threads: Mutex::new(Vec::new()),
        })
    }

    /// Stop listening and wait up to two seconds for the listener threads, which
    /// remove their own sockets (only if still theirs) as they end.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let handles = self
            .threads
            .lock()
            .map(|mut t| std::mem::take(&mut *t))
            .unwrap_or_default();
        join_bounded(handles, STOP_JOIN);
    }

    /// The socket path replies from this peer's messages should go to, listening
    /// on it first. None when no reply route can be made (see `route_for`).
    pub fn address_for(self: &Arc<Self>, peer_key: &str) -> Option<String> {
        match self.route_for(peer_key) {
            ReplyRoute::Listening(address) => Some(address),
            ReplyRoute::Unavailable(_) => None,
        }
    }

    /// `address_for`, saying why when the hub can't listen.
    pub fn route_for(self: &Arc<Self>, peer_key: &str) -> ReplyRoute {
        if self.stop.load(Ordering::Relaxed) {
            return ReplyRoute::Unavailable("the reply hub is stopped".to_string());
        }
        #[cfg(unix)]
        {
            let Ok(mut paths) = self.paths.lock() else {
                return ReplyRoute::Unavailable("the reply hub is unusable".to_string());
            };
            if let Some(path) = paths.get(peer_key) {
                return ReplyRoute::Listening(path.clone());
            }
            let path = reply_socket_path(peer_key);
            if let Err(e) = self.listen(peer_key.to_string(), &path) {
                return ReplyRoute::Unavailable(format!("couldn't listen: {}", e.kind()));
            }
            let text = path.to_string_lossy().into_owned();
            paths.insert(peer_key.to_string(), text.clone());
            ReplyRoute::Listening(text)
        }
        #[cfg(windows)]
        {
            let Ok(mut paths) = self.paths.lock() else {
                return ReplyRoute::Unavailable("the reply hub is unusable".to_string());
            };
            if let Some(name) = paths.get(peer_key) {
                return ReplyRoute::Listening(name.clone());
            }
            let name = reply_pipe_name(peer_key);
            if let Err(e) = self.listen_pipe(peer_key.to_string(), &name) {
                return ReplyRoute::Unavailable(format!("couldn't listen: {}", e.kind()));
            }
            paths.insert(peer_key.to_string(), name.clone());
            ReplyRoute::Listening(name)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = peer_key;
            ReplyRoute::Unavailable("this system can't listen".to_string())
        }
    }

    /// One server per peer: an instance waits for a client, and a fresh instance is
    /// created as soon as one connects so a second reply never finds no pipe.
    #[cfg(windows)]
    fn listen_pipe(self: &Arc<Self>, peer_key: String, name: &str) -> std::io::Result<()> {
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let descriptor = win::descriptor()?;
        let first = win::create_instance(&wide, &descriptor, true)?;
        let hub = Arc::clone(self);
        let handle = std::thread::spawn(move || {
            let mut waiting = Some(first);
            while !hub.stop.load(Ordering::Relaxed) {
                let pipe = match waiting.take() {
                    Some(pipe) => pipe,
                    None => match win::create_instance(&wide, &descriptor, false) {
                        Ok(pipe) => pipe,
                        Err(_) => {
                            std::thread::sleep(Duration::from_millis(500));
                            continue;
                        }
                    },
                };
                match win::accept(&pipe, &hub.stop) {
                    win::Accepted::Connected => {
                        let Some(slot) = ConnectionSlot::take() else {
                            win::disconnect(&pipe);
                            continue;
                        };
                        let hub = Arc::clone(&hub);
                        let key = peer_key.clone();
                        std::thread::spawn(move || {
                            let _slot = slot;
                            let pipe = Arc::new(pipe);
                            hub.handle_lines(win::PipeReader::server(Arc::clone(&pipe)), &key);
                            win::disconnect(&pipe);
                        });
                    }
                    win::Accepted::Stopped => break,
                    win::Accepted::Failed => {
                        win::disconnect(&pipe);
                        waiting = Some(pipe);
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
            }
        });
        if let Ok(mut threads) = self.threads.lock() {
            threads.push(handle);
        }
        Ok(())
    }

    #[cfg(unix)]
    fn listen(self: &Arc<Self>, peer_key: String, path: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        // The registry lock is held from claiming the path to binding it, so an older
        // listener's cleanup never sees the new socket as its own.
        let generation = LISTENER_GENERATION.fetch_add(1, Ordering::Relaxed);
        let listener = {
            let mut owners = LISTENERS.lock().unwrap_or_else(|e| e.into_inner());
            let _ = std::fs::remove_file(path);
            let listener = UnixListener::bind(path)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            listener.set_nonblocking(true)?;
            owners.retain(|(p, _)| p != path);
            owners.push((path.to_path_buf(), generation));
            listener
        };
        let hub = Arc::clone(self);
        let path = path.to_path_buf();
        let handle = std::thread::spawn(move || {
            while !hub.stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let Some(slot) = ConnectionSlot::take() else {
                            continue;
                        };
                        let hub = Arc::clone(&hub);
                        let key = peer_key.clone();
                        std::thread::spawn(move || {
                            let _slot = slot;
                            hub.handle(stream, &key);
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(500)),
                }
            }
            // Unlink only a socket this generation still owns.
            let mut owners = LISTENERS.lock().unwrap_or_else(|e| e.into_inner());
            if owners.iter().any(|(p, g)| *p == path && *g == generation) {
                owners.retain(|(p, _)| *p != path);
                let _ = std::fs::remove_file(&path);
            }
        });
        if let Ok(mut threads) = self.threads.lock() {
            threads.push(handle);
        }
        Ok(())
    }

    #[cfg(unix)]
    fn handle(&self, stream: std::os::unix::net::UnixStream, peer_key: &str) {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        self.handle_lines(stream, peer_key);
    }

    /// Frames from one connection, as `ReplyMessage`s for known local chats.
    #[cfg(any(unix, windows))]
    fn handle_lines(&self, reader: impl Read, peer_key: &str) {
        let deadline = Instant::now() + REPLY_DEADLINE;
        read_lines(reader, deadline, MAX_REPLY_BYTES, false, |line| {
            self.handle_line(line, peer_key);
            !self.stop.load(Ordering::Relaxed)
        });
    }

    /// One frame, as a `ReplyMessage` when it is from a known local chat.
    #[cfg(any(unix, windows))]
    fn handle_line(&self, line: &str, peer_key: &str) {
        let Ok(frame) = serde_json::from_str::<Value>(line.trim()) else {
            return;
        };
        // The auth frame, if any, is ignored: what counts is that the sender is a known local chat.
        if frame["type"] != "user" {
            return;
        }
        let Some(raw) = content_text(&frame["message"]["content"]) else {
            return;
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
            return;
        }
        (self.on_reply)(ReplyMessage {
            peer_key: peer_key.to_string(),
            from_session_id,
            from_name: pick("from-name", "from_name"),
            text,
            msg_id: frame["msg_id"].as_str().unwrap_or("").to_string(),
            in_reply_to: ["in_reply_to", "reply_to", "replyTo"]
                .iter()
                .find_map(|k| frame[*k].as_str())
                .filter(|id| !id.is_empty())
                .map(str::to_string),
        });
    }
}

/// A message's content as plain text: a string, or the text blocks of an array.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
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

// ---- Windows named pipes ----------------------------------------------------------

/// Windows pipe plumbing: bounded overlapped client I/O to a chat, the reply
/// pipe server with a current-user-only DACL, and process identity. Patterned on
/// `ipc::windows`; every overlapped operation is cancelled with `CancelIoEx` when
/// its deadline passes, and a buffer the kernel never released is leaked rather
/// than freed.
#[cfg(windows)]
mod win {
    use super::{Reader, Writer};
    use ::windows::Win32::Foundation::{
        CloseHandle, ERROR_BROKEN_PIPE, ERROR_INSUFFICIENT_BUFFER, ERROR_IO_PENDING, ERROR_NO_DATA,
        ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, FILETIME, HANDLE, HLOCAL,
        LocalFree, WAIT_OBJECT_0, WIN32_ERROR,
    };
    use ::windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use ::windows::Win32::Security::{
        GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use ::windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_GENERIC_READ,
        FILE_GENERIC_WRITE, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile,
        WriteFile,
    };
    use ::windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use ::windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT, WaitNamedPipeW,
    };
    use ::windows::Win32::System::Threading::{
        CreateEventW, GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken,
        PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
    };
    use ::windows::core::{HRESULT, PCWSTR, PWSTR};
    use std::io::{self, Read, Write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
    const WRITE_TIMEOUT: Duration = Duration::from_secs(3);
    /// How long the server waits for the next byte of a reply before giving up.
    const SERVER_READ_IDLE: Duration = Duration::from_secs(5);
    /// How long the kernel gets to confirm a cancelled operation.
    const CANCEL_DRAIN_MS: u32 = 5000;
    const PIPE_BUFFER: u32 = 64 * 1024;

    fn is(error: &::windows::core::Error, code: WIN32_ERROR) -> bool {
        error.code() == HRESULT::from_win32(code.0)
    }

    fn os_error(error: &::windows::core::Error) -> io::Error {
        let code = error.code().0 as u32;
        if code >> 16 == 0x8007 {
            io::Error::from_raw_os_error((code & 0xFFFF) as i32)
        } else {
            io::Error::other(error.to_string())
        }
    }

    fn is_closed(error: &::windows::core::Error) -> bool {
        [ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED, ERROR_NO_DATA]
            .iter()
            .any(|code| is(error, *code))
    }

    fn millis(wait: Duration) -> u32 {
        u32::try_from(wait.as_millis()).map_or(u32::MAX - 1, |m| m.min(u32::MAX - 1))
    }

    /// Owned kernel handle, closed on drop. A pipe handle may be used from several
    /// threads at once (one read, one write), each with its own OVERLAPPED.
    pub(super) struct Handle(HANDLE);
    // SAFETY: a kernel handle is a plain integer token; the calls made through it
    // are thread-safe.
    unsafe impl Send for Handle {}
    // SAFETY: see above.
    unsafe impl Sync for Handle {}
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: owned by this wrapper and closed exactly once.
                let _ = unsafe { CloseHandle(self.0) };
            }
        }
    }

    /// One in-flight overlapped operation: event, OVERLAPPED and staging buffer at
    /// a fixed heap address the kernel may keep using until it confirms release.
    struct Ctx {
        overlapped: OVERLAPPED,
        event: Handle,
        buf: Vec<u8>,
    }

    fn new_ctx(len: usize) -> io::Result<Box<Ctx>> {
        // SAFETY: manual-reset, initially non-signalled, unnamed event.
        let event = unsafe { CreateEventW(None, true, false, PCWSTR(std::ptr::null())) }
            .map(Handle)
            .map_err(|e| os_error(&e))?;
        Ok(Box::new(Ctx {
            overlapped: OVERLAPPED {
                hEvent: event.0,
                ..OVERLAPPED::default()
            },
            event,
            buf: vec![0u8; len],
        }))
    }

    /// Cancel `ctx`'s operation and wait for the kernel to let go of it. If it never
    /// does, the context is leaked and false is returned.
    fn cancel(pipe: HANDLE, ctx: Box<Ctx>) -> Option<Box<Ctx>> {
        // SAFETY: the operation was started on `pipe` with this OVERLAPPED.
        let _ = unsafe { CancelIoEx(pipe, Some(&ctx.overlapped as *const OVERLAPPED)) };
        // SAFETY: the event is owned by `ctx`.
        let settled = unsafe { WaitForSingleObject(ctx.event.0, CANCEL_DRAIN_MS) } == WAIT_OBJECT_0;
        if settled {
            Some(ctx)
        } else {
            let _ = Box::leak(ctx); // the kernel may still own it
            None
        }
    }

    enum Op<'a> {
        Read(&'a mut [u8]),
        Write(&'a [u8]),
    }

    /// One overlapped read or write bounded by `timeout`; a timeout is an
    /// `ErrorKind::TimedOut` error saying `<what> timed out`. A closed pipe reads as
    /// end of file and writes as `BrokenPipe`.
    fn transfer(pipe: HANDLE, op: Op<'_>, timeout: Duration, what: &str) -> io::Result<usize> {
        let (writing, len) = match &op {
            Op::Read(buf) => (false, buf.len()),
            Op::Write(data) => (true, data.len()),
        };
        if len == 0 {
            return Ok(0);
        }
        let mut ctx = new_ctx(len)?;
        if let Op::Write(data) = &op {
            ctx.buf.copy_from_slice(data);
        }
        let timed_out = || io::Error::new(io::ErrorKind::TimedOut, format!("{what} timed out"));
        let settle = |error: &::windows::core::Error| -> io::Result<usize> {
            if !is_closed(error) {
                Err(os_error(error))
            } else if writing {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "the chat closed the pipe",
                ))
            } else {
                Ok(0)
            }
        };
        // SAFETY: `ctx` stays at a fixed heap address; it is dropped only after the
        // kernel has released it, and is leaked (in `cancel`) when it has not.
        let started = unsafe {
            if writing {
                WriteFile(
                    pipe,
                    Some(ctx.buf.as_slice()),
                    None,
                    Some(&mut ctx.overlapped as *mut OVERLAPPED),
                )
            } else {
                ReadFile(
                    pipe,
                    Some(ctx.buf.as_mut_slice()),
                    None,
                    Some(&mut ctx.overlapped as *mut OVERLAPPED),
                )
            }
        };
        match started {
            Ok(()) => {}
            Err(e) if is(&e, ERROR_IO_PENDING) => {}
            Err(e) => return settle(&e),
        }
        // SAFETY: the event is owned by `ctx`.
        let wait = unsafe { WaitForSingleObject(ctx.event.0, millis(timeout)) };
        let finished = if wait == WAIT_OBJECT_0 {
            ctx
        } else {
            let Some(ctx) = cancel(pipe, ctx) else {
                return Err(timed_out());
            };
            ctx
        };
        let mut done = 0u32;
        // SAFETY: the operation is complete or cancelled; this only reads its status.
        let outcome = unsafe { GetOverlappedResult(pipe, &finished.overlapped, &mut done, false) };
        let cancelled_early = wait != WAIT_OBJECT_0;
        match outcome {
            Ok(()) if done > 0 || !cancelled_early => {
                if let Op::Read(buf) = op {
                    buf[..done as usize].copy_from_slice(&finished.buf[..done as usize]);
                }
                Ok(done as usize)
            }
            Ok(()) => Err(timed_out()),
            Err(_) if cancelled_early => Err(timed_out()),
            Err(e) => settle(&e),
        }
    }

    // ---- client ----

    pub(super) struct PipeWriter(Arc<Handle>);
    impl Write for PipeWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            transfer(self.0.0, Op::Write(buf), WRITE_TIMEOUT, "write")
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Reads from a pipe. A client reader has an overall deadline that starts at its
    /// first read; a server reader waits at most `per_read` for each chunk.
    pub(super) struct PipeReader {
        pipe: Arc<Handle>,
        per_read: Duration,
        overall: Option<Duration>,
        deadline: Option<Instant>,
    }

    impl PipeReader {
        pub(super) fn server(pipe: Arc<Handle>) -> Self {
            Self {
                pipe,
                per_read: SERVER_READ_IDLE,
                overall: None,
                deadline: None,
            }
        }
    }

    impl Read for PipeReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let limit = match self.overall {
                Some(total) => {
                    let deadline = *self.deadline.get_or_insert_with(|| Instant::now() + total);
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "read timed out"));
                    }
                    left.min(self.per_read)
                }
                None => self.per_read,
            };
            transfer(self.pipe.0, Op::Read(buf), limit, "read")
        }
    }

    /// Open the chat's pipe: connect within 3 s (`WaitNamedPipeW` while it is busy),
    /// then hand back a writer with a 3 s write deadline and a reader bounded by
    /// `ack` (plus a margin, so the caller's own ACK timer always fires first).
    pub(super) fn connect(path: &str, ack: Duration) -> io::Result<(Writer, Reader)> {
        let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let handle = loop {
            // SAFETY: `wide` is NUL-terminated.
            let opened = unsafe {
                CreateFileW(
                    PCWSTR(wide.as_ptr()),
                    FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    None,
                )
            };
            match opened {
                Ok(handle) => break Handle(handle),
                Err(e) if is(&e, ERROR_PIPE_BUSY) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "connect timed out"));
                    }
                    // SAFETY: `wide` is NUL-terminated; this blocks at most `left`.
                    let available = unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), millis(left)) };
                    if !available.as_bool() {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
                Err(e) => return Err(os_error(&e)),
            }
        };
        let pipe = Arc::new(handle);
        let reader = PipeReader {
            pipe: Arc::clone(&pipe),
            per_read: ack + Duration::from_millis(500),
            overall: Some(ack + Duration::from_millis(500)),
            deadline: None,
        };
        Ok((Box::new(PipeWriter(pipe)), Box::new(reader)))
    }

    // ---- server ----

    /// Owned security descriptor from the SDDL converter.
    pub(super) struct Descriptor(PSECURITY_DESCRIPTOR);
    // SAFETY: an immutable, self-contained descriptor block.
    unsafe impl Send for Descriptor {}
    impl Drop for Descriptor {
        fn drop(&mut self) {
            if !self.0.0.is_null() {
                // SAFETY: allocated with LocalAlloc by the converter; freed once.
                let _ = unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
            }
        }
    }

    fn user_sid_string() -> io::Result<String> {
        let mut token = HANDLE(std::ptr::null_mut());
        // SAFETY: pseudo handle for this process; the token is closed by `Handle`.
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
            .map_err(|e| os_error(&e))?;
        let token = Handle(token);
        let mut needed = 0u32;
        // SAFETY: size probe with a null buffer.
        match unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut needed) } {
            Err(e) if is(&e, ERROR_INSUFFICIENT_BUFFER) && needed > 0 => {}
            Err(e) => return Err(os_error(&e)),
            Ok(()) => return Err(io::Error::other("empty token user")),
        }
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        let mut written = 0u32;
        // SAFETY: the buffer holds at least `needed` bytes and is pointer-aligned.
        unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                Some(buffer.as_mut_ptr().cast::<core::ffi::c_void>()),
                needed,
                &mut written,
            )
        }
        .map_err(|e| os_error(&e))?;
        // SAFETY: filled by GetTokenInformation(TokenUser).
        let sid = unsafe { (*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid };
        let mut text = PWSTR(std::ptr::null_mut());
        // SAFETY: `sid` is valid; the string is LocalAlloc'd and freed below.
        unsafe { ConvertSidToStringSidW(sid, &mut text) }.map_err(|e| os_error(&e))?;
        // SAFETY: `text` is a NUL-terminated UTF-16 string.
        let value = unsafe {
            let mut len = 0usize;
            while *text.0.add(len) != 0 {
                len += 1;
            }
            let value = String::from_utf16_lossy(std::slice::from_raw_parts(text.0, len));
            let _ = LocalFree(Some(HLOCAL(text.0.cast::<core::ffi::c_void>())));
            value
        };
        Ok(value)
    }

    /// DACL: protected, GENERIC_ALL for the current user and SYSTEM only, owner the
    /// current user.
    pub(super) fn descriptor() -> io::Result<Descriptor> {
        let sid = user_sid_string()?;
        let sddl: Vec<u16> = format!("O:{sid}D:P(A;;GA;;;{sid})(A;;GA;;;SY)")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut raw = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sddl` is NUL-terminated; the result is freed by `Descriptor`.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut raw,
                None,
            )
        }
        .map_err(|e| os_error(&e))?;
        Ok(Descriptor(raw))
    }

    /// A byte-mode, overlapped, local-only instance of the pipe. The first instance
    /// refuses to share the name, so another process can't squat on it.
    pub(super) fn create_instance(
        wide_name: &[u16],
        descriptor: &Descriptor,
        first: bool,
    ) -> io::Result<Handle> {
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0.0,
            bInheritHandle: false.into(),
        };
        let open = if first {
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED
        } else {
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED
        };
        // SAFETY: `wide_name` is NUL-terminated; `attributes` outlives the call.
        let pipe = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide_name.as_ptr()),
                open,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS | PIPE_WAIT,
                255,
                PIPE_BUFFER,
                PIPE_BUFFER,
                0,
                Some(&attributes as *const SECURITY_ATTRIBUTES),
            )
        };
        if pipe.is_invalid() {
            return Err(os_error(&::windows::core::Error::from_win32()));
        }
        Ok(Handle(pipe))
    }

    pub(super) enum Accepted {
        Connected,
        Stopped,
        Failed,
    }

    /// Wait for a client, checking `stop` every 100 ms.
    pub(super) fn accept(pipe: &Handle, stop: &AtomicBool) -> Accepted {
        let Ok(mut ctx) = new_ctx(0) else {
            return Accepted::Failed;
        };
        // SAFETY: fixed heap address; released by the kernel before it is dropped,
        // else leaked in `cancel`.
        match unsafe { ConnectNamedPipe(pipe.0, Some(&mut ctx.overlapped as *mut OVERLAPPED)) } {
            Ok(()) => {}
            Err(e) if is(&e, ERROR_PIPE_CONNECTED) => return Accepted::Connected,
            Err(e) if is(&e, ERROR_IO_PENDING) => {}
            Err(_) => return Accepted::Failed,
        }
        loop {
            // SAFETY: the event is owned by `ctx`.
            let wait = unsafe { WaitForSingleObject(ctx.event.0, 100) };
            if wait == WAIT_OBJECT_0 {
                let mut done = 0u32;
                // SAFETY: signalled, so the kernel is finished with `ctx`.
                let outcome =
                    unsafe { GetOverlappedResult(pipe.0, &ctx.overlapped, &mut done, false) };
                return if outcome.is_ok() {
                    Accepted::Connected
                } else {
                    Accepted::Failed
                };
            }
            if wait != ::windows::Win32::Foundation::WAIT_TIMEOUT {
                let _ = cancel(pipe.0, ctx);
                return Accepted::Failed;
            }
            if stop.load(Ordering::Relaxed) {
                let _ = cancel(pipe.0, ctx);
                return Accepted::Stopped;
            }
        }
    }

    pub(super) fn disconnect(pipe: &Handle) {
        // SAFETY: a valid server pipe handle.
        let _ = unsafe { DisconnectNamedPipe(pipe.0) };
    }

    // ---- process identity ----

    /// When process `pid` started, in seconds since the Unix epoch.
    pub(super) fn creation_epoch_secs(pid: u32) -> Option<f64> {
        // SAFETY: a plain OpenProcess for limited query rights; closed by `Handle`.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let process = Handle(process);
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        // SAFETY: a valid process handle and four live FILETIME out-parameters.
        unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) }
            .ok()?;
        let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        Some((ticks as f64 - 116_444_736_000_000_000.0) / 1e7)
    }

    /// A recorded `procStart` as epoch seconds. Claude's Windows form isn't fixed
    /// here, so this takes a number (seconds, milliseconds, microseconds, FILETIME
    /// ticks or nanoseconds, told apart by size), an ISO-8601 time, or the
    /// `Www Mon DD HH:MM:SS YYYY` form macOS records. Anything else is unknown.
    pub(super) fn parse_start(text: &str) -> Option<f64> {
        let t = text.trim();
        if !t.is_ascii() {
            return None;
        }
        if let Ok(v) = t.parse::<f64>() {
            return Some(if v >= 5e17 {
                v / 1e9
            } else if v >= 1e16 {
                (v - 116_444_736_000_000_000.0) / 1e7
            } else if v >= 1e14 {
                v / 1e6
            } else if v >= 1e11 {
                v / 1e3
            } else {
                v
            });
        }
        parse_iso(t).or_else(|| parse_ctime(t))
    }

    fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
        let y = if month <= 2 { year - 1 } else { year };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    fn epoch(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> i64 {
        days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s
    }

    fn parse_iso(t: &str) -> Option<f64> {
        let b = t.as_bytes();
        if b.len() < 19
            || b[4] != b'-'
            || b[7] != b'-'
            || (b[10] != b'T' && b[10] != b' ')
            || b[13] != b':'
            || b[16] != b':'
        {
            return None;
        }
        let num = |range: std::ops::Range<usize>| t.get(range)?.parse::<i64>().ok();
        let base = epoch(
            num(0..4)?,
            num(5..7)?,
            num(8..10)?,
            num(11..13)?,
            num(14..16)?,
            num(17..19)?,
        );
        let mut rest = &t[19..];
        let mut fraction = 0.0;
        if let Some(after) = rest.strip_prefix('.') {
            let digits = after.chars().take_while(char::is_ascii_digit).count();
            fraction = format!("0.{}", &after[..digits]).parse().ok()?;
            rest = &after[digits..];
        }
        let offset = match rest {
            "" | "Z" | "z" => 0,
            zone if zone.starts_with(['+', '-']) => {
                let sign = if zone.starts_with('-') { -1 } else { 1 };
                let digits: String = zone[1..].chars().filter(char::is_ascii_digit).collect();
                if digits.len() < 2 {
                    return None;
                }
                let hours: i64 = digits[..2].parse().ok()?;
                let minutes: i64 = digits.get(2..4).map_or(Some(0), |m| m.parse().ok())?;
                sign * (hours * 3600 + minutes * 60)
            }
            _ => return None,
        };
        Some((base - offset) as f64 + fraction)
    }

    fn parse_ctime(t: &str) -> Option<f64> {
        let parts: Vec<&str> = t.split_whitespace().collect();
        let [_, month, day, clock, year] = parts[..] else {
            return None;
        };
        let months = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let month = i64::try_from(months.iter().position(|m| *m == month)?).ok()? + 1;
        let mut hms = clock.split(':').map(|p| p.parse::<i64>().ok());
        let (h, mi, s) = (hms.next()??, hms.next()??, hms.next()??);
        Some(epoch(year.parse().ok()?, month, day.parse().ok()?, h, mi, s) as f64)
    }
}
