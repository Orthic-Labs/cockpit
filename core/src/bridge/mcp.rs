//! `pulse bridge mcp`: a stdio MCP server (JSON-RPC 2.0, one message per line)
//! that lets a chat list the other chats it can reach, message them, and read
//! what was sent to it. Tools: `bridge_list`, `bridge_send`, `bridge_inbox`,
//! `bridge_whoami`.
//!
//! The chat's own id comes from its environment (`CLAUDE_SESSION_ID`,
//! `CODEX_THREAD_ID`, …), else `--session`, else an id made once per working
//! folder.

use super::roster::{self, LocalSession};
use super::store::{RegisteredSession, Store};
use super::{BridgeError, Identity, all_peers, envelope::now_ms, local_identity, send_text};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::time::Duration;

const SUPPORTED_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
const REMOTE_SEND_WAIT: Duration = Duration::from_secs(8);

/// Who is calling.
#[derive(Debug, Clone)]
pub struct CallerSession {
    pub id: String,
    /// "claude" or "codex".
    pub kind: String,
    pub name: String,
    pub cwd: String,
}

/// Work out the calling chat. `env` reads one environment variable.
pub fn identify(
    store: &Store,
    env: &dyn Fn(&str) -> Option<String>,
    session: Option<String>,
    kind: Option<String>,
    name: Option<String>,
    cwd: &str,
) -> Result<CallerSession, BridgeError> {
    let first = |names: &[&str]| {
        names
            .iter()
            .find_map(|n| env(n).filter(|v| !v.trim().is_empty()))
    };
    let from_claude = first(&["CLAUDE_SESSION_ID", "CLAUDE_CODE_SESSION_ID"]);
    let from_codex = first(&["CODEX_THREAD_ID", "CODEX_SESSION_ID"]);
    let (id, found_kind) = match (session, from_claude, from_codex) {
        (Some(id), _, _) => (Some(id), None),
        (None, Some(id), _) => (Some(id), Some("claude")),
        (None, None, Some(id)) => (Some(id), Some("codex")),
        _ => (None, None),
    };
    let kind = kind
        .or_else(|| found_kind.map(str::to_string))
        .unwrap_or_else(|| "codex".to_string());
    let id = match id {
        Some(id) => id,
        None => store.cwd_session_id(cwd, &kind)?,
    };
    let name = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
        std::path::Path::new(cwd)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Chat".to_string())
    });
    Ok(CallerSession {
        id,
        kind,
        name,
        cwd: cwd.to_string(),
    })
}

pub struct Server {
    pub store: Store,
    pub me: Identity,
    pub caller: CallerSession,
}

fn text_result(value: &Value, is_error: bool) -> Value {
    json!({
        "content": [{"type": "text", "text": value.to_string()}],
        "isError": is_error,
    })
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": "bridge_list",
            "description": "List the chats you can message through Pulse: other chats on this computer and on paired computers, with device, name, folder and status.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        },
        {
            "name": "bridge_send",
            "description": "Send a text message (up to 64 KiB) to another chat. `to` is an id, or a name from bridge_list. Returns the message id and whether it was delivered, held, refused, or queued for another computer.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": {"type": "string", "description": "Peer id or name from bridge_list"},
                    "text": {"type": "string", "description": "The message"}
                },
                "required": ["to", "text"],
                "additionalProperties": false
            }
        },
        {
            "name": "bridge_inbox",
            "description": "Unread messages for this chat (or for `session`), marked read. Use it when a message was held or refused.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": {"type": "string", "description": "Chat id; defaults to this chat"},
                    "since": {"type": "integer", "description": "Only messages received after this time (milliseconds since 1970)"}
                },
                "additionalProperties": false
            }
        },
        {
            "name": "bridge_whoami",
            "description": "This chat as the bridge knows it: its id, name, computer, and whether the Pulse relay is running.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        }
    ])
}

impl Server {
    /// Record this chat so rosters list it; repeated while the server runs.
    pub fn register(&self) {
        let _ = self.store.register_session(&RegisteredSession {
            id: self.caller.id.clone(),
            kind: self.caller.kind.clone(),
            name: self.caller.name.clone(),
            cwd: self.caller.cwd.clone(),
            pid: std::process::id(),
            updated: now_ms(),
        });
    }

    /// The chat's title: the host's own when known, else the registered one.
    fn title(&self) -> String {
        let sessions: Vec<LocalSession> = roster::local_sessions(&self.store);
        sessions
            .iter()
            .find(|s| s.id == self.caller.id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| self.caller.name.clone())
    }

    fn call_tool(&self, name: &str, args: &Value) -> Result<Value, BridgeError> {
        match name {
            "bridge_list" => {
                let peers = all_peers(&self.store, &self.me);
                let rows: Vec<Value> = peers
                    .iter()
                    .map(|p| {
                        let mut row = serde_json::to_value(p).unwrap_or(Value::Null);
                        row["self"] = json!(p.local && p.session == self.caller.id);
                        row
                    })
                    .collect();
                Ok(json!({"peers": rows, "relayRunning": self.store.relay_alive()}))
            }
            "bridge_send" => {
                let to = args["to"].as_str().unwrap_or("");
                let text = args["text"].as_str().unwrap_or("");
                if text.is_empty() {
                    return Err(BridgeError::Invalid("text is empty".into()));
                }
                let outcome = send_text(
                    &self.store,
                    &self.me,
                    &self.caller.id,
                    &self.title(),
                    to,
                    text,
                    REMOTE_SEND_WAIT,
                )?;
                Ok(json!({
                    "msg_id": outcome.msg_id,
                    "status": outcome.status,
                    "detail": outcome.detail,
                    "to": outcome.to.display,
                }))
            }
            "bridge_inbox" => {
                let session = args["session"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&self.caller.id);
                let since = args["since"].as_u64();
                let entries = self.store.take_unread(session, since)?;
                let messages: Vec<Value> = entries
                    .iter()
                    .map(|e| {
                        json!({
                            "seq": e.seq,
                            "id": e.env.id,
                            "received": e.received,
                            "ts": e.env.ts,
                            "from": {
                                "device": e.env.from.device,
                                "session": e.env.from.session,
                                "name": e.env.from.name,
                            },
                            "body": e.env.body,
                        })
                    })
                    .collect();
                Ok(json!({"session": session, "messages": messages}))
            }
            "bridge_whoami" => Ok(json!({
                "session": self.caller.id,
                "kind": self.caller.kind,
                "name": self.title(),
                "cwd": self.caller.cwd,
                "device": self.me.device,
                "deviceAlias": self.me.alias,
                "unread": self.store.unread_count(&self.caller.id),
                "relayRunning": self.store.relay_alive(),
            })),
            other => Err(BridgeError::Invalid(format!("unknown tool {other}"))),
        }
    }

    /// One JSON-RPC message in, the reply line out (none for notifications).
    pub fn handle(&self, line: &str) -> Option<String> {
        let request: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                return Some(error_reply(&Value::Null, -32700, "Parse error").to_string());
            }
        };
        // A notification (initialized, cancelled, …) has no id and gets no answer.
        let id = request.get("id").cloned()?;
        let method = request["method"].as_str().unwrap_or("");
        let params = &request["params"];
        let reply = match method {
            "initialize" => {
                let wanted = params["protocolVersion"].as_str().unwrap_or("");
                let version = SUPPORTED_VERSIONS
                    .iter()
                    .find(|v| **v == wanted)
                    .unwrap_or(&SUPPORTED_VERSIONS[0]);
                ok_reply(
                    &id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {"name": "pulse-bridge", "version": env!("CARGO_PKG_VERSION")},
                        "instructions": "Message other AI chats on this computer or paired computers. Call bridge_list for peers, bridge_send to write, bridge_inbox for held messages.",
                    }),
                )
            }
            "ping" => ok_reply(&id, json!({})),
            "tools/list" => ok_reply(&id, json!({"tools": tool_definitions()})),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or("");
                match self.call_tool(name, &params["arguments"]) {
                    Ok(value) => ok_reply(&id, text_result(&value, false)),
                    Err(e) => ok_reply(&id, text_result(&json!({"error": e.to_string()}), true)),
                }
            }
            _ => error_reply(&id, -32601, "Method not found"),
        };
        Some(reply.to_string())
    }

    /// Serve until `input` ends.
    pub fn serve<R: BufRead, W: Write>(&self, input: R, output: &mut W) -> std::io::Result<()> {
        self.register();
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(reply) = self.handle(&line) {
                writeln!(output, "{reply}")?;
                output.flush()?;
            }
        }
        self.store.forget_session(&self.caller.id);
        Ok(())
    }
}

fn ok_reply(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_reply(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Run on this process's stdin and stdout, re-registering the chat every 20
/// seconds so rosters keep listing it.
pub fn run(
    session: Option<String>,
    kind: Option<String>,
    name: Option<String>,
) -> Result<(), BridgeError> {
    let store = Store::open_default()?;
    let me = local_identity(&store)?;
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let caller = identify(
        &store,
        &|k| std::env::var(k).ok(),
        session,
        kind,
        name,
        &cwd,
    )?;
    let server = std::sync::Arc::new(Server { store, me, caller });
    let beat = server.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(20));
            beat.register();
        }
    });
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    server.serve(stdin.lock(), &mut stdout)?;
    Ok(())
}
