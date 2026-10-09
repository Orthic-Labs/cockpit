//! One journey through the Pulse bridge with two devices: pairing over a real
//! TLS connection, signed messages accepted or refused by the receiving
//! service, inbox trimming, rosters from disk and from a paired device, and the
//! CLI's `peers` / `send` path (caller identification, local hold, remote
//! queue) through the library. Delivery into Claude and Codex themselves is not
//! exercised here.
#![cfg(feature = "localsend")]

use pulse_core::bridge::envelope::{
    self, Envelope, EnvelopeError, Kind, MAX_BODY_BYTES, ReplayGuard, Sender, Target,
};
use pulse_core::bridge::roster::{self, RosterEntry};
use pulse_core::bridge::store::{RegisteredSession, RemoteRoster, Store};
use pulse_core::bridge::{Caller, Identity, all_peers, identify, send_text};
use pulse_core::localsend::proto;
use pulse_core::localsend::{Config, Event, Service, net};
use serde_json::json;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PORT: u16 = 53947;

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("pulse-bridge-{name}-{}", proto::random_hex(4)))
}

fn message(from: &str, to: &str, body: &str) -> Envelope {
    Envelope::new(
        Sender {
            device: from.into(),
            session: "chat-a".into(),
            name: "Planner on Mac A".into(),
        },
        Target {
            device: to.into(),
            session: "chat-b".into(),
        },
        Kind::Message,
        body,
    )
    .expect("envelope")
}

/// POST one `prepare-upload` the way a paired Pulse device does.
fn post(file_type: &str, preview: &str, fingerprint: &str) -> net::Reply {
    let body = json!({
        "info": {"alias": "Mac A", "version": "2.0", "deviceType": "desktop",
                 "fingerprint": fingerprint, "port": 53317, "protocol": "https"},
        "files": {"b0": {"id": "b0", "fileName": "x", "size": preview.len(),
                         "fileType": file_type, "preview": preview}}
    })
    .to_string();
    let ip: IpAddr = "127.0.0.1".parse().unwrap();
    net::request_json(
        ip,
        PORT,
        true,
        "",
        "POST",
        "/api/localsend/v2/prepare-upload",
        Some(body.as_bytes()),
        Duration::from_secs(30),
    )
    .expect("prepare-upload answers")
}

#[test]
fn two_devices_pair_exchange_signed_messages_and_the_cli_sends() {
    let now = envelope::now_ms();

    // ---- signing -----------------------------------------------------------
    assert_eq!(
        envelope::to_hex(&envelope::hmac_sha256(
            b"Jefe",
            b"what do ya want for nothing?"
        )),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    let key = envelope::from_hex(&envelope::new_pair_key()).unwrap();
    assert_eq!(key.len(), 32);
    let mut env = message("dev-a", "dev-b", "hello from A");
    env.sign(&key);
    let received = Envelope::from_json(&env.to_json()).unwrap();
    let mut replay = ReplayGuard::default();
    assert_eq!(
        envelope::accept(&received, Some(key.as_slice()), now, &mut replay),
        Ok(())
    );
    assert_eq!(
        envelope::accept(&received, Some(key.as_slice()), now, &mut replay),
        Err(EnvelopeError::Replay)
    );
    let mut tampered = received.clone();
    tampered.body = "HELLO".into();
    assert_eq!(
        envelope::accept(
            &tampered,
            Some(key.as_slice()),
            now,
            &mut ReplayGuard::default()
        ),
        Err(EnvelopeError::BadSignature)
    );
    assert_eq!(
        envelope::accept(
            &received,
            Some(&[7u8; 32][..]),
            now,
            &mut ReplayGuard::default()
        ),
        Err(EnvelopeError::BadSignature)
    );
    assert_eq!(
        envelope::accept(&received, None, now, &mut ReplayGuard::default()),
        Err(EnvelopeError::UnknownDevice)
    );
    let mut old = message("dev-a", "dev-b", "late");
    old.ts = now - 6 * 60 * 1000;
    old.sign(&key);
    assert_eq!(
        envelope::accept(&old, Some(key.as_slice()), now, &mut ReplayGuard::default()),
        Err(EnvelopeError::Skew)
    );
    assert!(matches!(
        Envelope::new(
            Sender::default(),
            Target::default(),
            Kind::Message,
            "x".repeat(MAX_BODY_BYTES + 1)
        ),
        Err(EnvelopeError::Oversize)
    ));
    let mut window = ReplayGuard::new(3);
    for nonce in ["n1", "n2", "n3", "n4"] {
        assert!(window.insert("dev-a", nonce));
    }
    assert!(
        window.insert("dev-a", "n1"),
        "the oldest nonce was forgotten"
    );

    // ---- pairing and signed delivery through a real service ----------------
    let (save, state) = (temp("save"), temp("state"));
    let heard: Arc<Mutex<Vec<Envelope>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = heard.clone();
    let service = Arc::new(
        Service::start(
            Config {
                alias: "Mac B (Pulse)".into(),
                port: PORT,
                save_dir: save.clone(),
                accept_known: true,
                state_dir: state.clone(),
                device_model: "Mac".into(),
            },
            Arc::new(move |event: Event| {
                if let Event::Bridge(envelope) = event {
                    sink.lock().unwrap().push(envelope);
                }
            }),
        )
        .expect("service starts"),
    );
    let b_fingerprint = service.fingerprint();
    let secret = envelope::new_pair_key();
    let pair_key = envelope::from_hex(&secret).unwrap();
    let signed = |body: &str, to: &str| {
        let mut e = message("phone-1", to, body);
        e.sign(&pair_key);
        e.to_json()
    };

    // Not paired yet: refused, nothing heard.
    let first = signed("too early", &b_fingerprint);
    assert_eq!(
        post(envelope::BRIDGE_FILE_TYPE, &first, "phone-1").status,
        403
    );

    // Pairing always asks, even with accept-known on; declining stores nothing.
    let asker = {
        let service = service.clone();
        std::thread::spawn(move || {
            for answer in [false, true] {
                for _ in 0..400 {
                    let waiting = service.snapshot().incoming.first().map(|i| i.id.clone());
                    if let Some(id) = waiting {
                        assert!(service.respond(&id, answer));
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        })
    };
    let offer = json!({ "offer": secret }).to_string();
    assert_eq!(
        post(envelope::PAIR_FILE_TYPE, &offer, "phone-1").status,
        403
    );
    assert_eq!(
        post(envelope::BRIDGE_FILE_TYPE, &first, "phone-1").status,
        403
    );
    assert_eq!(
        post(envelope::PAIR_FILE_TYPE, &offer, "phone-1").status,
        204
    );
    asker.join().unwrap();
    let known = std::fs::read_to_string(state.join("known-devices.json")).unwrap();
    assert!(known.contains("phone-1") && known.contains(&secret));

    // Paired: a valid message is taken once, and never touches the save folder.
    let good = signed("build is green", &b_fingerprint);
    assert_eq!(
        post(envelope::BRIDGE_FILE_TYPE, &good, "phone-1").status,
        204
    );
    assert_eq!(
        post(envelope::BRIDGE_FILE_TYPE, &good, "phone-1").status,
        403,
        "replay"
    );
    let forged = good.replace("build is green", "rm -rf");
    assert_eq!(
        post(envelope::BRIDGE_FILE_TYPE, &forged, "phone-1").status,
        403
    );
    let wrong_target = signed("misrouted", "someone-else");
    assert_eq!(
        post(envelope::BRIDGE_FILE_TYPE, &wrong_target, "phone-1").status,
        403
    );
    let impostor = signed("hi", &b_fingerprint);
    assert_eq!(
        post(envelope::BRIDGE_FILE_TYPE, &impostor, "phone-2").status,
        403
    );
    {
        let seen = heard.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].body, "build is green");
    }
    assert!(!save.exists() || std::fs::read_dir(&save).unwrap().next().is_none());
    assert!(service.snapshot().transfers.is_empty());
    assert!(service.is_paired("phone-1"));
    drop(service);
    std::thread::sleep(Duration::from_millis(600));

    // The key survives a restart.
    let restarted = Service::start(
        Config {
            alias: "Mac B (Pulse)".into(),
            port: PORT,
            save_dir: save.clone(),
            accept_known: false,
            state_dir: state.clone(),
            device_model: "Mac".into(),
        },
        Arc::new(|_| {}),
    )
    .expect("restart");
    assert!(restarted.is_paired("phone-1"));
    drop(restarted);

    // ---- store -------------------------------------------------------------
    let dir = temp("store");
    let store = Store::new(&dir).with_inbox_cap(4096);
    for i in 0..40 {
        let e = message(
            "dev-a",
            "dev-b",
            &format!("message {i} {}", "x".repeat(100)),
        );
        store.append_inbox("chat-b", &e).unwrap();
    }
    let size = std::fs::metadata(dir.join("bridge/inbox/chat-b.jsonl"))
        .unwrap()
        .len();
    assert!(size <= 4096, "inbox stays under its cap, was {size}");
    let kept = store.read_inbox("chat-b");
    assert!(kept.len() < 40 && kept.last().unwrap().seq == 40 && kept[0].seq > 1);
    assert_eq!(store.take_unread("chat-b", None).unwrap().len(), kept.len());
    assert!(store.take_unread("chat-b", None).unwrap().is_empty());
    let store = Store::new(&dir);

    // ---- roster ------------------------------------------------------------
    let me_pid = std::process::id();
    let sessions_dir = dir.join("claude-sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let file = |pid: u32, id: &str, name: &str| {
        json!({"pid": pid, "sessionId": id, "name": name, "cwd": "/work/app",
               "status": "idle", "messagingSocketPath": "/tmp/s.sock", "peerProtocol": 1})
        .to_string()
    };
    std::fs::write(
        sessions_dir.join(format!("{me_pid}.json")),
        file(me_pid, "claude-1", "Fix build"),
    )
    .unwrap();
    std::fs::write(
        sessions_dir.join("999999.json"),
        file(999_999, "claude-dead", "Gone"),
    )
    .unwrap();
    std::fs::write(sessions_dir.join("junk.json"), "not json").unwrap();
    std::fs::write(sessions_dir.join(format!("{me_pid}.abc.key")), "{}").unwrap();
    let alive = move |pid: u32| pid == me_pid;
    let claude = roster::claude_sessions_in(&sessions_dir, &alive);
    assert_eq!(claude.len(), 1);
    assert_eq!(claude[0].id, "claude-1");
    assert_eq!(claude[0].messaging_socket.as_deref(), Some("/tmp/s.sock"));

    let register = |id: &str, name: &str, pid: u32| {
        store
            .register_session(&RegisteredSession {
                id: id.into(),
                kind: "test".into(),
                name: name.into(),
                cwd: "/work/api".into(),
                pid,
                updated: now,
            })
            .unwrap();
    };
    register("chat-b", "Builder", me_pid);
    register("chat-gone", "Closed", 999_999);
    let local = roster::local_sessions_in(&store, Some(sessions_dir.as_path()), &alive);
    let ids: Vec<&str> = local.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&"claude-1") && ids.contains(&"chat-b") && !ids.contains(&"chat-gone"));

    let remote = |received: u64| RemoteRoster {
        device: "dev-b".into(),
        alias: "Mac B".into(),
        received,
        entries: vec![RosterEntry {
            session: "codex-9".into(),
            name: "Fix build".into(),
            kind: "codex".into(),
            cwd: "/srv".into(),
            status: "idle".into(),
        }],
    };
    let peers = roster::merge(&local, "dev-a", "Mac A", &[remote(now)], now);
    assert!(
        peers
            .iter()
            .any(|p| p.display == "Fix build on Mac B" && !p.local)
    );
    assert!(
        roster::merge(
            &local,
            "dev-a",
            "Mac A",
            &[remote(now - 10 * 60 * 1000)],
            now
        )
        .iter()
        .all(|p| p.local),
        "a stale roster is dropped"
    );
    assert!(roster::resolve(&peers, "fix build").is_err(), "ambiguous");
    assert_eq!(
        roster::resolve(&peers, "Fix build on Mac B").unwrap().id,
        "dev-b:codex-9"
    );
    assert_eq!(roster::resolve(&peers, "builder").unwrap().id, "chat-b");

    // ---- the CLI's peers / send path -----------------------------------------
    store
        .save_remote_roster(&remote(envelope::now_ms()))
        .unwrap();
    let me = Identity {
        device: "dev-a".into(),
        alias: "Mac A".into(),
    };
    register("chat-a", "Planner", me_pid);
    let local = roster::local_sessions_in(&store, Some(sessions_dir.as_path()), &alive);
    let no_env = |_: &str| None::<String>;
    let planner = identify(&local, &no_env, Some("Planner")).unwrap();
    assert_eq!(planner.id, "chat-a");
    assert!(identify(&local, &no_env, Some("nobody")).is_err());
    assert_eq!(
        identify(&local, &no_env, None).unwrap(),
        Caller {
            id: "cli".into(),
            name: "Pulse CLI".into(),
            reply_socket: None
        }
    );
    // Inside a Claude chat its own socket names it and is the reply address.
    let in_claude =
        |name: &str| (name == "CLAUDE_CODE_MESSAGING_SOCKET").then(|| "/tmp/s.sock".to_string());
    let claude = identify(&local, &in_claude, None).unwrap();
    assert_eq!(claude.id, "claude-1");
    assert_eq!(claude.reply_socket.as_deref(), Some("/tmp/s.sock"));
    // Inside Codex the thread id names it; no socket.
    let in_codex = |name: &str| (name == "CODEX_THREAD_ID").then(|| "codex-7".to_string());
    let codex = identify(&local, &in_codex, None).unwrap();
    assert_eq!(codex.id, "codex-7");
    assert!(codex.reply_socket.is_none());

    let listed = all_peers(&store, &me);
    let ids: Vec<&str> = listed.iter().map(|p| p.id.as_str()).collect();
    assert!(ids.contains(&"chat-b") && ids.contains(&"dev-b:codex-9"));
    let remote_peer = listed.iter().find(|p| !p.local).unwrap();
    assert_eq!(remote_peer.display, "Fix build on Mac B");
    assert_eq!(remote_peer.kind, "codex");

    // A local chat that can't be pushed to keeps the message in its inbox.
    let wait = Duration::from_millis(200);
    let sent = send_text(&store, &me, &planner, "chat-b", "build is green", wait).unwrap();
    assert_eq!(sent.status, "held");
    let messages = store.take_unread("chat-b", None).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].env.body, "build is green");
    assert_eq!(messages[0].env.from.name, "Planner on Mac A");
    assert_eq!(messages[0].env.from.session, "chat-a");
    assert!(store.take_unread("chat-b", None).unwrap().is_empty());
    assert!(send_text(&store, &me, &planner, "Planner", "me", wait).is_err());

    // A chat on a paired device is queued for the relay, unsigned until sent.
    let queued = send_text(&store, &me, &planner, "Fix build on Mac B", "ping", wait).unwrap();
    assert_eq!(queued.status, "queued");
    let outbox = store.outbox_queued();
    assert_eq!(outbox.len(), 1);
    assert_eq!(outbox[0].target_device, "dev-b");
    assert_eq!(outbox[0].envelope.to.session, "codex-9");
    assert_eq!(outbox[0].envelope.from.session, "chat-a");
    assert!(!outbox[0].envelope.is_signed());
    let mut relayed = outbox[0].envelope.clone();
    relayed.sign(&key);
    assert_eq!(relayed.verify(&key, envelope::now_ms()), Ok(()));

    let bad = send_text(&store, &me, &planner, "nobody at all", "hi", wait).unwrap_err();
    assert!(bad.to_string().contains("nobody at all"));

    for path in [save, state, dir] {
        let _ = std::fs::remove_dir_all(path);
    }
}
