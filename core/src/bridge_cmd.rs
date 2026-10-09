//! `pulse bridge …`: let AI chats message each other through Pulse.
//!
//! mcp                         stdio MCP server a chat runs (registered by `bridge install`)
//! peers [--json]              chats that can be messaged, here and on paired computers
//! send <to> <text…> [--session ID]
//! inbox [--json] [--session ID]
//! daemon [--pair <device>]    sharing service plus the bridge relay, without the hub
//! install | uninstall         register the MCP server with Claude and Codex

use pulse_core::bridge::store::Store;
use pulse_core::bridge::{self, deliver_claude, install, local_identity, mcp};
use pulse_core::localsend::{Config, Event, Service, proto};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The chat id `send` and `inbox` use when `--session` is not given.
const CLI_SESSION: &str = "cli";

pub fn run(mut args: Vec<String>, machine: bool) -> Result<(), String> {
    if args.is_empty() {
        return Err(
            "bridge needs a command: mcp, peers, send, inbox, daemon, install, uninstall".into(),
        );
    }
    let command = args.remove(0);
    match command.as_str() {
        "mcp" => {
            let session = crate::take_option(&mut args, "--session")?;
            let kind = crate::take_option(&mut args, "--kind")?;
            let name = crate::take_option(&mut args, "--name")?;
            mcp::run(session, kind, name).map_err(|e| e.to_string())
        }
        "peers" => peers(machine),
        "send" => {
            let session = crate::take_option(&mut args, "--session")?;
            if args.len() < 2 {
                return Err("send needs <to> and <text>".into());
            }
            let to = args.remove(0);
            send(&to, &args.join(" "), session.as_deref(), machine)
        }
        "inbox" => {
            let session = crate::take_option(&mut args, "--session")?;
            inbox(session.as_deref().unwrap_or(CLI_SESSION), machine)
        }
        "daemon" => {
            let pair = crate::take_option(&mut args, "--pair")?;
            daemon(pair)
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

fn peers(machine: bool) -> Result<(), String> {
    let (store, me) = open()?;
    let list = bridge::all_peers(&store, &me);
    if machine {
        println!(
            "{}",
            json!({"peers": list, "relayRunning": store.relay_alive()})
        );
        return Ok(());
    }
    if list.is_empty() {
        println!("No chats found.");
    }
    for peer in &list {
        println!(
            "{}\t{}\t{}\t{}",
            peer.id,
            peer.display,
            peer.status,
            if peer.local {
                "this computer"
            } else {
                "paired"
            }
        );
    }
    if !store.relay_alive() {
        eprintln!("The Pulse relay is not running: chats on other computers are not reachable.");
    }
    Ok(())
}

fn send(to: &str, text: &str, session: Option<&str>, machine: bool) -> Result<(), String> {
    let (store, me) = open()?;
    let outcome = bridge::send_text(
        &store,
        &me,
        session.unwrap_or(CLI_SESSION),
        "Pulse CLI",
        to,
        text,
        Duration::from_secs(10),
    )
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

fn inbox(session: &str, machine: bool) -> Result<(), String> {
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let entries = store
        .take_unread(session, None)
        .map_err(|e| e.to_string())?;
    if machine {
        let rows: Vec<Value> = entries
            .iter()
            .map(|e| {
                json!({"seq": e.seq, "id": e.env.id, "received": e.received,
                       "from": e.env.from.name, "fromSession": e.env.from.session,
                       "body": e.env.body})
            })
            .collect();
        println!("{}", json!({"session": session, "messages": rows}));
        return Ok(());
    }
    if entries.is_empty() {
        println!("No unread messages.");
    }
    for entry in &entries {
        println!(
            "[{}] {}\n{}\n",
            entry.seq, entry.env.from.name, entry.env.body
        );
    }
    Ok(())
}

fn home_downloads() -> std::path::PathBuf {
    #[cfg(windows)]
    let variable = "USERPROFILE";
    #[cfg(not(windows))]
    let variable = "HOME";
    std::env::var_os(variable)
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join("Downloads")
}

/// The sharing service and the bridge relay in this process. The hub does the
/// same inside itself; run only one of them.
fn daemon(pair: Option<String>) -> Result<(), String> {
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let host = sysinfo::System::host_name().unwrap_or_else(|| "Computer".to_string());
    let config = Config {
        alias: format!("{host} (Pulse)"),
        port: proto::PORT,
        save_dir: home_downloads(),
        accept_known: false,
        state_dir: store.localsend_dir(),
        device_model: if cfg!(windows) { "Windows" } else { "Mac" }.to_string(),
    };
    let slot: Arc<Mutex<Option<Arc<Service>>>> = Arc::new(Mutex::new(None));
    let for_events = slot.clone();
    let service = Arc::new(
        Service::start(
            config,
            Arc::new(move |event: Event| {
                if let Event::Bridge(envelope) = event {
                    let current = for_events.lock().ok().and_then(|s| s.clone());
                    if let Some(service) = current {
                        std::thread::spawn(move || bridge::on_inbound(&service, envelope));
                    }
                }
            }),
        )
        .map_err(|e| {
            format!("{e} (If the Pulse hub is running, the bridge already runs inside it.)")
        })?,
    );
    if let Ok(mut current) = slot.lock() {
        *current = Some(service.clone());
    }
    let for_replies = service.clone();
    let on_reply = Arc::new(move |reply: deliver_claude::ReplyMessage| {
        let service = for_replies.clone();
        std::thread::spawn(move || bridge::on_local_reply(&service, reply));
    });
    let is_known = Arc::new(bridge::is_known_local_session);
    deliver_claude::set_reply_hub(Some(deliver_claude::ReplyHub::new(on_reply, is_known)));
    eprintln!("Pulse bridge relay running. Press Ctrl-C to stop.");
    if let Some(target) = pair {
        // Give discovery a moment, then ask; the other computer shows a prompt.
        std::thread::sleep(Duration::from_secs(4));
        eprintln!("Asking {target} to pair; accept on that computer…");
        match service.bridge_pair(&target) {
            Ok(()) => eprintln!("Paired with {target}."),
            Err(e) => eprintln!("Pairing failed: {e}"),
        }
    }
    loop {
        bridge::tick(&service);
        std::thread::sleep(Duration::from_secs(2));
    }
}
