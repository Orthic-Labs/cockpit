//! Linked computers: how this computer reaches another one's `pulse` over ssh.
//!
//! A link is `{device, ssh, pulse, shell, deviceId?}`: the name chats are shown under
//! ("<chat> on <device>"), the ssh destination (a host alias from
//! `~/.ssh/config`, or `user@host`), the path of `pulse` on that computer, the
//! family of shell ssh lands in there (`posix` or `powershell`, found when the link
//! is made) and the peer's stable device id (learned from its chat listing and then
//! pinned). Links live in `<state>/bridge/links.json` and are made with
//! `pulse bridge link <device> <ssh-host> [--pulse PATH]`.
//!
//! Two remote commands exist: `pulse bridge peers --local --json` (that
//! computer's chats) and `pulse bridge post <base64 envelope>` (deliver one
//! message there). The remote command is one string, every word quoted for the
//! remote shell family, and the destination follows `--` so it cannot be read as
//! an ssh option. ssh must already work without a prompt (keys, BatchMode);
//! nothing here sets ssh up. `PULSE_BRIDGE_SSH` names another program to run
//! instead of `ssh` (tests); it gets the destination and the words as separate
//! arguments, unquoted. Each poll of a link is recorded in `links-status.json`.

use super::envelope::{Envelope, now_ms};
use super::roster::RosterEntry;
use super::store::Store;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long one remote command may take, ssh connection included.
pub const REMOTE_TIMEOUT: Duration = Duration::from_secs(25);
/// Most output kept from one remote command, per stream.
const OUTPUT_CAP: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub device: String,
    pub ssh: String,
    /// The `pulse` binary there; "pulse" means on PATH.
    #[serde(default = "default_pulse")]
    pub pulse: String,
    /// The shell ssh lands in there: "posix" (default) or "powershell".
    #[serde(default = "default_shell")]
    pub shell: String,
    /// The peer's stable device id, learned from its chat listing and pinned.
    #[serde(default, rename = "deviceId", skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
}

impl Default for Link {
    fn default() -> Link {
        Link {
            device: String::new(),
            ssh: String::new(),
            pulse: default_pulse(),
            shell: default_shell(),
            device_id: None,
        }
    }
}

fn default_pulse() -> String {
    "pulse".to_string()
}

fn default_shell() -> String {
    "posix".to_string()
}

/// What `pulse bridge peers --local --json` prints on a linked computer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RemoteListing {
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub chats: Vec<RosterEntry>,
}

/// A linked computer's chats, or why they could not be listed.
#[derive(Debug, Clone)]
pub struct RemoteChats {
    pub link: Link,
    pub listing: Result<RemoteListing, String>,
}

/// How the last polls of a linked computer went.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkStatus {
    #[serde(default)]
    pub device: String,
    /// When a poll last succeeded (ms); none when it never has.
    #[serde(default)]
    pub last_ok_ms: Option<u64>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_error_ms: Option<u64>,
    /// When the link was last polled (ms); 0 when never.
    #[serde(default)]
    pub last_attempt_ms: u64,
}

impl LinkStatus {
    /// Whether the most recent poll succeeded.
    pub fn online(&self) -> bool {
        match (self.last_ok_ms, self.last_error_ms) {
            (Some(ok), Some(err)) => ok >= err,
            (Some(_), None) => true,
            _ => false,
        }
    }

    /// How long ago (ms) the link last answered, when it ever did.
    pub fn age_ms(&self, now: u64) -> Option<u64> {
        self.last_ok_ms.map(|t| now.saturating_sub(t))
    }
}

fn ssh_program() -> String {
    std::env::var("PULSE_BRIDGE_SSH")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "ssh".to_string())
}

/// An ssh destination is a host alias or `user@host`: never an option, never a command.
fn validate_ssh(ssh: &str) -> Result<(), String> {
    let ok = !ssh.is_empty()
        && !ssh.starts_with('-')
        && ssh.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '@' | '.' | '_' | '-' | ':' | '[' | ']' | '+' | '~')
        });
    if ok {
        Ok(())
    } else {
        Err("the ssh host must be a host alias or user@host (no spaces, ';' or leading '-')".into())
    }
}

fn validate_pulse(pulse: &str) -> Result<(), String> {
    if pulse.trim().is_empty() || pulse.contains(['\n', '\r', '\0']) {
        Err("the pulse path is empty or has a line break".to_string())
    } else {
        Ok(())
    }
}

/// `text` as one POSIX shell word.
fn posix_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// `text` as one PowerShell single-quoted string.
fn powershell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// The one thing PowerShell may expand: the installed-app candidate below.
fn powershell_pulse(pulse: &str) -> String {
    if pulse == PULSE_CANDIDATES[2] {
        format!("& \"{pulse}\"")
    } else {
        format!("& {}", powershell_quote(pulse))
    }
}

/// `<pulse> bridge <args…>` as one command string for the remote shell family.
fn remote_command(link: &Link, args: &[&str]) -> String {
    let powershell = link.shell == "powershell";
    let mut command = if powershell {
        powershell_pulse(&link.pulse)
    } else {
        posix_quote(&link.pulse)
    };
    command.push_str(" bridge");
    for arg in args {
        command.push(' ');
        command.push_str(&if powershell {
            powershell_quote(arg)
        } else {
            posix_quote(arg)
        });
    }
    command
}

/// Read a stream to its end on its own thread, keeping at most `OUTPUT_CAP` bytes.
fn drain<R: Read + Send + 'static>(stream: Option<R>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        if let Some(mut stream) = stream {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = stream.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let room = OUTPUT_CAP.saturating_sub(kept.len());
                kept.extend_from_slice(&chunk[..n.min(room)]);
            }
        }
        let _ = tx.send(String::from_utf8_lossy(&kept).into_owned());
    });
    rx
}

/// Run `<pulse> bridge <args…>` on the linked computer; stdout on exit 0.
fn run(link: &Link, args: &[&str], timeout: Duration) -> Result<String, String> {
    validate_ssh(&link.ssh)?;
    validate_pulse(&link.pulse)?;
    let program = ssh_program();
    let mut command = Command::new(&program);
    if program == "ssh" {
        command.args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "LogLevel=ERROR",
            "--",
        ]);
        command.arg(&link.ssh);
        command.arg(remote_command(link, args));
    } else {
        command.arg(&link.ssh);
        command.arg(&link.pulse).arg("bridge").args(args);
    }
    command
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
        .map_err(|e| format!("couldn't run ssh: {e}"))?;
    let out_rx = drain(child.stdout.take());
    let err_rx = drain(child.stderr.take());
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                // The pipes close with the child; a stray grandchild holding one
                // must not hold this call too.
                let _ = out_rx.recv_timeout(Duration::from_secs(2));
                let _ = err_rx.recv_timeout(Duration::from_secs(2));
                return Err(format!(
                    "{} did not answer within {} s",
                    link.device,
                    timeout.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("ssh failed: {e}")),
        }
    };
    let out = out_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();
    let err = err_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();
    if status.success() {
        Ok(out)
    } else {
        let detail = err.trim();
        let detail = if detail.is_empty() {
            out.trim().to_string()
        } else {
            detail.to_string()
        };
        Err(format!(
            "{} answered {}: {}",
            link.device,
            status.code().unwrap_or(-1),
            detail
        ))
    }
}

fn b64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Decode what `b64` made (plain base64, `=` padding).
pub fn unb64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

/// The envelope as the `post` argument.
pub fn encode_envelope(env: &Envelope) -> String {
    b64(env.to_json().as_bytes())
}

pub fn decode_envelope(text: &str) -> Result<Envelope, String> {
    let bytes = unb64(text.trim()).ok_or_else(|| "not base64".to_string())?;
    let json = String::from_utf8(bytes).map_err(|_| "not UTF-8".to_string())?;
    Envelope::from_json(&json).map_err(|e| e.to_string())
}

/// This computer's links.
pub fn all(store: &Store) -> Vec<Link> {
    store.links()
}

pub fn find(store: &Store, device: &str) -> Option<Link> {
    let wanted = device.trim().to_lowercase();
    store
        .links()
        .into_iter()
        .find(|l| l.device.to_lowercase() == wanted)
}

/// The link whose peer reported this stable device id.
pub fn find_by_device_id(store: &Store, device_id: &str) -> Option<Link> {
    store
        .links()
        .into_iter()
        .find(|l| l.device_id.as_deref() == Some(device_id))
}

/// Where `pulse` lives when the link does not say: PATH, then the installed app on a Mac,
/// then the installed app on Windows (PowerShell expands `$env:`; ssh to Windows lands in
/// PowerShell by default).
const PULSE_CANDIDATES: [&str; 3] = [
    "pulse",
    "/Applications/Pulse.app/Contents/Helpers/pulse",
    "$env:LOCALAPPDATA\\Programs\\Pulse\\Helpers\\pulse.exe",
];

/// Add or replace a link, then check it by listing that computer's chats. The remote
/// shell family is found by trying POSIX quoting first, then PowerShell, and the one
/// that answered is stored; with no explicit `pulse` path, so is the first candidate
/// that answers. The peer's device id, when it reports one, is pinned.
pub fn add(store: &Store, mut link: Link) -> Result<RemoteListing, String> {
    if link.device.trim().is_empty() || link.ssh.trim().is_empty() {
        return Err("a link needs a device name and an ssh host".to_string());
    }
    if link.device.contains(':') {
        return Err("the device name can't contain ':'".to_string());
    }
    validate_ssh(&link.ssh)?;
    validate_pulse(&link.pulse)?;
    link.device_id = None;
    let explicit = link.pulse != "pulse";
    let first_shell = if link.shell == "powershell" {
        "powershell"
    } else {
        "posix"
    };
    let other_shell = if first_shell == "posix" {
        "powershell"
    } else {
        "posix"
    };
    // The test hook ignores quoting, so one shell family is enough there.
    let shells: Vec<&str> = if ssh_program() == "ssh" {
        vec![first_shell, other_shell]
    } else {
        vec![first_shell]
    };
    let requested = link.pulse.clone();
    let mut found = None;
    let mut first_error = None;
    'search: for shell in shells {
        link.shell = shell.to_string();
        let candidates: Vec<&str> = if explicit {
            vec![requested.as_str()]
        } else if shell == "powershell" {
            vec![PULSE_CANDIDATES[0], PULSE_CANDIDATES[2]]
        } else {
            vec![PULSE_CANDIDATES[0], PULSE_CANDIDATES[1]]
        };
        for candidate in candidates {
            link.pulse = candidate.to_string();
            match list_chats_identified(&link) {
                Ok(answer) => {
                    found = Some(answer);
                    break 'search;
                }
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
    }
    let (listing, device_id) = found.ok_or_else(|| {
        let last = first_error.unwrap_or_default();
        if explicit {
            last
        } else {
            format!("{last} (pass --pulse <path of pulse on {}>)", link.device)
        }
    })?;
    link.device_id = device_id;
    let mut links: Vec<Link> = store
        .links()
        .into_iter()
        .filter(|l| !l.device.eq_ignore_ascii_case(&link.device))
        .collect();
    links.push(link);
    links.sort_by(|a, b| a.device.to_lowercase().cmp(&b.device.to_lowercase()));
    store.save_links(&links).map_err(|e| e.to_string())?;
    Ok(listing)
}

pub fn remove(store: &Store, device: &str) -> Result<bool, String> {
    let before = store.links();
    let after: Vec<Link> = before
        .iter()
        .filter(|l| !l.device.eq_ignore_ascii_case(device.trim()))
        .cloned()
        .collect();
    let removed = after.len() != before.len();
    if removed {
        store.save_links(&after).map_err(|e| e.to_string())?;
    }
    Ok(removed)
}

fn last_json_line(out: &str) -> Option<&str> {
    out.lines().rev().find(|l| l.trim_start().starts_with('{'))
}

/// The chats open on a linked computer.
pub fn list_chats(link: &Link) -> Result<RemoteListing, String> {
    list_chats_identified(link).map(|(listing, _)| listing)
}

/// The chats open on a linked computer and the stable device id it reported
/// (`"deviceId"` in the listing), when it reports one.
pub fn list_chats_identified(link: &Link) -> Result<(RemoteListing, Option<String>), String> {
    let out = run(link, &["peers", "--local", "--json"], REMOTE_TIMEOUT)?;
    let line =
        last_json_line(&out).ok_or_else(|| format!("{} printed no chat list", link.device))?;
    let value: Value = serde_json::from_str(line)
        .map_err(|e| format!("{} printed something else: {e}", link.device))?;
    let device_id = value["deviceId"]
        .as_str()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    let listing = serde_json::from_value(value)
        .map_err(|e| format!("{} printed something else: {e}", link.device))?;
    Ok((listing, device_id))
}

const STATUS_FILE: &str = "links-status.json";

fn record_status(store: &Store, device: &str, error: Option<&str>) {
    let now = now_ms();
    let _ = store.update_state(STATUS_FILE, |all: &mut BTreeMap<String, LinkStatus>| {
        let entry = all.entry(device.to_lowercase()).or_default();
        entry.device = device.to_string();
        entry.last_attempt_ms = now;
        match error {
            None => {
                entry.last_ok_ms = Some(now);
                entry.last_error = None;
            }
            Some(text) => {
                entry.last_error = Some(text.to_string());
                entry.last_error_ms = Some(now);
            }
        }
    });
}

/// Poll one link: list its chats, pin or check its device id, record how it went.
/// Whether a remote answer means the command reached the wrong shell family: PowerShell
/// choking on POSIX quoting, or a POSIX shell / cmd on PowerShell syntax.
fn wrong_shell(error: &str) -> bool {
    error.contains("ParserError")
        || error.contains("is not recognized as")
        || error.contains("syntax error")
        || error.contains("command not found")
}

/// Poll `link`; when the answer says the stored shell family is wrong (a link made
/// before families were recorded, or a computer whose login shell changed), try the
/// other family once and keep it when it answers.
fn list_with_shell_fallback(
    store: &Store,
    link: &mut Link,
) -> Result<(RemoteListing, Option<String>), String> {
    match list_chats_identified(link) {
        Err(error) if wrong_shell(&error) && std::env::var_os("PULSE_BRIDGE_SSH").is_none() => {
            let other = if link.shell == "powershell" { "posix" } else { "powershell" };
            let mut retry = link.clone();
            retry.shell = other.to_string();
            match list_chats_identified(&retry) {
                Ok(answer) => {
                    let device = link.device.clone();
                    let shell = other.to_string();
                    let _ = store.update_state("links.json", move |links: &mut Vec<Link>| {
                        for l in links.iter_mut() {
                            if l.device.eq_ignore_ascii_case(&device) {
                                l.shell = shell.clone();
                            }
                        }
                    });
                    link.shell = other.to_string();
                    Ok(answer)
                }
                Err(_) => Err(error),
            }
        }
        other => other,
    }
}

fn poll(store: &Store, mut link: Link) -> RemoteChats {
    let pinned = link.device_id.clone();
    let polled = list_with_shell_fallback(store, &mut link);
    let listing = polled.and_then(|(listing, seen)| match (pinned, seen) {
        (Some(pinned), Some(seen)) if pinned != seen => Err(format!(
            "{} is not the computer this link was made with (device id changed); \
             link it again if it was reinstalled",
            link.device
        )),
        (None, Some(seen)) => Ok((listing, Some(seen))),
        _ => Ok((listing, None)),
    });
    let listing = listing.map(|(listing, learned)| {
        if let Some(id) = learned {
            let device = link.device.clone();
            let stored = id.clone();
            let _ = store.update_state("links.json", move |links: &mut Vec<Link>| {
                for l in links.iter_mut() {
                    if l.device.eq_ignore_ascii_case(&device) && l.device_id.is_none() {
                        l.device_id = Some(stored.clone());
                    }
                }
            });
            link.device_id = Some(id);
        }
        listing
    });
    record_status(
        store,
        &link.device,
        listing.as_ref().err().map(String::as_str),
    );
    RemoteChats { link, listing }
}

/// One linked computer's chats (`None` when no such link), asked now.
pub fn list_chats_for(store: &Store, device: &str) -> Option<RemoteChats> {
    find(store, device).map(|link| poll(store, link))
}

/// Every link's chats, asked in parallel.
pub fn list_all(store: &Store) -> Vec<RemoteChats> {
    let handles: Vec<_> = all(store)
        .into_iter()
        .map(|link| {
            let store = store.clone();
            std::thread::spawn(move || poll(&store, link))
        })
        .collect();
    handles.into_iter().filter_map(|h| h.join().ok()).collect()
}

/// How each link's last poll went: a link that is down shows as offline, with when
/// it last answered, instead of being missing. Links never polled report no success.
pub fn link_status(store: &Store) -> Vec<LinkStatus> {
    let recorded: BTreeMap<String, LinkStatus> = store.read_state(STATUS_FILE).unwrap_or_default();
    all(store)
        .into_iter()
        .map(|link| {
            recorded
                .get(&link.device.to_lowercase())
                .cloned()
                .unwrap_or_else(|| LinkStatus {
                    device: link.device.clone(),
                    ..LinkStatus::default()
                })
        })
        .collect()
}

/// Deliver `env` to the chat it names on the linked computer. The result is
/// the receipt `pulse bridge post` printed there (`{status, detail}`).
pub fn post(link: &Link, env: &Envelope) -> Result<Value, String> {
    let encoded = encode_envelope(env);
    let out = run(link, &["post", &encoded], REMOTE_TIMEOUT)?;
    let line = last_json_line(&out).ok_or_else(|| format!("{} printed no receipt", link.device))?;
    serde_json::from_str(line).map_err(|e| format!("{} printed something else: {e}", link.device))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for text in ["", "a", "ab", "abc", "hello world", "ünïcödé ✓"] {
            let encoded = b64(text.as_bytes());
            assert_eq!(unb64(&encoded).unwrap(), text.as_bytes(), "{text}");
        }
        assert_eq!(b64(b"abc"), "YWJj");
        assert_eq!(b64(b"ab"), "YWI=");
        assert!(unb64("!!").is_none());
    }
}
