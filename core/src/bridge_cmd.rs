//! `pulse bridge …`: message AI chats on other computers through Pulse. The
//! only agent-facing surface; same-computer chats already talk natively and
//! `send` reaches them natively too.
//!
//! peers [--local] [--json]           every chat that can be messaged, here and on linked computers
//! send <chat on device> <text…> [--from CHAT] [--json]
//! link <device> <ssh-host> [--pulse PATH] [--json]   link a computer reachable over ssh
//! unlink <device>
//! status [--json]                    hub, chats here, linked computers
//! install|uninstall [--claude] [--codex] [--dry-run]   the Pulse skill for Claude and Codex
//! inbox [--from CHAT] [--json]       messages that could not be delivered natively
//! post <base64 envelope>             (run over ssh by a linked computer) deliver one message here

use pulse_core::bridge::links::{self, Link};
use pulse_core::bridge::roster;
use pulse_core::bridge::store::Store;
use pulse_core::bridge::{self, Caller, control, identify, install, local_identity};
use serde_json::{Value, json};

pub fn run(mut args: Vec<String>, machine: bool) -> Result<(), String> {
    if args.is_empty() {
        return Err(
            "bridge needs a command: peers, send, link, unlink, status, install, uninstall, inbox"
                .into(),
        );
    }
    let command = args.remove(0);
    match command.as_str() {
        "peers" => {
            let local = args.iter().any(|a| a == "--local");
            peers(local, machine)
        }
        "send" => {
            let from = crate::take_option(&mut args, "--from")?;
            if args.len() < 2 {
                return Err("send needs \"<chat> on <device>\" and \"<text>\"".into());
            }
            let to = args.remove(0);
            send(&to, &args.join(" "), from.as_deref(), machine)
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
            let from = crate::take_option(&mut args, "--from")?;
            inbox(from.as_deref(), machine)
        }
        "install" | "uninstall" => install::run(&command, args, machine),
        "post" => match args.as_slice() {
            [encoded] => post(encoded),
            _ => Err("post needs one base64 envelope".into()),
        },
        other => Err(format!("unknown bridge command: {other}")),
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

fn peers(local: bool, machine: bool) -> Result<(), String> {
    let (store, me) = open()?;
    let list = if local {
        bridge::local_peers(&store, &me)
    } else {
        bridge::all_peers(&store, &me)
    };
    if machine {
        let rows: Vec<Value> = list
            .iter()
            .map(|p| {
                json!({"chat": p.display, "name": p.name, "device": p.device_alias,
                       "kind": p.kind, "status": p.status, "local": p.local, "id": p.id,
                       "session": p.session, "cwd": p.cwd})
            })
            .collect();
        // `chats` is what a linked computer reads (links::RemoteListing).
        println!(
            "{}",
            json!({"device": me.alias, "peers": rows,
                   "chats": list.iter().filter(|p| p.local).map(|p| json!({
                       "session": p.session, "name": p.name, "kind": p.kind,
                       "cwd": p.cwd, "status": p.status})).collect::<Vec<_>>(),
                   "hubRunning": store.relay_alive()})
        );
        return Ok(());
    }
    if list.is_empty() {
        println!("No chats found.");
    }
    for peer in &list {
        println!("{}\t{}\t{}", peer.display, peer.kind, peer.status);
    }
    if cfg!(unix) && !store.relay_alive() {
        eprintln!("Pulse is not running: messages into Claude chats here are kept until it is.");
    }
    Ok(())
}

fn send(to: &str, text: &str, from: Option<&str>, machine: bool) -> Result<(), String> {
    let (store, me) = open()?;
    let from = caller(&store, from)?;
    let outcome = bridge::send_text(&store, &me, &from, to, text).map_err(|e| e.to_string())?;
    if machine {
        println!(
            "{}",
            json!({"msg_id": outcome.msg_id, "status": outcome.status,
                   "detail": outcome.detail, "to": outcome.to.display})
        );
    } else {
        println!(
            "{}: {} ({})",
            outcome.to.display, outcome.status, outcome.detail
        );
    }
    Ok(())
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
            json!({"hubRunning": hub, "device": me.alias, "chatsHere": here, "links": rows})
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
    Ok(())
}

fn inbox(from: Option<&str>, machine: bool) -> Result<(), String> {
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let session = caller(&store, from)?.id;
    let entries = store
        .take_unread(&session, None)
        .map_err(|e| e.to_string())?;
    if machine {
        let rows: Vec<Value> = entries
            .iter()
            .map(|e| json!({"id": e.env.id, "from": e.env.from.name, "body": e.env.body}))
            .collect();
        println!("{}", json!({"messages": rows}));
        return Ok(());
    }
    if entries.is_empty() {
        println!("No unread messages.");
    }
    for entry in &entries {
        println!("{}\n{}\n", entry.env.from.name, entry.env.body);
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
        json!({"msg_id": receipt.msg_id, "status": receipt.state.as_str(), "detail": receipt.detail})
    );
    Ok(())
}
