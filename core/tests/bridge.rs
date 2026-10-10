//! One journey through the Pulse bridge with two computers over ssh. Computer
//! B (its own HOME) has a Claude chat with a fake messaging socket; a shell
//! script stands in for ssh and runs the real `pulse` binary as B. Computer A
//! links B, lists its chats, sends a message that B's `pulse bridge post`
//! delivers into that socket, then sees a refusal once the chat is gone, and
//! unlinks. Delivery into a real Claude or Codex chat is not exercised here.
//! On Windows the chat is a named pipe (`\\.\pipe\LOCAL\cc-msg-*`), the ssh stand-in
//! a `.cmd`, the chat gets the auth frame before the message, and the same
//! journey covers the hub's reply pipe, the reply address a delivery advertises,
//! a chat's answer arriving as a reply, and `procStart` identity.
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
        updated_ms: None,
        liveness: "unknown".into(),
    }
}

fn remote(device: &str, chats: Vec<RosterEntry>) -> RemoteChats {
    RemoteChats {
        link: Link {
            device: device.into(),
            ssh: "fake".into(),
            pulse: "pulse".into(),
            ..Default::default()
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

/// What a real Claude chat (Claude Code 2.1.295) demands of the wrapper before it takes the
/// message as one from a peer: exactly these attributes in this order, a class it knows
/// that matches a chat bypassing permissions, a sender name it would not rewrite (64
/// characters at most, no control characters, trimmed) and no closing tag inside the text.
/// A real chat parks anything else for an approval and drops it without an answer, which
/// the fake chat below (it answers "delivered" to every line) would never show.
fn claude_accepts(content: &str) -> Result<(), String> {
    let rest = content
        .strip_prefix("<cross-session-message")
        .ok_or("no opening tag")?;
    let (mut head, body) = rest
        .split_once(">\n")
        .ok_or("the opening tag does not end")?;
    let body = body
        .strip_suffix("\n</cross-session-message>")
        .ok_or("no closing tag at the end")?;
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for key in ["from", "from-session", "from-name", "from-mode"] {
        let prefix = format!(" {key}=\"");
        if let Some(after) = head.strip_prefix(prefix.as_str()) {
            let (value, tail) = after
                .split_once('"')
                .ok_or_else(|| format!("{key} is not closed"))?;
            seen.push((key, value));
            head = tail;
        }
    }
    if !head.is_empty() {
        return Err(format!("attributes Claude does not know: {head}"));
    }
    let value = |wanted: &str| {
        seen.iter()
            .find(|(key, _)| *key == wanted)
            .map(|(_, value)| *value)
            .ok_or_else(|| format!("{wanted} is missing"))
    };
    let from = value("from")?;
    if from.is_empty()
        || !from
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "%:_/.\\-".contains(c))
    {
        return Err(format!("from has characters Claude refuses: {from}"));
    }
    let session = value("from-session")?;
    if session.is_empty()
        || session.len() > 80
        || !session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("from-session is not a plain id: {session}"));
    }
    let name = value("from-name")?;
    if name.is_empty()
        || name != name.trim()
        || name.chars().count() > 64
        || name.chars().any(|c| c.is_control() || c == '<' || c == '>')
    {
        return Err(format!("Claude would rewrite from-name: {name}"));
    }
    let mode = value("from-mode")?;
    if mode != "bypass" {
        return Err(format!(
            "from-mode {mode} is held by a chat that bypasses permissions"
        ));
    }
    if body
        .to_ascii_lowercase()
        .contains("</cross-session-message")
    {
        return Err("the text holds a closing tag".to_string());
    }
    Ok(())
}

/// Computer B's Claude chat: a messaging socket that forwards every line it reads.
/// Returns the socket path and the lines received.
#[cfg(unix)]
fn fake_chat(b: &std::path::Path) -> (String, std::sync::mpsc::Receiver<String>) {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    let socket = b.join("cc.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (line_tx, line_rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        while let Ok((stream, _)) = listener.accept() {
            let mut line = String::new();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            if reader.read_line(&mut line).is_ok() && !line.is_empty() {
                let _ = line_tx.send(line);
                // Claude answers a frame with a status; without one Pulse reports "sent".
                let ack = "{\"type\":\"control\",\"action\":\"peer_message_status\",\
                           \"status\":\"delivered\"}\n";
                let _ = (&stream).write_all(ack.as_bytes());
            }
        }
    });
    (socket.to_string_lossy().into_owned(), line_rx)
}

/// The same chat on Windows: a named pipe in Claude's `LOCAL\cc-msg-<hash>` shape that
/// reads the auth frame and the user frame, answers with a delivered status, and
/// forwards both lines.
#[cfg(windows)]
fn fake_chat(_b: &std::path::Path) -> (String, std::sync::mpsc::Receiver<String>) {
    use std::io::{BufRead, BufReader, Write};
    use std::os::windows::io::FromRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows::core::PCWSTR;

    struct Pipe(HANDLE);
    // SAFETY: a kernel handle moved to the one thread that uses it.
    unsafe impl Send for Pipe {}

    let name = format!(
        r"\\.\pipe\LOCAL\cc-msg-{}",
        pulse_core::localsend::proto::random_hex(8)
    );
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let create = move || {
        // SAFETY: `wide` is NUL-terminated; default (same-user) security.
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                255,
                64 * 1024,
                64 * 1024,
                0,
                None,
            )
        };
        assert!(!handle.is_invalid(), "CreateNamedPipeW failed");
        Pipe(handle)
    };
    let first = create();
    let (line_tx, line_rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut current = first;
        loop {
            // SAFETY: a valid pipe instance; a client that raced in is fine.
            let _ = unsafe { ConnectNamedPipe(current.0, None) };
            let served = std::mem::replace(&mut current, create());
            let tx = line_tx.clone();
            std::thread::spawn(move || {
                // Capture the whole `Pipe` (Send), not its raw-pointer field.
                let served = served;
                // SAFETY: the handle is owned by `served` and moved into the File.
                let file = unsafe { std::fs::File::from_raw_handle(served.0.0) };
                let mut reader = BufReader::new(&file);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let user = line.contains("\"type\":\"user\"");
                    let _ = tx.send(line);
                    if user {
                        let ack = "{\"type\":\"control\",\"action\":\"peer_message_status\",\"status\":\"delivered\"}\n";
                        let _ = (&file).write_all(ack.as_bytes());
                        let _ = file.sync_all(); // returns once the client has read it
                        break;
                    }
                }
            });
        }
    });
    (name, line_rx)
}

/// A stand-in for ssh that runs B's `pulse` as B.
#[cfg(unix)]
fn fake_ssh(a: &std::path::Path, b: &std::path::Path, socket: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = a.join("fake-ssh.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nshift\nexec env HOME='{b}' CLAUDE_CONFIG_DIR='{b}/.claude' \
             CLAUDE_CODE_MESSAGING_SOCKET='{sock}' \"$@\"\n",
            b = b.display(),
            sock = socket
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

#[cfg(windows)]
fn fake_ssh(a: &std::path::Path, b: &std::path::Path, _socket: &str) -> std::path::PathBuf {
    // `for /f` strips the host argument and runs the rest verbatim (`%*`, unlike `%2`,
    // keeps the `=` of a base64 envelope). Rust quotes every argument it hands a batch
    // file, so the rest starts with a quote; `call` keeps cmd from reading the whole
    // line as one command name.
    let script = a.join("fake-ssh.cmd");
    std::fs::write(
        &script,
        format!(
            "@echo off\r\nset \"USERPROFILE={b}\"\r\nset \"LOCALAPPDATA={b}\\local\"\r\n\
             set \"APPDATA={b}\\roaming\"\r\nset \"CLAUDE_CONFIG_DIR={b}\\.claude\"\r\n\
             for /f \"usebackq tokens=1,* delims= \" %%a in ('%*') do call %%b\r\n",
            b = b.display()
        ),
    )
    .unwrap();
    script
}

/// Windows only: the hub's reply pipe, in-process delivery with its auth frame and
/// reply address, a chat's answer arriving as a `ReplyMessage`, and a recorded
/// `procStart` that must match the process.
#[cfg(windows)]
fn windows_replies(
    line_rx: &std::sync::mpsc::Receiver<String>,
    sessions: &std::path::Path,
    pid: u32,
) {
    use pulse_core::bridge::LocalSession;
    use pulse_core::bridge::deliver_claude::{ReplyHub, deliver, set_reply_hub};
    use std::io::Write;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    let recv = || {
        let line = line_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the chat pipe got a frame");
        serde_json::from_str::<serde_json::Value>(line.trim()).unwrap()
    };
    let (reply_tx, reply_rx) = mpsc::channel();
    let hub = ReplyHub::new(
        Arc::new(
            move |reply: pulse_core::bridge::deliver_claude::ReplyMessage| {
                let _ = reply_tx.send(reply);
            },
        ),
        Arc::new(|id: &str| id == "chat-b"),
    );
    let address = hub.address_for("A:chat-a").expect("a reply pipe");
    // One pipe for every peer, named the way Claude names its own (it sends its notice
    // about a held or refused message only to such a pipe).
    let hex = address
        .strip_prefix(r"\\.\pipe\LOCAL\cc-msg-")
        .unwrap_or_else(|| panic!("{address}"));
    assert!(
        hex.len() == 32 && hex.chars().all(|c| c.is_ascii_hexdigit()),
        "{address}"
    );
    assert_eq!(
        hub.address_for("C:chat-c").as_deref(),
        Some(address.as_str())
    );
    assert_eq!(
        hub.address_for("A:chat-a").as_deref(),
        Some(address.as_str())
    );
    set_reply_hub(Some(hub));

    let session = LocalSession {
        id: "chat-b".into(),
        kind: "claude".into(),
        name: "Planner".into(),
        cwd: String::new(),
        status: "idle".into(),
        updated_ms: None,
        liveness: "unknown".into(),
        pid: Some(pid),
        messaging_socket: None,
        peer_protocol: Some(1),
        entrypoint: None,
        raw: serde_json::Value::Null,
    };
    let envelope = Envelope::new(
        Sender {
            device: "A".into(),
            session: "chat-a".into(),
            name: "Driver on A".into(),
        },
        Target {
            device: "B".into(),
            session: "chat-b".into(),
        },
        "direct hello",
    )
    .unwrap();

    // The chat sees the auth frame first, then a user frame that names the reply pipe.
    let receipt = deliver(&session, &envelope).unwrap();
    assert_eq!(receipt.state.as_str(), "delivered", "{}", receipt.detail);
    let auth = recv();
    assert_eq!(auth["type"], "auth");
    assert_eq!(auth["token"], "x");
    let user = recv();
    assert_eq!(user["type"], "user");
    assert_eq!(user["from"], format!("uds:{address}"));
    assert!(
        user["message"]["content"]
            .as_str()
            .unwrap()
            .contains("direct hello")
    );

    // Its answer, written to that pipe (auth frame first, as Claude does), becomes a reply.
    let answer = serde_json::json!({
        "msgV": 1, "msg_id": "r1", "type": "user", "priority": "next",
        "message": {"role": "user", "content": "<cross-session-message from=\"uds:x\" \
            from-session=\"chat-b\" from-name=\"Planner\" from-mode=\"bypass\">\nreply text\n\
            </cross-session-message>"},
    });
    let mut client = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&address)
        .expect("open the reply pipe");
    writeln!(
        client,
        "{}",
        serde_json::json!({"type": "auth", "token": "ignored"})
    )
    .unwrap();
    writeln!(client, "{answer}").unwrap();
    drop(client);
    let reply = reply_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("a reply arrived");
    assert_eq!(reply.peer_key, "A:chat-a");
    assert_eq!(reply.from_session_id, "chat-b");
    assert_eq!(reply.from_name, "Planner");
    assert_eq!(reply.text, "reply text");

    // A recorded start time must match the process; a reused pid doesn't.
    let key = std::fs::read_dir(sessions)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "key"))
        .expect("key file");
    let write_key = |start: &str| {
        std::fs::write(
            &key,
            serde_json::json!({"peerToken": "x", "pidDomain": "windows", "procStart": start})
                .to_string(),
        )
        .unwrap();
    };
    write_key("1");
    assert!(
        deliver(&session, &envelope).is_err(),
        "a pid with another start time is not the chat"
    );
    let mut system = sysinfo::System::new();
    let target = sysinfo::Pid::from_u32(pid);
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[target]), true);
    write_key(&system.process(target).unwrap().start_time().to_string());
    let again = deliver(&session, &envelope).unwrap();
    assert_eq!(again.state.as_str(), "delivered", "{}", again.detail);
    assert_eq!(recv()["type"], "auth");
    assert_eq!(recv()["type"], "user");
    set_reply_hub(None);
}

#[cfg(any(unix, windows))]
fn journey() {
    use pulse_core::bridge::store::Store;
    use pulse_core::bridge::{Caller, all_peers, identify, local_identity, send_text};
    use std::path::PathBuf;
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

    // Computer B: one Claude chat with a fake messaging socket (a named pipe on Windows).
    let sessions = b.join(".claude/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    // The only test in this binary, so no other thread reads the environment yet.
    #[cfg(windows)]
    unsafe {
        std::env::set_var("CLAUDE_CONFIG_DIR", b.join(".claude"));
    }
    let (socket, line_rx) = fake_chat(&b);
    let pid = std::process::id();
    let session_file = sessions.join(format!("{pid}.json"));
    std::fs::write(
        &session_file,
        serde_json::json!({
            "pid": pid, "sessionId": "chat-b", "name": "Planner",
            "cwd": b.to_string_lossy(), "status": "idle", "peerProtocol": 1,
            "messagingSocketPath": socket, "entrypoint": "claude-desktop",
        })
        .to_string(),
    )
    .unwrap();
    #[cfg(unix)]
    let key = r#"{"peerToken": "x"}"#;
    #[cfg(windows)]
    let key = r#"{"peerToken": "x", "pidDomain": "windows"}"#;
    std::fs::write(sessions.join(format!("{pid}.{}.key", "ab".repeat(32))), key).unwrap();

    // The fake ssh: drops the host argument and runs B's pulse as B.
    let pulse = env!("CARGO_BIN_EXE_pulse");
    let script = fake_ssh(&a, &b, &socket);
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
            ..Default::default()
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
    // On Windows the chat gets the auth frame (the key file's token) before the message.
    #[cfg(windows)]
    {
        let auth = line_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("B's chat pipe got the auth frame");
        let auth: serde_json::Value = serde_json::from_str(auth.trim()).unwrap();
        assert_eq!(auth["type"], "auth");
        assert_eq!(auth["token"], "x");
    }
    let line = line_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("B's chat socket got a message");
    let frame: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    let content = frame["message"]["content"].as_str().unwrap();
    assert!(content.contains("<cross-session-message"), "{content}");
    assert_eq!(claude_accepts(content), Ok(()), "{content}");
    assert!(content.contains("from-name=\"Driver on "), "{content}");
    assert!(content.contains(" via Pulse\""), "{content}");
    assert!(content.contains("hello from A"), "{content}");

    // c2) Windows: reply pipe, reply address, answer routing and start-time identity.
    #[cfg(windows)]
    windows_replies(&line_rx, &sessions, pid);

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
    #[cfg(any(unix, windows))]
    journey();
    roster_and_wire_format();
}
