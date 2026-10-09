//! `pulse bridge …`: message AI chats on other computers through Pulse. The
//! only agent-facing surface; same-computer chats already talk natively and
//! `send` reaches them natively too.
//!
//! peers [--json]                      every chat that can be messaged, here and on paired computers
//! send <chat on device> <text…> [--from CHAT] [--json]
//! pair <device>                       pair with a nearby computer (a prompt appears there); the running Pulse hub does it
//! status [--json]                     relay, chats here, chats on paired computers
//! install|uninstall [--claude] [--codex] [--dry-run]   the Pulse skill for Claude and Codex
//! inbox [--from CHAT] [--json]        messages that could not be delivered natively

use pulse_core::bridge::roster::{self, REMOTE_MAX_AGE_MS};
use pulse_core::bridge::store::Store;
use pulse_core::bridge::{self, Caller, control, identify, install, local_identity};
use serde_json::{Value, json};
use std::time::Duration;

pub fn run(mut args: Vec<String>, machine: bool) -> Result<(), String> {
    if args.is_empty() {
        return Err(
            "bridge needs a command: peers, send, pair, status, install, uninstall, inbox".into(),
        );
    }
    let command = args.remove(0);
    match command.as_str() {
        "peers" => peers(machine),
        "send" => {
            let from = crate::take_option(&mut args, "--from")?;
            if args.len() < 2 {
                return Err("send needs \"<chat> on <device>\" and \"<text>\"".into());
            }
            let to = args.remove(0);
            send(&to, &args.join(" "), from.as_deref(), machine)
        }
        "pair" => match args.as_slice() {
            [device] => pair(device),
            _ => Err("pair needs one <device> (a nearby computer's name)".into()),
        },
        "status" => status(machine),
        "inbox" => {
            let from = crate::take_option(&mut args, "--from")?;
            inbox(from.as_deref(), machine)
        }
        "install" | "uninstall" => install::run(&command, args, machine),
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

fn peers(machine: bool) -> Result<(), String> {
    let (store, me) = open()?;
    let list = bridge::all_peers(&store, &me);
    if machine {
        let rows: Vec<Value> = list
            .iter()
            .map(|p| {
                json!({"chat": p.display, "name": p.name, "device": p.device_alias,
                       "kind": p.kind, "status": p.status, "local": p.local, "id": p.id})
            })
            .collect();
        println!(
            "{}",
            json!({"peers": rows, "relayRunning": store.relay_alive()})
        );
        return Ok(());
    }
    if list.is_empty() {
        println!("No chats found.");
    }
    for peer in &list {
        println!("{}\t{}\t{}", peer.display, peer.kind, peer.status);
    }
    if !store.relay_alive() {
        eprintln!("The Pulse relay is not running: chats on other computers are not reachable.");
    }
    Ok(())
}

fn send(to: &str, text: &str, from: Option<&str>, machine: bool) -> Result<(), String> {
    let (store, me) = open()?;
    let from = caller(&store, from)?;
    let outcome = bridge::send_text(&store, &me, &from, to, text, Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
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

fn status(machine: bool) -> Result<(), String> {
    let (store, me) = open()?;
    let here = roster::local_sessions(&store).len();
    let now = bridge::envelope::now_ms();
    let remotes: Vec<_> = store
        .remote_rosters()
        .into_iter()
        .filter(|r| now.saturating_sub(r.received) <= REMOTE_MAX_AGE_MS)
        .collect();
    let relay = store.relay_alive();
    if machine {
        let rows: Vec<Value> = remotes
            .iter()
            .map(|r| {
                json!({"device": r.alias, "chats": r.entries.len(),
                       "ageSeconds": now.saturating_sub(r.received) / 1000})
            })
            .collect();
        println!(
            "{}",
            json!({"relayRunning": relay, "device": me.alias, "chatsHere": here, "paired": rows})
        );
        return Ok(());
    }
    println!(
        "Relay: {}",
        if relay {
            "running"
        } else {
            "not running (open Pulse)"
        }
    );
    println!("{here} chats on {}", me.alias);
    if remotes.is_empty() {
        println!(
            "No paired computer has shared its chats lately (pair one with `pulse bridge pair <device>`)."
        );
    }
    for r in &remotes {
        println!("{} chats on {}", r.entries.len(), r.alias);
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

/// Ask a nearby computer to pair; accept on that computer. The running Pulse
/// hub (it owns nearby sharing and its port) does the pairing and starts using
/// the new key at once.
fn pair(device: &str) -> Result<(), String> {
    let store = Store::open_default().map_err(|e| e.to_string())?;
    if !control::hub_running(&store) {
        return Err("Start Pulse (the hub runs nearby sharing) and try again.".to_string());
    }
    eprintln!("Asking {device} to pair; accept on that computer...");
    let reply = control::call(
        &store,
        "pair",
        json!({"device": device}),
        Duration::from_secs(210),
    )?;
    if reply["ok"].as_bool() == Some(true) {
        println!("Paired with {device}.");
        Ok(())
    } else {
        Err(reply["error"]
            .as_str()
            .unwrap_or("Pairing failed.")
            .to_string())
    }
}
