//! Linked computers: how this computer reaches another one's `pulse` over ssh.
//!
//! A link is `{device, ssh, pulse}`: the name chats are shown under
//! ("<chat> on <device>"), the ssh destination (a host alias from
//! `~/.ssh/config`, or `user@host`), and the path of `pulse` on that computer.
//! Links live in `<state>/bridge/links.json` and are made with
//! `pulse bridge link <device> <ssh-host> [--pulse PATH]`.
//!
//! Two remote commands exist: `pulse bridge peers --local --json` (that
//! computer's chats) and `pulse bridge post <base64 envelope>` (deliver one
//! message there). The envelope is base64 so no quoting survives two shells.
//! ssh must already work without a prompt (keys, BatchMode); nothing here
//! sets ssh up. `PULSE_BRIDGE_SSH` names another program to run instead of
//! `ssh` (tests).

use super::envelope::Envelope;
use super::roster::RosterEntry;
use super::store::Store;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long one remote command may take, ssh connection included.
pub const REMOTE_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub device: String,
    pub ssh: String,
    /// The `pulse` binary there; "pulse" means on PATH.
    #[serde(default = "default_pulse")]
    pub pulse: String,
}

fn default_pulse() -> String {
    "pulse".to_string()
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

fn ssh_program() -> String {
    std::env::var("PULSE_BRIDGE_SSH")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "ssh".to_string())
}

/// Run `<pulse> bridge <args…>` on the linked computer; stdout on exit 0.
fn run(link: &Link, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut remote = vec![link.pulse.clone(), "bridge".to_string()];
    remote.extend(args.iter().map(|a| a.to_string()));
    let mut command = Command::new(ssh_program());
    if ssh_program() == "ssh" {
        command.args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "LogLevel=ERROR",
        ]);
    }
    command.arg(&link.ssh);
    command.args(&remote);
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
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let mut err = String::new();
        if let Some(s) = stdout.as_mut() {
            let _ = s.read_to_string(&mut out);
        }
        if let Some(s) = stderr.as_mut() {
            let _ = s.read_to_string(&mut err);
        }
        let _ = tx.send((out, err));
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
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
    let (out, err) = rx.recv().unwrap_or_default();
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

/// Where `pulse` lives when the link does not say: PATH, then the installed app on a Mac,
/// then the installed app on Windows (PowerShell expands `$env:`; ssh to Windows lands in
/// PowerShell by default).
const PULSE_CANDIDATES: [&str; 3] = [
    "pulse",
    "/Applications/Pulse.app/Contents/Helpers/pulse",
    "$env:LOCALAPPDATA\\Programs\\Pulse\\Helpers\\pulse.exe",
];

/// Add or replace a link, then check it by listing that computer's chats. With no
/// explicit `pulse` path, the first candidate that answers is stored.
pub fn add(store: &Store, mut link: Link) -> Result<RemoteListing, String> {
    if link.device.trim().is_empty() || link.ssh.trim().is_empty() {
        return Err("a link needs a device name and an ssh host".to_string());
    }
    if link.device.contains(':') {
        return Err("the device name can't contain ':'".to_string());
    }
    let listing = if link.pulse == "pulse" {
        let mut found = None;
        let mut last = String::new();
        for candidate in PULSE_CANDIDATES {
            link.pulse = candidate.to_string();
            match list_chats(&link) {
                Ok(listing) => {
                    found = Some(listing);
                    break;
                }
                Err(e) => last = e,
            }
        }
        found.ok_or_else(|| format!("{last} (pass --pulse <path of pulse on {}>)", link.device))?
    } else {
        list_chats(&link)?
    };
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

/// The chats open on a linked computer.
pub fn list_chats(link: &Link) -> Result<RemoteListing, String> {
    let out = run(link, &["peers", "--local", "--json"], REMOTE_TIMEOUT)?;
    let line = out
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .ok_or_else(|| format!("{} printed no chat list", link.device))?;
    serde_json::from_str(line).map_err(|e| format!("{} printed something else: {e}", link.device))
}

/// Every link's chats, asked in parallel.
pub fn list_all(store: &Store) -> Vec<RemoteChats> {
    let handles: Vec<_> = all(store)
        .into_iter()
        .map(|link| {
            std::thread::spawn(move || RemoteChats {
                listing: list_chats(&link),
                link,
            })
        })
        .collect();
    handles
        .into_iter()
        .filter_map(|h| h.join().ok())
        .collect()
}

/// Deliver `env` to the chat it names on the linked computer. The result is
/// the receipt `pulse bridge post` printed there (`{status, detail}`).
pub fn post(link: &Link, env: &Envelope) -> Result<Value, String> {
    let encoded = encode_envelope(env);
    let out = run(link, &["post", &encoded], REMOTE_TIMEOUT)?;
    let line = out
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .ok_or_else(|| format!("{} printed no receipt", link.device))?;
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
