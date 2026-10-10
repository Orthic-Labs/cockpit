//! `pulse bridge …`: message AI chats through Pulse, here and on linked computers. The
//! only agent-facing surface.
//!
//! peers [--local] [--json]           every chat that can be messaged, here and on linked computers
//! send <chat> [<text>…] [--from CHAT] [--stdin] [--json] [--]   everything after `--` is text
//! reply <message-id> [<text>…] [--stdin] [--json] [--]           answer a received message
//! link <device> <ssh-host> [--pulse PATH] [--json]   link a computer reachable over ssh
//! unlink <device>
//! status [--json]                    hub, chats here, linked computers
//! install|uninstall [--claude] [--codex] [--dry-run]   the Pulse skill for Claude and Codex
//! inbox [--from CHAT|--session ID] [--all] [--ack] [--json]   messages kept for a chat
//! post <base64 envelope>             (run over ssh by a linked computer) deliver one message here
//!
//! `send` and `reply` exit 0 for delivered/queued/held/sent, 2 for refused/unsupported and 3
//! when the outcome is unknown; the receipt is printed in every case.

use pulse_core::bridge::links::{self, Link};
use pulse_core::bridge::roster;
use pulse_core::bridge::store::Store;
use pulse_core::bridge::{self, Caller, SendOutcome, control, identify, install, local_identity};
use serde_json::{Value, json};
use std::io::{Read, Write};

const COMMANDS: &str = "peers, send, reply, link, unlink, status, install, uninstall, inbox";

pub fn run(mut args: Vec<String>, machine: bool) -> Result<(), String> {
    if args.is_empty() {
        return Err(format!("bridge needs a command: {COMMANDS}"));
    }
    let command = args.remove(0);
    match command.as_str() {
        "peers" => {
            let local = args.iter().any(|a| a == "--local");
            peers(local, machine)
        }
        "send" => {
            let mut p = Parsed::new(args, &["--from"], &["--stdin", "--json"])?;
            let machine = machine || p.flag("--json");
            let from = p.value("--from");
            let to = p.positional.first().cloned().ok_or(SEND_USAGE)?;
            p.positional.remove(0);
            let text = p.text()?;
            send(&to, &text, from.as_deref(), machine)
        }
        "reply" => {
            let mut p = Parsed::new(args, &[], &["--stdin", "--json"])?;
            let machine = machine || p.flag("--json");
            let id = p.positional.first().cloned().ok_or(REPLY_USAGE)?;
            p.positional.remove(0);
            let text = p.text()?;
            reply(&id, &text, machine)
        }
        "link" => {
            let pulse = crate::take_option(&mut args, "--pulse")?;
            match args.as_slice() {
                [device, ssh] => link(device, ssh, pulse.as_deref(), machine),
                _ => Err("link needs <device> <ssh-host> [--pulse PATH]".into()),
            }
        }
        "unlink" => match args.as_slice() {
            [device] => unlink(device),
            _ => Err("unlink needs one <device>".into()),
        },
        "status" => status(machine),
        "inbox" => {
            let p = Parsed::new(
                args,
                &["--from", "--session"],
                &["--all", "--ack", "--json"],
            )?;
            let machine = machine || p.flag("--json");
            if !p.positional.is_empty() {
                return Err("inbox takes options only".into());
            }
            inbox(&p, machine)
        }
        "install" | "uninstall" => install::run(&command, args, machine),
        "post" => match args.as_slice() {
            [encoded] => post(encoded),
            _ => Err("post needs one base64 envelope".into()),
        },
        other => Err(format!("unknown bridge command: {other}; use {COMMANDS}")),
    }
}

const SEND_USAGE: &str = "send needs <chat> and text: pulse bridge send <chat> [<text>…] \
     [--from CHAT] [--stdin] [--json] [--]";
const REPLY_USAGE: &str = "reply needs <message-id> and text: pulse bridge reply <message-id> \
     [<text>…] [--stdin] [--json] [--]";

/// Arguments split at the first `--`: before it options and positionals, after it literal text.
struct Parsed {
    values: Vec<(String, String)>,
    flags: Vec<String>,
    positional: Vec<String>,
    literal: Vec<String>,
}

impl Parsed {
    fn new(args: Vec<String>, valued: &[&str], flags: &[&str]) -> Result<Parsed, String> {
        let mut out = Parsed {
            values: Vec::new(),
            flags: Vec::new(),
            positional: Vec::new(),
            literal: Vec::new(),
        };
        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            if arg == "--" {
                out.literal = iter.by_ref().collect();
            } else if valued.contains(&arg.as_str()) {
                let value = iter.next().ok_or(format!("{arg} requires a value"))?;
                if out.values.iter().any(|(k, _)| *k == arg) {
                    return Err(format!("duplicate {arg}"));
                }
                out.values.push((arg, value));
            } else if flags.contains(&arg.as_str()) {
                out.flags.push(arg);
            } else if arg.starts_with("--") {
                return Err(format!(
                    "unknown option {arg} (put text that starts with -- after a lone --)"
                ));
            } else {
                out.positional.push(arg);
            }
        }
        Ok(out)
    }

    fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|f| f == name)
    }

    fn value(&self, name: &str) -> Option<String> {
        self.values
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }

    /// The message body: stdin with `--stdin`, else the remaining words joined by single spaces
    /// (words before `--`, then everything after it verbatim).
    fn text(&self) -> Result<String, String> {
        let words: Vec<&str> = self
            .positional
            .iter()
            .chain(self.literal.iter())
            .map(String::as_str)
            .collect();
        let text = if self.flag("--stdin") {
            if !words.is_empty() {
                return Err("--stdin takes the whole text; give no text arguments".into());
            }
            let mut body = String::new();
            std::io::stdin()
                .read_to_string(&mut body)
                .map_err(|e| format!("couldn't read stdin: {e}"))?;
            if body.ends_with('\n') {
                body.pop();
                if body.ends_with('\r') {
                    body.pop();
                }
            }
            body
        } else {
            words.join(" ")
        };
        if text.trim().is_empty() {
            return Err("the message text is empty".into());
        }
        Ok(text)
    }
}

fn open() -> Result<(Store, bridge::Identity), String> {
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let me = local_identity(&store).map_err(|e| e.to_string())?;
    Ok((store, me))
}

/// The chat this command runs in (from `--from` or the environment).
fn caller(store: &Store, from: Option<&str>) -> Result<Caller, String> {
    let env = |name: &str| std::env::var(name).ok();
    identify(&roster::local_sessions(store), &env, from).map_err(|e| e.to_string())
}

/// A stable id for this install: `<bridge>/device-id`, created on first use.
fn local_device_id(store: &Store) -> String {
    let path = store.root().join("device-id");
    if let Ok(text) = std::fs::read_to_string(&path) {
        let id = text.trim();
        if id.len() == 36 {
            return id.to_string();
        }
    }
    let id = bridge::envelope::new_uuid();
    let _ = std::fs::create_dir_all(store.root());
    let _ = std::fs::write(&path, format!("{id}\n"));
    id
}

/// Each link's last poll result, read from `links::link_status`.
fn link_states(store: &Store) -> Vec<links::LinkStatus> {
    links::link_status(store)
}

fn peers(local: bool, machine: bool) -> Result<(), String> {
    let (store, me) = open()?;
    let list = if local {
        bridge::local_peers(&store, &me)
    } else {
        bridge::all_peers(&store, &me)
    };
    let sessions = roster::local_sessions(&store);
    let liveness = |id: &str| {
        sessions
            .iter()
            .find(|s| s.id == id)
            .map_or("unknown", |s| s.liveness.as_str())
            .to_string()
    };
    if machine {
        let rows: Vec<Value> = list
            .iter()
            .map(|p| {
                json!({"chat": p.display, "name": p.name, "device": p.device_alias,
                       "kind": p.kind, "status": p.status, "local": p.local, "id": p.id,
                       "session": p.session, "cwd": p.cwd, "updatedMs": p.updated_ms,
                       "liveness": if p.local { liveness(&p.session) } else { "unknown".into() }})
            })
            .collect();
        // `chats` is what a linked computer reads (links::RemoteListing).
        println!(
            "{}",
            json!({"device": me.alias, "deviceId": local_device_id(&store), "peers": rows,
                   "chats": list.iter().filter(|p| p.local).map(|p| json!({
                       "session": p.session, "name": p.name, "kind": p.kind,
                       "cwd": p.cwd, "status": p.status, "updated_ms": p.updated_ms,
                       "liveness": liveness(&p.session)})).collect::<Vec<_>>(),
                   "hubRunning": store.relay_alive()})
        );
        return Ok(());
    }
    if list.is_empty() {
        println!("No chats found.");
    }
    for peer in &list {
        let folder = std::path::Path::new(&peer.cwd)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let age = peer
            .updated_ms
            .map(|ms| format!("{} ago", age_text(ms)))
            .unwrap_or_default();
        let live = if peer.local {
            liveness(&peer.session)
        } else {
            "unknown".into()
        };
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            peer.display, peer.kind, peer.status, live, folder, age
        );
    }
    if !local {
        let now = bridge::envelope::now_ms();
        for state in link_states(&store) {
            if !state.online() && state.last_error.is_some() {
                let since = state
                    .age_ms(now)
                    .map_or("never reached".to_string(), |age| {
                        format!("offline {}", since_text(age))
                    });
                println!("{}\t{since}", state.device);
            }
        }
    }
    if cfg!(unix) && !store.relay_alive() {
        eprintln!("Pulse is not running: messages to Claude chats here may be held until it is.");
    }
    Ok(())
}

/// Print the receipt, then exit 0 (delivered/queued/held/sent), 2 (refused/unsupported) or 3.
fn finish(outcome: &SendOutcome, machine: bool) -> Result<(), String> {
    if machine {
        println!(
            "{}",
            json!({"msgId": outcome.msg_id, "state": outcome.status,
                   "detail": outcome.detail, "to": outcome.to.display})
        );
    } else {
        println!(
            "{}: {} ({}) [{}]",
            outcome.to.display, outcome.status, outcome.detail, outcome.msg_id
        );
    }
    let code = match outcome.status.as_str() {
        "delivered" | "queued" | "held" | "sent" => 0,
        "refused" | "unsupported" => 2,
        _ => 3,
    };
    if code != 0 {
        let _ = std::io::stdout().flush();
        std::process::exit(code);
    }
    Ok(())
}

fn send(to: &str, text: &str, from: Option<&str>, machine: bool) -> Result<(), String> {
    let (store, me) = open()?;
    let from = caller(&store, from)?;
    let outcome = bridge::send_text(&store, &me, &from, to, text).map_err(|e| e.to_string())?;
    finish(&outcome, machine)
}

/// Answer a received message: send to the chat it came from, as the chat that received it.
fn reply(id: &str, text: &str, machine: bool) -> Result<(), String> {
    let (store, me) = open()?;
    let route = store
        .reply_route(id.trim())
        .ok_or("no route for that message id (routes are kept 24 h)")?;
    let sessions = roster::local_sessions(&store);
    let from = match sessions.iter().find(|s| s.id == route.to_session) {
        Some(s) => Caller {
            id: s.id.clone(),
            name: s.name.clone(),
            reply_socket: if s.kind == "claude" {
                s.messaging_socket.clone()
            } else {
                None
            },
        },
        None => Caller {
            id: route.to_session.clone(),
            name: route.to_session.clone(),
            reply_socket: None,
        },
    };
    // A chat on this computer is addressed by its id, one elsewhere by `<device>:<session>`.
    let to = if route.from_device == me.alias {
        route.from_session.clone()
    } else {
        format!("{}:{}", route.from_device, route.from_session)
    };
    let outcome = bridge::send_text(&store, &me, &from, &to, text).map_err(|e| e.to_string())?;
    finish(&outcome, machine)
}

fn link(device: &str, ssh: &str, pulse: Option<&str>, machine: bool) -> Result<(), String> {
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let link = Link {
        device: device.trim().to_string(),
        ssh: ssh.trim().to_string(),
        pulse: pulse
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .unwrap_or("pulse")
            .to_string(),
        ..Link::default()
    };
    let listing = links::add(&store, link.clone())?;
    if machine {
        println!(
            "{}",
            json!({"device": link.device, "ssh": link.ssh, "pulse": link.pulse,
                   "chats": listing.chats.len(), "theirName": listing.device})
        );
    } else {
        println!(
            "Linked {} ({}): {} chats there now.",
            link.device,
            link.ssh,
            listing.chats.len()
        );
    }
    Ok(())
}

fn unlink(device: &str) -> Result<(), String> {
    let store = Store::open_default().map_err(|e| e.to_string())?;
    if links::remove(&store, device)? {
        println!("Unlinked {device}.");
        Ok(())
    } else {
        Err(format!("no link named {device}"))
    }
}

fn status(machine: bool) -> Result<(), String> {
    let (store, me) = open()?;
    let here = roster::local_sessions(&store).len();
    let hub = control::hub_running(&store);
    let remotes = links::list_all(&store);
    if machine {
        let rows: Vec<Value> = remotes
            .iter()
            .map(|r| match &r.listing {
                Ok(l) => json!({"device": r.link.device, "ssh": r.link.ssh,
                                "chats": l.chats.len(), "error": Value::Null}),
                Err(e) => json!({"device": r.link.device, "ssh": r.link.ssh,
                                 "chats": Value::Null, "error": e}),
            })
            .collect();
        println!(
            "{}",
            json!({"hubRunning": hub, "device": me.alias, "chatsHere": here, "links": rows,
                   "bridgeEnabled": store.bridge_enabled()})
        );
        return Ok(());
    }
    println!(
        "Pulse: {}",
        if hub {
            "running"
        } else {
            "not running (open Pulse)"
        }
    );
    if !store.bridge_enabled() {
        println!("Bridge off: sends are refused (turn it on in Pulse).");
    }
    println!("{here} chats on {}", me.alias);
    if remotes.is_empty() {
        println!("No linked computers (link one with `pulse bridge link <device> <ssh-host>`).");
    }
    for r in &remotes {
        match &r.listing {
            Ok(l) => println!(
                "{} chats on {} ({})",
                l.chats.len(),
                r.link.device,
                r.link.ssh
            ),
            Err(e) => println!("{} ({}): {e}", r.link.device, r.link.ssh),
        }
    }
    let now = bridge::envelope::now_ms();
    for state in link_states(&store) {
        if let Some(error) = &state.last_error {
            let seen = state
                .age_ms(now)
                .map_or("never reached".to_string(), |age| {
                    format!("last reached {} ago", since_text(age))
                });
            println!("{}: {seen}; last error: {error}", state.device);
        }
    }
    Ok(())
}

fn inbox(p: &Parsed, machine: bool) -> Result<(), String> {
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let session = match p.value("--session") {
        Some(id) => id,
        None => caller(&store, p.value("--from").as_deref())?.id,
    };
    let entries = if p.flag("--all") {
        store.read_inbox(&session)
    } else {
        store
            .take_unread_ack(&session, None, false)
            .map_err(|e| e.to_string())?
    };
    let newest = entries.iter().map(|e| e.seq).max();
    let acked = p.flag("--ack");
    if let (true, Some(seq)) = (acked, newest) {
        store.ack_inbox(&session, seq).map_err(|e| e.to_string())?;
    }
    let evicted = store.inbox_evicted(&session);
    if machine {
        let rows: Vec<Value> = entries
            .iter()
            .map(|e| {
                json!({"id": e.env.id, "seq": e.seq, "from": e.env.from.name,
                       "body": e.env.body})
            })
            .collect();
        println!(
            "{}",
            json!({"messages": rows, "evicted": evicted, "acknowledged": acked && newest.is_some()})
        );
        return Ok(());
    }
    if entries.is_empty() {
        println!("No unread messages.");
    }
    for entry in &entries {
        println!("{}\n{}\n", entry.env.from.name, entry.env.body);
    }
    if evicted > 0 {
        println!("{evicted} older messages were dropped to keep the inbox small.");
    }
    if !entries.is_empty() && !acked {
        println!("Shown again next time; add --ack to mark these read.");
    }
    Ok(())
}

/// A linked computer's `send`, arriving over ssh: deliver here and print the
/// receipt as one JSON line.
fn post(encoded: &str) -> Result<(), String> {
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let env = links::decode_envelope(encoded)?;
    let receipt = bridge::receive(&store, &env);
    println!(
        "{}",
        json!({"msg_id": receipt.msg_id, "status": receipt.state.as_str(),
               "detail": receipt.detail})
    );
    Ok(())
}

/// "42s", "5min", "3h" or "2d" since `ms` (milliseconds since the epoch).
fn age_text(ms: u64) -> String {
    since_text(bridge::envelope::now_ms().saturating_sub(ms))
}

/// `age_text` for an age already in milliseconds.
fn since_text(age_ms: u64) -> String {
    let secs = age_ms / 1000;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}min", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}
