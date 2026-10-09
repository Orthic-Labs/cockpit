//! One journey through the Pulse bridge with two computers over ssh. Computer
//! B (its own HOME) has a Claude chat with a fake messaging socket; a shell
//! script stands in for ssh and runs the real `pulse` binary as B. Computer A
//! links B, lists its chats, sends a message that B's `pulse bridge post`
//! delivers into that socket, then sees a refusal once the chat is gone, and
//! unlinks. Delivery into a real Claude or Codex chat is not exercised here.
#![cfg(feature = "localsend")]

use pulse_core::bridge::links::{self, Link, RemoteChats, RemoteListing};
use pulse_core::bridge::roster::{self, RosterEntry};
use pulse_core::bridge::{BridgeError, Envelope, Sender, Target};

fn entry(session: &str, name: &str) -> RosterEntry {
    RosterEntry {
        session: session.into(),
        name: name.into(),
        kind: "claude".into(),
        cwd: String::new(),
        status: "idle".into(),
    }
}

fn remote(device: &str, chats: Vec<RosterEntry>) -> RemoteChats {
    RemoteChats {
        link: Link {
            device: device.into(),
            ssh: "fake".into(),
            pulse: "pulse".into(),
        },
        listing: Ok(RemoteListing {
            device: device.into(),
            chats,
        }),
    }
}

/// Rosters merge linked chats under "<chat> on <device>"; a shared title is ambiguous.
fn roster_and_wire_format() {
    let remotes = [
        remote("B", vec![entry("chat-b", "Planner")]),
        remote("C", vec![entry("chat-c", "Planner")]),
    ];
    let peers = roster::merge(&[], "A", &remotes);
    assert_eq!(peers.len(), 2);
    assert_eq!(peers[0].display, "Planner on B");
    assert_eq!(peers[0].id, "B:chat-b");
    assert!(!peers[0].local);
    let hit = roster::resolve(&peers, "Planner on C").unwrap();
    assert_eq!(hit.session, "chat-c");
    match roster::resolve(&peers, "Planner") {
        Err(BridgeError::Invalid(text)) => assert!(text.contains("more than one"), "{text}"),
        other => panic!("expected an ambiguity error, got {other:?}"),
    }
    assert!(matches!(
        roster::resolve(&peers, "nobody"),
        Err(BridgeError::NotFound(_))
    ));

    let env = Envelope::new(
        Sender {
            device: "A".into(),
            session: "chat-a".into(),
            name: "Driver on A".into(),
        },
        Target {
            device: "B".into(),
            session: "chat-b".into(),
        },
        "ünïcödé ✓ \"quoted\"\nsecond line",
    )
    .unwrap();
    let back = links::decode_envelope(&links::encode_envelope(&env)).unwrap();
    assert_eq!(back, env);
    assert!(links::decode_envelope("!!not base64!!").is_err());
}

#[cfg(unix)]
fn journey() {
    use pulse_core::bridge::store::Store;
    use pulse_core::bridge::{Caller, all_peers, identify, local_identity, send_text};
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    let temp = |name: &str| -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pulse-bridge-{name}-{}-{}",
            std::process::id(),
            pulse_core::localsend::proto::random_hex(4)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    };
    let a = temp("a");
    let b = temp("b");

    // Computer B: one Claude chat with a fake messaging socket.
    let sessions = b.join(".claude/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let socket = b.join("cc.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let pid = std::process::id();
    let session_file = sessions.join(format!("{pid}.json"));
    std::fs::write(
        &session_file,
        serde_json::json!({
            "pid": pid, "sessionId": "chat-b", "name": "Planner",
            "cwd": b.to_string_lossy(), "status": "idle", "peerProtocol": 1,
            "messagingSocketPath": socket.to_string_lossy(), "entrypoint": "cli",
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        sessions.join(format!("{pid}.{}.key", "ab".repeat(32))),
        r#"{"peerToken": "x"}"#,
    )
    .unwrap();
    let (line_tx, line_rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            let mut line = String::new();
            if BufReader::new(stream).read_line(&mut line).is_ok() && !line.is_empty() {
                let _ = line_tx.send(line);
            }
        }
    });

    // The fake ssh: drops the host argument and runs B's pulse as B.
    let pulse = env!("CARGO_BIN_EXE_pulse");
    let script = a.join("fake-ssh.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nshift\nexec env HOME='{b}' CLAUDE_CONFIG_DIR='{b}/.claude' \
             CLAUDE_CODE_MESSAGING_SOCKET='{sock}' \"$@\"\n",
            b = b.display(),
            sock = socket.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The only test in this binary, so no other thread reads the environment yet.
    unsafe { std::env::set_var("PULSE_BRIDGE_SSH", &script) };

    let store = Store::new(&a);
    let me = local_identity(&store).unwrap();

    // a) linking lists B's chats.
    let listing = links::add(
        &store,
        Link {
            device: "B".into(),
            ssh: "fake".into(),
            pulse: pulse.into(),
        },
    )
    .expect("link B");
    assert!(
        listing
            .chats
            .iter()
            .any(|c| c.name == "Planner" && c.session == "chat-b"),
        "{listing:?}"
    );
    assert_eq!(links::all(&store).len(), 1);

    // b) the roster shows it as a remote peer.
    let peers = all_peers(&store, &me);
    let planner = peers
        .iter()
        .find(|p| p.id == "B:chat-b")
        .unwrap_or_else(|| panic!("no remote Planner in {peers:?}"));
    assert_eq!(planner.display, "Planner on B");
    assert!(!planner.local);

    // The calling chat is found from the environment, as `pulse bridge send` does.
    let env = |name: &str| match name {
        "CLAUDE_CODE_MESSAGING_SOCKET" => Some("/tmp/chat-a.sock".to_string()),
        "CLAUDE_SESSION_ID" => Some("chat-a".to_string()),
        _ => None,
    };
    let found = identify(&[], &env, None).unwrap();
    assert_eq!(found.id, "chat-a");
    assert_eq!(found.reply_socket.as_deref(), Some("/tmp/chat-a.sock"));

    // c) a message goes over ssh and into B's chat socket.
    let driver = Caller {
        id: "chat-a".into(),
        name: "Driver".into(),
        reply_socket: None,
    };
    let sent = send_text(&store, &me, &driver, "Planner on B", "hello from A").unwrap();
    assert_eq!(sent.status, "delivered", "{}", sent.detail);
    let line = line_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("B's chat socket got a message");
    let frame: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    let content = frame["message"]["content"].as_str().unwrap();
    assert!(content.contains("<cross-session-message"), "{content}");
    assert!(content.contains("from-name=\"Driver on "), "{content}");
    assert!(content.contains(" via Pulse\""), "{content}");
    assert!(content.contains("hello from A"), "{content}");

    // d) once the chat is gone B refuses; A's roster no longer offers it.
    std::fs::remove_file(&session_file).unwrap();
    let gone = Envelope::new(
        Sender {
            device: "A".into(),
            session: "chat-a".into(),
            name: "Driver on A".into(),
        },
        Target {
            device: "B".into(),
            session: "chat-b".into(),
        },
        "anyone there?",
    )
    .unwrap();
    let link = links::find(&store, "b").expect("link kept");
    let receipt = links::post(&link, &gone).unwrap();
    assert_eq!(receipt["status"], "refused", "{receipt}");
    assert!(
        receipt["detail"].as_str().unwrap().contains("isn't open"),
        "{receipt}"
    );
    assert!(send_text(&store, &me, &driver, "Planner on B", "again").is_err());

    // e) unlinking removes the remote chats.
    assert!(links::remove(&store, "B").unwrap());
    assert!(!links::remove(&store, "B").unwrap());
    assert!(all_peers(&store, &me).iter().all(|p| p.local));

    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&b);
}

#[test]
fn two_computers_message_each_other_over_ssh() {
    #[cfg(unix)]
    journey();
    roster_and_wire_format();
}
