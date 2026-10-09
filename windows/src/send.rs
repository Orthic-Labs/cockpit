//! Nearby sharing in the notch (the Windows counterpart of mac/Notch/Sources/Sharing): the Send
//! ring, its hover card, Paste, file drops, and the incoming-request, "Saved to Downloads",
//! message and error cards. The LocalSend protocol runs in the hub
//! (hub/src-tauri/src/share.rs); this module only talks to it through files and two named
//! events in `%LOCALAPPDATA%\Pulse`, exactly as the Mac notch does with Darwin notifications:
//!
//! * the hub publishes `share-state.json` and signals `Local\dev.orthic.pulse.share.state`
//!   (at least every four seconds); a state older than 15 s means the hub is not running;
//! * the notch drops one JSON file per command into `share-commands\` (send, accept, decline,
//!   cancel, refresh) and signals `Local\dev.orthic.pulse.share.command`;
//! * the notch starts `pulse-hub.exe --background` (no window) when sharing is on and no hub
//!   answers.
//!
//! Threading: `start` runs one watcher thread that reads the state, expires cards and posts the
//! caller's window message whenever anything visible changed. Everything else is called on the
//! UI thread (the hot key needs the thread that owns the window) and only reads or mutates the
//! model behind one lock; nothing here draws. Wiring is described in the report that came with
//! this file; every public item says what it is for.

#![allow(dead_code)]

use crate::card::{Button, CardContent, Head, Hit, Lead, Live, Mark, Row};
use crate::diag;
use crate::glyphs::{Symbol, Tile};
use crate::json::{self, Value};
use std::collections::HashSet;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[path = "send_sys.rs"]
mod sys;

/// Short text mark drawn inside the Send ring (like `Cell::glyph`).
pub const GLYPH: &str = "Snd";
/// Hub section a click on the Send cell opens (Settings, Nearby sharing).
pub const SECTION: &str = "general";
/// `WM_HOTKEY` id of the Ctrl+V paste key while the pointer is on the Send cell.
pub const HOTKEY_ID: i32 = 0x5E4D;

const STATE_EVENT: &str = "dev.orthic.pulse.share.state";
const COMMAND_EVENT: &str = "dev.orthic.pulse.share.command";
const HUB_EXE: &str = "pulse-hub.exe";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const FRESH_MS: f64 = 15_000.0;
const MAX_STATE_BYTES: usize = 4 * 1024 * 1024;

// ---- what the hub publishes ---------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct Device {
    fingerprint: String,
    alias: String,
    device_type: Option<String>,
    device_model: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct Incoming {
    id: String,
    from: String,
    file_count: usize,
    total_bytes: u64,
    is_message: bool,
    preview: Option<String>,
    first_file: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct Transfer {
    id: String,
    /// "send" or "receive".
    direction: String,
    peer: String,
    /// waiting, active, done, failed, cancelled or declined.
    state: String,
    total_bytes: u64,
    done_bytes: u64,
    files_total: usize,
    files_done: usize,
    current: Option<String>,
    saved_to: Option<String>,
    saved_files: Vec<String>,
    error: Option<String>,
    message: Option<String>,
}

impl Transfer {
    fn is_open(&self) -> bool {
        self.state == "active" || self.state == "waiting"
    }

    fn fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.done_bytes as f64 / self.total_bytes as f64).clamp(0.0, 1.0) as f32
        }
    }
}

/// Shown on the Send card while Windows Firewall blocks Pulse (the Mac says Local Network).
pub const FIREWALL_HINT: &str = "Allow Pulse through Windows Firewall.";

#[derive(Clone, Debug, PartialEq)]
struct State {
    running: bool,
    error: Option<String>,
    /// "port_in_use" when another program (the LocalSend app) holds port 53317.
    error_kind: Option<String>,
    /// "blocked" when Windows Firewall refuses Pulse's local network traffic (hub `localNetwork`).
    local_network: Option<String>,
    save_dir: Option<String>,
    devices: Vec<Device>,
    incoming: Vec<Incoming>,
    transfers: Vec<Transfer>,
    warnings: Vec<String>,
    notice: Option<(u64, String)>,
    scanning: bool,
    updated_at: f64,
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn number(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

fn items<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value.get(key).and_then(Value::as_array).unwrap_or(&[])
}

/// The state file. The reader has no booleans, so the hub also writes `runningN`, `scanningN`
/// and each request's `isMessageN` as 0/1.
fn parse_state(bytes: &[u8]) -> Option<State> {
    let root = json::parse(bytes, MAX_STATE_BYTES)?;
    let devices = items(&root, "devices")
        .iter()
        .filter_map(|d| {
            Some(Device {
                fingerprint: text(d, "fingerprint").filter(|f| !f.is_empty())?,
                alias: text(d, "alias").filter(|a| !a.is_empty())?,
                device_type: text(d, "deviceType"),
                device_model: text(d, "deviceModel"),
            })
        })
        .collect();
    let incoming = items(&root, "incoming")
        .iter()
        .filter_map(|i| {
            Some(Incoming {
                id: text(i, "id")?,
                from: text(i, "from").unwrap_or_default(),
                file_count: number(i, "fileCount") as usize,
                total_bytes: number(i, "totalBytes") as u64,
                is_message: number(i, "isMessageN") >= 1.0,
                preview: text(i, "preview"),
                first_file: items(i, "files").first().and_then(|f| text(f, "name")),
            })
        })
        .collect();
    let transfers = items(&root, "transfers")
        .iter()
        .filter_map(|t| {
            Some(Transfer {
                id: text(t, "id")?,
                direction: text(t, "direction").unwrap_or_default(),
                peer: text(t, "peer").unwrap_or_default(),
                state: text(t, "state").unwrap_or_default(),
                total_bytes: number(t, "totalBytes") as u64,
                done_bytes: number(t, "doneBytes") as u64,
                files_total: number(t, "filesTotal") as usize,
                files_done: number(t, "filesDone") as usize,
                current: text(t, "current"),
                saved_to: text(t, "savedTo"),
                saved_files: items(t, "savedFiles")
                    .iter()
                    .filter_map(|f| f.as_str().map(str::to_string))
                    .collect(),
                error: text(t, "error"),
                message: text(t, "message"),
            })
        })
        .collect();
    let notice = root
        .get("notice")
        .and_then(|n| Some((number(n, "id") as u64, text(n, "text")?)));
    Some(State {
        running: number(&root, "runningN") >= 1.0,
        error: text(&root, "error"),
        error_kind: text(&root, "errorKind"),
        local_network: text(&root, "localNetwork"),
        save_dir: text(&root, "saveDir"),
        devices,
        incoming,
        transfers,
        warnings: items(&root, "warnings")
            .iter()
            .filter_map(|w| w.as_str().map(str::to_string))
            .collect(),
        notice,
        scanning: number(&root, "scanningN") >= 1.0,
        updated_at: number(&root, "updatedAt"),
    })
}

// ---- what the notch shows -------------------------------------------------------------------

/// A click on a row of a panel (or a paste/drop answer) the caller passes to `perform`.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// A device was picked (device row of the hover card or the "Send to…" card).
    SendTo(String),
    /// Send the clipboard.
    Paste,
    /// Put the last thing received (a message, or the saved files) back on the clipboard.
    CopyLast,
    /// Announce again and sweep the subnet.
    Refresh,
    /// Accept the incoming request.
    Accept,
    /// Decline the incoming request.
    Decline,
    /// Stop the transfer in flight.
    Cancel,
    /// Put the card away (a transfer carries on and is announced when it ends).
    Close,
    /// Show the saved file in Explorer.
    Show,
    /// Copy the received message.
    Copy,
    /// Open the received message as a web address.
    OpenLink,
    /// A row of an installer card (`installer.rs`), run by that module.
    Installer(crate::installer::Choice),
}

/// Content of the hover card or of a notch card. `actions[i]` belongs to `content.rows[i]`
/// (one entry per thing on the row that can be pressed: a button row has one per pill, then
/// one for its close; any other row has one); `head[i]` to `content.head[i]`. A button with
/// an action is clickable and should look it, the others are text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Panel {
    pub content: CardContent,
    pub actions: Vec<Vec<Option<Action>>>,
    pub head: Vec<Option<Action>>,
}

impl Panel {
    /// A panel for `content` whose every button is dead; rows and header already in
    /// `content` are taken as they stand.
    pub fn new(content: CardContent) -> Self {
        let actions = content.rows.iter().map(|r| vec![None; r.slots()]).collect();
        let head = vec![None; content.head.len()];
        Self {
            content,
            actions,
            head,
        }
    }

    /// Appends a row that is one thing to press (or text, with no action).
    pub fn row(&mut self, row: Row, action: Option<Action>) {
        self.content.rows.push(row);
        self.actions.push(vec![action]);
    }

    /// Appends a button row with the action of each pill, then of its close.
    pub fn buttons(&mut self, row: Row, actions: Vec<Option<Action>>) {
        self.content.rows.push(row);
        self.actions.push(actions);
    }

    /// Sets the header's round buttons and their actions.
    pub fn heads(&mut self, heads: Vec<(Head, Option<Action>)>) {
        self.content.head = heads.iter().map(|(head, _)| *head).collect();
        self.head = heads.into_iter().map(|(_, action)| action).collect();
    }

    /// Which buttons have an action, for the renderer.
    pub fn live(&self) -> Live {
        Live {
            head: self.head.iter().map(Option::is_some).collect(),
            rows: self
                .actions
                .iter()
                .map(|row| row.iter().map(Option::is_some).collect())
                .collect(),
        }
    }

    /// The action of the button under `hit`.
    pub fn action(&self, hit: Hit) -> Option<Action> {
        match hit {
            Hit::Head(index) => self.head.get(index).cloned().flatten(),
            Hit::Row(row, button) => self
                .actions
                .get(row)
                .and_then(|buttons| buttons.get(button))
                .cloned()
                .flatten(),
        }
    }
}

/// The Send ring: an empty track while idle, an arc while bytes move.
#[derive(Clone, Debug, PartialEq)]
pub struct Ring {
    /// Used share of the transfer in flight; `None` draws no arc.
    pub fraction: Option<f32>,
    /// A transfer is waiting or moving.
    pub active: bool,
    /// Sharing cannot work (port taken, not started): draw the glyph in the warning colour.
    pub problem: bool,
    /// Text under the ring: "Idle", "Off", "Starting…" or the percentage.
    pub label: String,
}

#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    ring: Ring,
    hover: Panel,
    popup: Option<Panel>,
}

#[derive(Clone, Debug, PartialEq)]
enum Card {
    None,
    Incoming(String),
    Saved(Vec<String>),
    Message(String),
    Note,
    Choose,
    Sending,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stage {
    Waiting,
    Active,
    Done,
    Problem,
}

#[derive(Clone, Debug, PartialEq)]
pub enum View {
    /// Detail text and the prompt's own buttons.
    Plain,
    /// "Send to…": fingerprint, alias, model and symbol of each device.
    List {
        rows: Vec<(String, String, String, Symbol)>,
        scanning: bool,
    },
    Transfer {
        stage: Stage,
        fraction: Option<f32>,
        can_cancel: bool,
        /// The peer's device symbol, or the outcome's.
        symbol: Symbol,
    },
}

/// A card of the send flow, before it is laid out as a `Panel`.
#[derive(Clone, Debug, PartialEq)]
pub struct Prompt {
    pub title: String,
    pub detail: String,
    pub problem: bool,
    pub buttons: Vec<(String, Action)>,
    pub view: View,
    /// The large icon of a plain card.
    pub lead: Lead,
}

/// The symbol on a pill, as the Mac's `DiskImageCard.symbol(for:)` picks it.
fn pill_symbol(action: &Action) -> Symbol {
    match action {
        Action::Accept => Symbol::DownApp,
        Action::Decline | Action::Cancel => Symbol::Stop,
        Action::Show => Symbol::Folder,
        Action::Close => Symbol::Clock,
        Action::Copy | Action::Paste | Action::CopyLast => Symbol::Copy,
        Action::OpenLink => Symbol::Compass,
        Action::Refresh => Symbol::Refresh,
        Action::SendTo(_) => Symbol::Plane,
        Action::Installer(choice) => crate::installer::symbol(*choice),
    }
}

impl Prompt {
    fn plain(title: String, detail: String, problem: bool) -> Prompt {
        Prompt {
            title,
            detail,
            problem,
            buttons: Vec::new(),
            view: View::Plain,
            lead: Lead::Tile(Tile::Folder),
        }
    }

    pub fn panel(&self) -> Panel {
        let mut content = CardContent {
            title: self.title.clone(),
            subtitle: (!self.detail.is_empty()).then(|| self.detail.clone()),
            problem: self.problem,
            ..CardContent::default()
        };
        match &self.view {
            View::Plain => {
                // The Mac's pills, then a round close; on a request the close declines.
                content.lead = Some(self.lead.clone());
                content.wide = true;
                let mut panel = Panel::new(content);
                let close = if self.buttons.iter().any(|(_, a)| *a == Action::Decline) {
                    Action::Decline
                } else {
                    Action::Close
                };
                let pills: Vec<&(String, Action)> = self
                    .buttons
                    .iter()
                    .filter(|(_, action)| *action != Action::Close)
                    .collect();
                let mut actions: Vec<Option<Action>> =
                    pills.iter().map(|(_, a)| Some(a.clone())).collect();
                actions.push(Some(close));
                let buttons = pills
                    .iter()
                    .map(|(label, action)| Button::new(label.clone(), pill_symbol(action)))
                    .collect();
                panel.buttons(
                    Row::Buttons {
                        buttons,
                        close: true,
                    },
                    actions,
                );
                panel
            }
            View::List {
                rows: devices,
                scanning,
            } => {
                let mut panel = Panel::new(content);
                if devices.is_empty() {
                    panel.row(
                        Row::Waiting("Waiting for a device to appear\u{2026}".into()),
                        None,
                    );
                }
                for (fingerprint, alias, model, symbol) in devices {
                    panel.row(
                        Row::Device {
                            symbol: *symbol,
                            alias: alias.clone(),
                            model: model.clone(),
                        },
                        Some(Action::SendTo(fingerprint.clone())),
                    );
                }
                panel.heads(vec![
                    if *scanning {
                        (Head::Scanning, None)
                    } else {
                        (Head::Refresh, Some(Action::Refresh))
                    },
                    (Head::Close, Some(Action::Close)),
                ]);
                panel
            }
            View::Transfer {
                stage,
                fraction,
                can_cancel,
                symbol,
            } => {
                content.lead = Some(Lead::Disc {
                    symbol: *symbol,
                    problem: *stage == Stage::Problem,
                });
                let mut panel = Panel::new(content);
                // A bare bar (indeterminate until the receiver answers) while a transfer
                // runs, and nothing but the words once it has ended.
                if matches!(stage, Stage::Waiting | Stage::Active) {
                    panel.row(Row::Progress(*fraction), None);
                }
                let mut buttons = Vec::new();
                let mut actions = Vec::new();
                if *can_cancel {
                    buttons.push(Button::new("Cancel", Symbol::Stop));
                    actions.push(Some(Action::Cancel));
                }
                actions.push(Some(Action::Close));
                panel.buttons(
                    Row::Buttons {
                        buttons,
                        close: true,
                    },
                    actions,
                );
                panel
            }
        }
    }
}

fn percent(fraction: f32) -> String {
    format!("{}%", (fraction.clamp(0.0, 1.0) * 100.0).round() as u32)
}

/// 12.3 MB style sizes (decimal, like Explorer's "file size" wording on the Mac side).
fn bytes_text(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn kind_of(device: &Device) -> &'static str {
    match device.device_type.as_deref() {
        Some("mobile") => "Phone",
        Some("desktop") => "Computer",
        Some("web") => "Browser",
        Some("headless") => "Terminal",
        Some("server") => "Server",
        _ => "Device",
    }
}

/// The symbol of a device's type, as the Mac's `NearbySharing.symbol(of:)` picks it.
fn device_symbol(device: &Device) -> Symbol {
    match device.device_type.as_deref() {
        Some("mobile") => Symbol::Phone,
        Some("desktop") => Symbol::Laptop,
        Some("web") => Symbol::Globe,
        Some("headless") => Symbol::Terminal,
        Some("server") => Symbol::Server,
        _ => Symbol::Desktop,
    }
}

fn files_text(count: usize) -> String {
    if count == 1 {
        "1 file".to_string()
    } else {
        format!("{count} files")
    }
}

/// The message as a web address, when it is nothing else.
fn link_in(message: &str) -> Option<String> {
    let trimmed = message.trim();
    let lower = trimmed.to_ascii_lowercase();
    let web = lower.starts_with("http://") || lower.starts_with("https://");
    (web && trimmed.len() <= 2048
        && !trimmed
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"'))
    .then(|| trimmed.to_string())
}

fn folder_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

// ---- the model ----------------------------------------------------------------------------

/// The one item "Copy last" puts on the clipboard, kept in `nearby-last.json`.
#[derive(Clone, Debug, PartialEq)]
struct Last {
    /// The message, or `None` for saved files.
    text: Option<String>,
    files: Vec<String>,
    /// Seconds since 1970.
    at: u64,
}

/// Message text beyond this is cut (with a note) so the file stays small.
const LAST_TEXT_CAP: usize = 64 * 1024;
const MAX_LAST_BYTES: usize = 512 * 1024;

fn capped(text: &str) -> String {
    if text.len() <= LAST_TEXT_CAP {
        return text.to_string();
    }
    let mut end = LAST_TEXT_CAP;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n\u{2026} (truncated)", &text[..end])
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn load_last() -> Option<Last> {
    let bytes = std::fs::read(bridge_dir()?.join("nearby-last.json")).ok()?;
    let root = json::parse(&bytes, MAX_LAST_BYTES)?;
    let files: Vec<String> = items(&root, "files")
        .iter()
        .filter_map(|f| f.as_str().map(str::to_string))
        .collect();
    let text = (text(&root, "kind").as_deref() == Some("text"))
        .then(|| text(&root, "text"))
        .flatten();
    if text.is_none() && files.is_empty() {
        return None;
    }
    Some(Last {
        text,
        files,
        at: number(&root, "at") as u64,
    })
}

fn save_last(last: &Last) {
    let Some(dir) = bridge_dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let files: Vec<String> = last.files.iter().map(|f| json_text(f)).collect();
    let body = match &last.text {
        Some(t) => format!(
            "{{\"kind\":\"text\",\"text\":{},\"files\":[],\"at\":{}}}",
            json_text(t),
            last.at
        ),
        None => format!(
            "{{\"kind\":\"files\",\"files\":[{}],\"at\":{}}}",
            files.join(","),
            last.at
        ),
    };
    let temp = dir.join("nearby-last.tmp");
    if std::fs::write(&temp, body).is_ok() {
        let _ = std::fs::rename(&temp, dir.join("nearby-last.json"));
    }
}

/// "just now", "3 min ago", "2 hr ago".
fn ago(at: u64) -> String {
    let seconds = now_secs().saturating_sub(at);
    if seconds < 45 {
        "just now".to_string()
    } else if seconds < 3600 {
        format!("{} min ago", ((seconds + 30) / 60).max(1))
    } else if seconds < 86_400 {
        format!("{} hr ago", seconds / 3600)
    } else {
        format!("{} d ago", seconds / 86_400)
    }
}

impl Last {
    /// The hover card's top row: "Copy last: <preview> · <age>".
    fn row_label(&self) -> Option<String> {
        let what = match &self.text {
            Some(t) => {
                let line = t.lines().next().unwrap_or("").trim();
                if line.is_empty() {
                    return None;
                }
                if line.chars().count() > 24 {
                    format!("{}\u{2026}", line.chars().take(24).collect::<String>())
                } else {
                    line.to_string()
                }
            }
            None => {
                let name = folder_name(self.files.first()?);
                match self.files.len() {
                    1 => name,
                    n => format!("{name} +{}", n - 1),
                }
            }
        };
        Some(format!("Copy last: {what} \u{b7} {}", ago(self.at)))
    }
}

struct Pending {
    paths: Vec<PathBuf>,
    text: Option<String>,
}

struct Model {
    enabled: bool,
    state: Option<State>,
    stamp: Option<(SystemTime, u64)>,
    primed: bool,
    seen_finished: HashSet<String>,
    last_notice: u64,
    last_error: Option<String>,
    card: Card,
    prompt: Option<Prompt>,
    expiry: Option<(Instant, Card)>,
    card_hovered: bool,
    pending: Option<Pending>,
    last_choose: Option<Prompt>,
    send_peer: Option<Device>,
    send_summary: String,
    send_baseline: HashSet<String>,
    send_transfer: Option<String>,
    send_finished: bool,
    hovering: bool,
    hotkey: bool,
    drop_targeting: bool,
    hub_launched: Option<Instant>,
    hub: Option<Child>,
    last: Option<Last>,
}

impl Model {
    fn new() -> Model {
        Model {
            enabled: true,
            state: None,
            stamp: None,
            primed: false,
            seen_finished: HashSet::new(),
            last_notice: 0,
            last_error: None,
            card: Card::None,
            prompt: None,
            expiry: None,
            card_hovered: false,
            pending: None,
            last_choose: None,
            send_peer: None,
            send_summary: String::new(),
            send_baseline: HashSet::new(),
            send_transfer: None,
            send_finished: false,
            hovering: false,
            hotkey: false,
            drop_targeting: false,
            hub_launched: None,
            hub: None,
            last: None,
        }
    }

    // -- state in

    fn wall_ms() -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as f64)
            .unwrap_or(0.0)
    }

    /// The hub writes at least every four seconds; older than fifteen means it is not running.
    fn fresh(&self) -> Option<&State> {
        self.state
            .as_ref()
            .filter(|s| Self::wall_ms() - s.updated_at < FRESH_MS)
    }

    fn devices(&self) -> Vec<Device> {
        self.fresh().map(|s| s.devices.clone()).unwrap_or_default()
    }

    /// Where a paste or drop goes without asking: the only device nearby.
    fn target(&self) -> Option<Device> {
        let list = self.devices();
        (list.len() == 1).then(|| list[0].clone())
    }

    /// Reads `share-state.json` when it changed (or `force`).
    fn read_state(&mut self, force: bool) {
        let Some(path) = bridge_dir().map(|d| d.join("share-state.json")) else {
            return;
        };
        let stamp = std::fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok().map(|t| (t, m.len())));
        if !force && stamp == self.stamp {
            return;
        }
        self.stamp = stamp;
        let Some(state) = std::fs::read(&path).ok().and_then(|b| parse_state(&b)) else {
            return;
        };
        self.state = Some(state.clone());
        self.process(&state);
    }

    // -- cards

    fn remember(&mut self, last: Last) {
        save_last(&last);
        self.last = Some(last);
    }

    fn show(&mut self, prompt: Prompt, card: Card) {
        self.card = card;
        self.prompt = Some(prompt);
    }

    fn clear_card(&mut self) {
        self.expiry = None;
        self.card_hovered = false;
        if self.card == Card::None {
            return;
        }
        if self.card == Card::Choose {
            self.last_choose = None;
        }
        if self.card == Card::Sending {
            self.send_peer = None;
            self.send_transfer = None;
            self.send_finished = false;
        }
        self.card = Card::None;
        self.prompt = None;
    }

    fn schedule_expiry(&mut self, seconds: f32) {
        self.expiry = Some((
            Instant::now() + Duration::from_secs_f32(seconds),
            self.card.clone(),
        ));
    }

    fn show_note(&mut self, title: &str, detail: &str, problem: bool) {
        self.show(
            Prompt::plain(title.to_string(), detail.to_string(), problem),
            Card::Note,
        );
        self.schedule_expiry(if problem { 8.0 } else { 4.0 });
    }

    fn tick(&mut self) {
        if let Some((deadline, card)) = self.expiry.clone()
            && Instant::now() >= deadline
        {
            self.expiry = None;
            if self.card == card && !self.card_hovered {
                if card == Card::Choose {
                    self.pending = None;
                }
                self.clear_card();
            }
        }
        self.watch_hub();
    }

    /// The pointer is over (or left) the card.
    fn popup_hover(&mut self, on: bool) {
        self.card_hovered = on;
        match self.card.clone() {
            Card::Saved(_) | Card::Note => {
                self.expiry = None;
                if !on {
                    self.schedule_expiry(3.0);
                }
            }
            Card::Choose => {
                self.expiry = None;
                if !on {
                    self.schedule_expiry(30.0);
                }
            }
            Card::Sending => {
                if on {
                    self.expiry = None;
                } else if self.send_finished {
                    self.schedule_expiry(2.0);
                }
            }
            _ => {}
        }
    }

    /// Sharing on, hub not answering: start it with no window.
    fn watch_hub(&mut self) {
        if !self.enabled || self.fresh().is_some() {
            return;
        }
        if let Some(child) = self.hub.as_mut() {
            if matches!(child.try_wait(), Ok(None)) {
                return;
            }
            self.hub = None;
        }
        if self
            .hub_launched
            .is_some_and(|at| at.elapsed() < Duration::from_secs(30))
        {
            return;
        }
        self.hub_launched = Some(Instant::now());
        self.hub = spawn_hub();
    }

    // -- the news from the hub

    fn process(&mut self, new: &State) {
        if !self.primed {
            // What finished before the notch looked is not news.
            self.primed = true;
            for transfer in new.transfers.iter().filter(|t| !t.is_open()) {
                self.seen_finished.insert(transfer.id.clone());
            }
            self.last_notice = new.notice.as_ref().map_or(0, |n| n.0);
        }
        if self.card == Card::Choose {
            self.refresh_choose_card();
        }
        if self.card == Card::Sending {
            self.update_sending(new);
        }
        if let Card::Incoming(id) = &self.card
            && !new.incoming.iter().any(|i| &i.id == id)
        {
            self.clear_card();
        }
        if let Some(request) = new.incoming.first() {
            if self.card != Card::Incoming(request.id.clone()) {
                self.show_incoming(request, new.save_dir.as_deref());
            }
            return;
        }
        for transfer in new.transfers.iter().filter(|t| !t.is_open()) {
            if self.seen_finished.insert(transfer.id.clone()) {
                self.announce(transfer, new.save_dir.as_deref());
            }
        }
        if let Some((id, text)) = &new.notice
            && *id > self.last_notice
        {
            self.last_notice = *id;
            self.show_note("Couldn't send", text, true);
        }
        // A sharing service that cannot start (above all: the LocalSend app holds port
        // 53317) is said once, on a card, and stays on the hover card.
        let previous = self.last_error.clone();
        match (&new.error, &previous) {
            (Some(error), last) if last.as_ref() != Some(error) => {
                self.last_error = Some(error.clone());
                if self.enabled {
                    let (title, retry) = if new.error_kind.as_deref() == Some("port_in_use") {
                        (
                            "Nearby sharing can't start",
                            " Pulse tries again every 10 seconds.",
                        )
                    } else {
                        ("Sharing isn't running", "")
                    };
                    self.show_note(title, &format!("{error}{retry}"), true);
                    self.schedule_expiry(12.0);
                }
            }
            (None, Some(_)) => self.last_error = None,
            _ => {}
        }
    }

    fn show_incoming(&mut self, request: &Incoming, folder: Option<&str>) {
        let title;
        let mut detail;
        if request.is_message {
            title = format!("{} wants to send a message", request.from);
            detail = request.preview.clone().unwrap_or_default();
        } else {
            let total = bytes_text(request.total_bytes);
            title = if request.file_count == 1 {
                format!("{} wants to send 1 file ({total})", request.from)
            } else {
                format!(
                    "{} wants to send {} files ({total})",
                    request.from, request.file_count
                )
            };
            detail = request.first_file.clone().unwrap_or_default();
        }
        let place = folder_name(folder.unwrap_or("Downloads"));
        if !detail.is_empty() {
            detail.push_str(" · ");
        }
        detail.push_str(&format!("Saves to {place}"));
        self.expiry = None;
        let mut prompt = Prompt::plain(title, detail, false);
        if request.is_message {
            prompt.lead = Lead::Tile(Tile::Message);
        }
        prompt.buttons = vec![
            ("Accept".into(), Action::Accept),
            ("Decline".into(), Action::Decline),
        ];
        self.show(prompt, Card::Incoming(request.id.clone()));
    }

    fn announce(&mut self, transfer: &Transfer, folder: Option<&str>) {
        match (transfer.direction.as_str(), transfer.state.as_str()) {
            ("receive", "done") if transfer.message.is_some() => {
                let message = transfer.message.clone().unwrap_or_default();
                self.remember(Last {
                    text: Some(capped(&message)),
                    files: Vec::new(),
                    at: now_secs(),
                });
                let mut prompt = Prompt::plain(
                    format!("Message from {}", transfer.peer),
                    message.clone(),
                    false,
                );
                prompt.lead = Lead::Tile(Tile::Message);
                prompt.buttons = vec![("Copy".into(), Action::Copy)];
                if link_in(&message).is_some() {
                    prompt.buttons.push(("Open".into(), Action::OpenLink));
                }
                prompt.buttons.push(("Close".into(), Action::Close));
                self.show(prompt, Card::Message(message));
            }
            ("receive", "done") => {
                let files = transfer.saved_files.clone();
                if !files.is_empty() {
                    self.remember(Last {
                        text: None,
                        files: files.clone(),
                        at: now_secs(),
                    });
                }
                let place = folder_name(
                    transfer
                        .saved_to
                        .as_deref()
                        .or(folder)
                        .unwrap_or("Downloads"),
                );
                let detail = if files.len() == 1 {
                    folder_name(&files[0])
                } else {
                    format!("{} from {}", files_text(files.len()), transfer.peer)
                };
                let mut prompt = Prompt::plain(format!("Saved to {place}"), detail, false);
                if let Some(first) = files.first() {
                    prompt.lead = Lead::File(first.clone());
                }
                prompt.buttons = vec![
                    ("Show".into(), Action::Show),
                    ("Close".into(), Action::Close),
                ];
                self.show(prompt, Card::Saved(files));
                self.schedule_expiry(8.0);
            }
            ("send", "done") => {
                let count = transfer.files_total.max(1);
                self.show_note(
                    &format!("Sent to {}", transfer.peer),
                    &files_text(count),
                    false,
                );
            }
            (_, "declined") => {
                self.show_note(
                    &format!("{} declined", transfer.peer),
                    "Nothing was sent.",
                    true,
                );
            }
            (_, "failed") => {
                let title = if transfer.direction == "send" {
                    "Couldn't send"
                } else {
                    "Couldn't receive"
                };
                self.show_note(title, transfer.error.as_deref().unwrap_or(""), true);
            }
            _ => {}
        }
    }

    // -- sending

    fn send(&mut self, paths: Vec<PathBuf>, text: Option<String>) {
        let files: Vec<PathBuf> = paths.into_iter().filter(|p| p.exists()).collect();
        if files.is_empty() && text.as_deref().unwrap_or("").is_empty() {
            return;
        }
        if !self.fresh().is_some_and(|s| s.running) {
            let detail = self
                .state
                .as_ref()
                .and_then(|s| s.error.clone())
                .unwrap_or_else(|| "Nearby sharing is off or still starting.".to_string());
            self.show_note("Sharing isn't running", &detail, true);
            return;
        }
        match self.target() {
            Some(device) => self.deliver(files, text, &device),
            None => {
                // Several devices, or none yet: list them on a card (looking again when there
                // are none) and send when one is clicked, so nothing goes to a device by habit.
                self.pending = Some(Pending { paths: files, text });
                self.show_choose();
            }
        }
    }

    fn deliver(&mut self, paths: Vec<PathBuf>, text: Option<String>, device: &Device) {
        let list: Vec<String> = paths
            .iter()
            .map(|p| json_text(&p.to_string_lossy()))
            .collect();
        let mut body = format!(
            "{{\"command\":\"send\",\"to\":{},\"paths\":[{}]",
            json_text(&device.fingerprint),
            list.join(",")
        );
        if let Some(text) = text.as_deref().filter(|t| !t.is_empty()) {
            body.push_str(&format!(",\"text\":{}", json_text(text)));
        }
        body.push('}');
        write_command(&body);
        self.begin_sending(device, &paths, text.as_deref());
    }

    fn summary(paths: &[PathBuf]) -> String {
        match paths {
            [] => "Text".to_string(),
            [one] => one
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            many => files_text(many.len()),
        }
    }

    fn begin_sending(&mut self, device: &Device, paths: &[PathBuf], _text: Option<&str>) {
        // A request waiting for an answer keeps the card; the Send ring still shows this.
        if matches!(self.card, Card::Incoming(_)) {
            return;
        }
        self.send_peer = Some(device.clone());
        self.send_summary = Self::summary(paths);
        self.send_baseline = self
            .state
            .as_ref()
            .map(|s| s.transfers.iter().map(|t| t.id.clone()).collect())
            .unwrap_or_default();
        self.send_transfer = None;
        self.send_finished = false;
        self.expiry = None;
        let prompt = self.sending_prompt(None, device);
        self.show(prompt, Card::Sending);
        // The hub should list the transfer at once; if it never does, let go.
        self.schedule_expiry(20.0);
    }

    fn sending_prompt(&self, transfer: Option<&Transfer>, peer: &Device) -> Prompt {
        let summary = &self.send_summary;
        let device = device_symbol(peer);
        let make = |title: String,
                    detail: String,
                    stage: Stage,
                    fraction: Option<f32>,
                    cancel: bool,
                    symbol: Symbol| {
            Prompt {
                title,
                detail,
                problem: stage == Stage::Problem,
                buttons: Vec::new(),
                view: View::Transfer {
                    stage,
                    fraction,
                    can_cancel: cancel,
                    symbol,
                },
                lead: Lead::Tile(Tile::Folder),
            }
        };
        let waiting = format!("Waiting for {} to accept…", peer.alias);
        let Some(transfer) = transfer else {
            return make(
                waiting,
                summary.clone(),
                Stage::Waiting,
                None,
                false,
                device,
            );
        };
        match transfer.state.as_str() {
            "active" => {
                let mut detail = format!(
                    "{} of {}",
                    bytes_text(transfer.done_bytes),
                    bytes_text(transfer.total_bytes)
                );
                if transfer.files_total > 1 {
                    detail.push_str(&format!(
                        " · {} of {} files",
                        (transfer.files_done + 1).min(transfer.files_total),
                        transfer.files_total
                    ));
                } else if let Some(current) = transfer.current.as_deref().filter(|c| !c.is_empty())
                {
                    detail = format!("{current} · {detail}");
                }
                make(
                    format!("Sending to {}", peer.alias),
                    detail,
                    Stage::Active,
                    Some(transfer.fraction()),
                    true,
                    device,
                )
            }
            "done" => {
                let count = transfer.files_total.max(1);
                let what = if count == 1 {
                    summary.clone()
                } else {
                    format!("{count} files")
                };
                make(
                    "Sent".into(),
                    format!("To {} · {what}", peer.alias),
                    Stage::Done,
                    Some(1.0),
                    false,
                    Symbol::Check,
                )
            }
            "declined" => make(
                "Declined".into(),
                format!("{} declined. Nothing was sent.", peer.alias),
                Stage::Problem,
                None,
                false,
                Symbol::Hand,
            ),
            "failed" => make(
                "Couldn't send".into(),
                transfer.error.clone().unwrap_or_default(),
                Stage::Problem,
                None,
                false,
                Symbol::Warning,
            ),
            _ => make(waiting, summary.clone(), Stage::Waiting, None, true, device),
        }
    }

    /// Follows the transfer this card started, from "waiting" to its end.
    fn update_sending(&mut self, new: &State) {
        let Some(peer) = self.send_peer.clone() else {
            return;
        };
        if self.send_transfer.is_none() {
            let started = new
                .transfers
                .iter()
                .rev()
                .find(|t| t.direction == "send" && !self.send_baseline.contains(&t.id));
            let Some(started) = started else { return };
            self.send_transfer = Some(started.id.clone());
            self.expiry = None;
        }
        let Some(transfer) = new
            .transfers
            .iter()
            .find(|t| Some(&t.id) == self.send_transfer.as_ref())
        else {
            return;
        };
        match transfer.state.as_str() {
            "cancelled" => {
                self.seen_finished.insert(transfer.id.clone());
                self.clear_card();
            }
            "done" | "declined" | "failed" => {
                self.seen_finished.insert(transfer.id.clone());
                self.send_finished = true;
                self.prompt = Some(self.sending_prompt(Some(transfer), &peer));
                if !self.card_hovered {
                    self.schedule_expiry(if transfer.state == "done" { 2.0 } else { 6.0 });
                }
            }
            _ => self.prompt = Some(self.sending_prompt(Some(transfer), &peer)),
        }
    }

    // -- the device list card

    fn choose_prompt(&self) -> Prompt {
        let live = self.fresh();
        let list = live.map(|s| s.devices.clone()).unwrap_or_default();
        let rows = list
            .iter()
            .map(|d| {
                (
                    d.fingerprint.clone(),
                    d.alias.clone(),
                    d.device_model
                        .clone()
                        .unwrap_or_else(|| kind_of(d).to_string()),
                    device_symbol(d),
                )
            })
            .collect();
        let (title, detail) = if list.is_empty() {
            (
                "Looking for devices…",
                "Open LocalSend on the other device.".to_string(),
            )
        } else {
            (
                "Send to…",
                self.pending
                    .as_ref()
                    .map(|p| Self::summary(&p.paths))
                    .unwrap_or_default(),
            )
        };
        Prompt {
            title: title.to_string(),
            detail,
            problem: false,
            buttons: Vec::new(),
            view: View::List {
                rows,
                scanning: live.is_some_and(|s| s.scanning),
            },
            lead: Lead::Tile(Tile::Folder),
        }
    }

    fn show_choose(&mut self) {
        let prompt = self.choose_prompt();
        self.last_choose = Some(prompt.clone());
        self.show(prompt, Card::Choose);
        self.schedule_expiry(30.0);
        if self.fresh().is_none_or(|s| s.devices.is_empty()) {
            write_command("{\"command\":\"refresh\"}");
        }
    }

    /// Devices come and go, and a scan starts and ends, while the card is up.
    fn refresh_choose_card(&mut self) {
        let prompt = self.choose_prompt();
        if self.last_choose.as_ref() != Some(&prompt) {
            self.last_choose = Some(prompt.clone());
            self.prompt = Some(prompt);
        }
    }

    // -- clicks

    fn perform(&mut self, action: Action) {
        match action {
            Action::SendTo(fingerprint) => self.send_to(&fingerprint),
            Action::CopyLast => {} // handled by `copy_last`, outside the lock
            Action::Paste => {} // handled by `paste_clipboard`, which reads the clipboard first
            Action::Refresh => write_command("{\"command\":\"refresh\"}"),
            Action::Accept | Action::Decline => {
                if let Card::Incoming(id) = self.card.clone() {
                    let verb = if action == Action::Accept {
                        "accept"
                    } else {
                        "decline"
                    };
                    write_command(&format!(
                        "{{\"command\":\"{verb}\",\"id\":{}}}",
                        json_text(&id)
                    ));
                    self.clear_card();
                }
            }
            Action::Cancel => {
                if self.card == Card::Sending {
                    let open = self.send_transfer.clone().filter(|id| {
                        self.fresh()
                            .is_some_and(|s| s.transfers.iter().any(|t| &t.id == id && t.is_open()))
                    });
                    if let Some(id) = open {
                        write_command(&format!(
                            "{{\"command\":\"cancel\",\"id\":{}}}",
                            json_text(&id)
                        ));
                    }
                }
            }
            Action::Close => {
                if self.card == Card::Choose {
                    self.pending = None;
                }
                // The close only puts the card away: a transfer carries on and is announced
                // when it ends.
                self.clear_card();
            }
            Action::Show => {
                if let Card::Saved(files) = self.card.clone() {
                    reveal(&files);
                }
                self.clear_card();
            }
            Action::Copy => {
                if let Card::Message(message) = self.card.clone() {
                    sys::write_text(&message);
                }
                self.clear_card();
            }
            Action::OpenLink => {
                if let Card::Message(message) = self.card.clone()
                    && let Some(url) = link_in(&message)
                {
                    open_link(&url);
                }
                self.clear_card();
            }
            Action::Installer(_) => {}
        }
    }

    /// A click on a device: a paste or drop that was waiting for an answer goes to it at once.
    fn send_to(&mut self, fingerprint: &str) {
        let device = self
            .devices()
            .into_iter()
            .find(|d| d.fingerprint == fingerprint);
        if let (Some(waiting), Some(device)) = (self.pending.take(), device) {
            self.deliver(waiting.paths, waiting.text, &device);
        }
    }

    fn cancel_current(&mut self) {
        let open = self.fresh().and_then(|s| {
            s.transfers
                .iter()
                .rev()
                .find(|t| t.is_open())
                .map(|t| t.id.clone())
        });
        if let Some(id) = open {
            write_command(&format!(
                "{{\"command\":\"cancel\",\"id\":{}}}",
                json_text(&id)
            ));
        }
    }

    // -- the ring and the hover card

    fn ring(&self) -> Ring {
        let live = self.fresh();
        if let Some(transfer) = live.and_then(|s| s.transfers.iter().rev().find(|t| t.is_open())) {
            let fraction = transfer.fraction();
            return Ring {
                fraction: Some(fraction),
                active: true,
                problem: false,
                label: if transfer.state == "waiting" {
                    "Waiting".to_string()
                } else {
                    percent(fraction)
                },
            };
        }
        let (label, problem) = if !self.enabled {
            ("Off", false)
        } else if let Some(live) = live {
            ("Idle", live.error.is_some())
        } else {
            ("Starting…", false)
        };
        Ring {
            fraction: None,
            active: false,
            problem,
            label: label.to_string(),
        }
    }

    fn hover_panel(&self) -> Panel {
        let live = self.fresh();
        let devices = live.map(|s| s.devices.clone()).unwrap_or_default();
        let mut rows: Vec<Row> = Vec::new();
        let mut actions: Vec<Option<Action>> = Vec::new();
        let mut add = |row: Row, action: Option<Action>| {
            rows.push(row);
            actions.push(action);
        };
        if let Some(label) = self.last.as_ref().and_then(Last::row_label) {
            add(
                Row::Pair {
                    label,
                    value: String::new(),
                },
                Some(Action::CopyLast),
            );
        }
        if let Some(transfer) = live.and_then(|s| s.transfers.iter().rev().find(|t| t.is_open())) {
            let label = if transfer.direction == "send" {
                format!("Sending to {}", transfer.peer)
            } else {
                format!("Receiving from {}", transfer.peer)
            };
            let fraction = transfer.fraction();
            if transfer.state == "waiting" {
                add(
                    Row::Meter {
                        label,
                        trailing: String::new(),
                        fraction: None,
                        summary: if transfer.direction == "send" {
                            format!("Waiting for {} to accept", transfer.peer)
                        } else {
                            "Waiting".to_string()
                        },
                    },
                    None,
                );
            } else {
                let mut detail = format!(
                    "{} of {}",
                    bytes_text(transfer.done_bytes),
                    bytes_text(transfer.total_bytes)
                );
                if transfer.files_total > 1 {
                    detail.push_str(&format!(
                        " · {} of {} files",
                        (transfer.files_done + 1).min(transfer.files_total),
                        transfer.files_total
                    ));
                }
                add(
                    Row::Meter {
                        label,
                        trailing: String::new(),
                        fraction: Some(fraction),
                        summary: detail,
                    },
                    None,
                );
            }
            add(
                Row::Pair {
                    label: "Cancel".into(),
                    value: String::new(),
                },
                Some(Action::Cancel),
            );
        } else {
            let line = if !self.enabled {
                "Off".to_string()
            } else if let Some(live) = live {
                if let Some(error) = &live.error {
                    error.clone()
                } else if self.drop_targeting && self.target().is_some() {
                    format!(
                        "Release to send to {}",
                        self.target().map(|d| d.alias).unwrap_or_default()
                    )
                } else if devices.is_empty() {
                    if live.scanning {
                        "Looking for devices…".to_string()
                    } else {
                        "No devices nearby".to_string()
                    }
                } else {
                    format!("{} nearby", devices.len())
                }
            } else {
                "Starting…".to_string()
            };
            add(
                Row::Pair {
                    label: "Nearby sharing".into(),
                    value: line,
                },
                None,
            );
        }
        if let Some(live) = live {
            for warning in &live.warnings {
                add(Row::Text(warning.clone()), None);
            }
        }
        for device in &devices {
            add(
                Row::Pair {
                    label: device.alias.clone(),
                    value: kind_of(device).to_string(),
                },
                Some(Action::SendTo(device.fingerprint.clone())),
            );
        }
        let blocked = live.is_some_and(|s| s.local_network.as_deref() == Some("blocked"));
        if blocked {
            add(Row::Text(FIREWALL_HINT.into()), None);
        }
        if live.is_some_and(|s| s.running) {
            if !devices.is_empty() {
                add(
                    Row::Pair {
                        label: "Paste clipboard".into(),
                        value: String::new(),
                    },
                    Some(Action::Paste),
                );
                add(
                    Row::Text("Ctrl+V sends the clipboard · drop files here".into()),
                    None,
                );
            }
            let scanning = live.is_some_and(|s| s.scanning);
            let again = if scanning { "Looking…" } else { "Look again" };
            add(
                Row::Pair {
                    label: again.to_string(),
                    value: String::new(),
                },
                (!scanning).then_some(Action::Refresh),
            );
        }
        Panel {
            content: CardContent {
                title: "Send".into(),
                accessory: None,
                rows,
                mark: Mark::Send,
                ..CardContent::default()
            },
            actions: actions.into_iter().map(|action| vec![action]).collect(),
            head: Vec::new(),
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            ring: self.ring(),
            hover: self.hover_panel(),
            popup: self.prompt.as_ref().map(Prompt::panel),
        }
    }
}

// ---- files and processes --------------------------------------------------------------------

fn bridge_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|p| p.join("Pulse"))
}

/// A JSON string literal.
fn json_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

static COMMAND_SEQ: AtomicU64 = AtomicU64::new(0);

/// Drops one command file for the hub (write, then rename, so it never reads half a file) and
/// wakes it.
fn write_command(body: &str) {
    let Some(dir) = bridge_dir().map(|d| d.join("share-commands")) else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    let stamp = format!(
        "{micros:020}-{:08}-{:04}",
        std::process::id(),
        COMMAND_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let temp = dir.join(format!("{stamp}.tmp"));
    let done = dir.join(format!("{stamp}.json"));
    if std::fs::write(&temp, body).is_ok() && std::fs::rename(&temp, &done).is_ok() {
        sys::signal(COMMAND_EVENT);
    } else {
        let _ = std::fs::remove_file(&temp);
    }
}

fn locate_hub() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let mut candidates = vec![
        dir.join(HUB_EXE),
        dir.join("hub").join(HUB_EXE),
        dir.join("Helpers").join(HUB_EXE),
    ];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(
            PathBuf::from(local)
                .join("Programs")
                .join("Pulse")
                .join(HUB_EXE),
        );
    }
    candidates.into_iter().find(|path| path.is_file())
}

/// Starts the hub with no window. A hub that is already running exits at once (its own
/// single-instance guard), so this is safe when `hub.rs` started one meanwhile.
fn spawn_hub() -> Option<Child> {
    let Some(path) = locate_hub() else {
        diag::info("share_hub_missing", &[("action", "no_sharing")]);
        return None;
    };
    match Command::new(&path)
        .arg("--background")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        Ok(child) => Some(child),
        Err(error) => {
            diag::info(
                "share_hub_spawn_failed",
                &[("reason", error.to_string().as_str())],
            );
            None
        }
    }
}

/// Selects a saved file in Explorer (or opens the folder when several were saved).
fn reveal(files: &[String]) {
    let existing: Vec<&String> = files
        .iter()
        .filter(|f| Path::new(f.as_str()).exists() && !f.contains('"'))
        .collect();
    let mut command = Command::new("explorer.exe");
    match existing.as_slice() {
        [] => return,
        [one] => {
            command.raw_arg(format!("/select,\"{one}\""));
        }
        [first, ..] => {
            let Some(parent) = Path::new(first.as_str()).parent() else {
                return;
            };
            command.arg(parent);
        }
    }
    let _ = command.creation_flags(CREATE_NO_WINDOW).spawn();
}

/// Opens a web address in the default browser (Explorer hands it to the registered handler).
/// `url` came through `link_in`: http(s) only, no spaces or quotes.
fn open_link(url: &str) {
    let _ = Command::new("explorer.exe")
        .arg(url)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

// ---- the public face ------------------------------------------------------------------------

static MODEL: LazyLock<Mutex<Model>> = LazyLock::new(|| Mutex::new(Model::new()));
static STOP: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);
static WINDOW: AtomicIsize = AtomicIsize::new(0);
static MESSAGE: AtomicU32 = AtomicU32::new(0);
static HOTKEY_WINDOW: AtomicIsize = AtomicIsize::new(0);

fn model() -> MutexGuard<'static, Model> {
    MODEL.lock().unwrap_or_else(PoisonError::into_inner)
}

fn notify() {
    sys::post_message(
        WINDOW.load(Ordering::Relaxed),
        MESSAGE.load(Ordering::Relaxed),
    );
}

/// Starts nearby sharing's watcher thread. `window` is the key of a window of the UI thread
/// (`raii::hwnd_key`, the controller window works) that receives `message` (a `WM_APP + n`)
/// whenever the ring, the hover card or a notch card changed, and `WM_HOTKEY` for the paste
/// key. On `message` the caller re-reads `ring()`, `hover_panel()` and `popup_panel()` and
/// redraws. Calling it twice does nothing.
pub fn start(window: isize, message: u32) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    STOP.store(false, Ordering::SeqCst);
    WINDOW.store(window, Ordering::SeqCst);
    MESSAGE.store(message, Ordering::SeqCst);
    {
        let mut model = model();
        model.last = load_last();
        model.read_state(true);
        model.tick();
    }
    let spawned = std::thread::Builder::new()
        .name("pulse-share".into())
        .spawn(watcher);
    if let Err(error) = spawned {
        diag::info(
            "share_thread_failed",
            &[("reason", error.to_string().as_str())],
        );
    }
    notify();
}

fn watcher() {
    let event = sys::open_event(STATE_EVENT);
    let mut last: Option<Snapshot> = None;
    let mut last_read = Instant::now();
    while !STOP.load(Ordering::Relaxed) {
        let signalled = sys::wait(event, 250);
        let snapshot = {
            let mut model = model();
            if signalled {
                model.read_state(true);
                last_read = Instant::now();
            } else if last_read.elapsed() >= Duration::from_secs(1) {
                model.read_state(false);
                last_read = Instant::now();
            }
            model.tick();
            model.snapshot()
        };
        if last.as_ref() != Some(&snapshot) {
            last = Some(snapshot);
            notify();
        }
    }
    sys::close(event);
}

/// Stops the watcher, releases the paste key and ends a hub this module started. Call when the
/// notch quits (UI thread).
pub fn stop() {
    STOP.store(true, Ordering::SeqCst);
    STARTED.store(false, Ordering::SeqCst);
    release_hotkey();
    let mut model = model();
    if let Some(mut hub) = model.hub.take() {
        let _ = hub.kill();
        let _ = hub.wait();
    }
}

/// Sharing is on in the notch's settings (`nearby_enabled`, default true). Off stops the notch
/// from starting a hub for it; the hub itself stops sharing when it re-reads the same setting.
pub fn set_enabled(on: bool) {
    model().enabled = on;
    notify();
}

/// The Send ring.
pub fn ring() -> Ring {
    model().ring()
}

/// The hover card of the Send cell: transfer or status line, nearby devices (click sends a
/// waiting paste or drop to it), Paste, Look again.
pub fn hover_panel() -> Panel {
    model().hover_panel()
}

/// The card that hangs from the Send ring while there is news: an incoming request (Accept,
/// Decline), the device list a paste or drop waits on, the transfer in flight, "Saved to
/// Downloads", a received message (Copy, Open), or an error such as the LocalSend app holding
/// the port. `None` when no card is up. It takes precedence over the hover card.
pub fn popup_panel() -> Option<Panel> {
    let sharing = model().prompt.as_ref().map(Prompt::panel);
    sharing.or_else(crate::installer::popup_panel)
}

/// The pointer is over or left the Send cell or its hover card. While it is over, Ctrl+V is
/// taken as "send the clipboard" (`WM_HOTKEY` with `wparam == HOTKEY_ID`); never otherwise.
/// UI thread only.
pub fn set_hover(over_send: bool) {
    let window = WINDOW.load(Ordering::Relaxed);
    {
        let mut model = model();
        model.hovering = over_send;
    }
    if over_send {
        if HOTKEY_WINDOW.load(Ordering::Relaxed) == 0
            && window != 0
            && sys::register_paste_key(window, HOTKEY_ID)
        {
            HOTKEY_WINDOW.store(window, Ordering::Relaxed);
        }
    } else {
        release_hotkey();
    }
}

fn release_hotkey() {
    let window = HOTKEY_WINDOW.swap(0, Ordering::Relaxed);
    if window != 0 {
        sys::unregister_paste_key(window, HOTKEY_ID);
    }
}

/// The pointer is over or left the notch card (`popup_panel`): hovering keeps it up.
pub fn popup_hover(on: bool) {
    model().popup_hover(on);
    crate::installer::popup_hover(on);
    notify();
}

/// A file is being dragged over the Send cell (only if the caller tracks it; `WM_DROPFILES`
/// itself gives no enter/leave). Changes the hover line to "Release to send to <device>".
pub fn set_drop_targeting(on: bool) {
    let changed = {
        let mut model = model();
        let changed = model.drop_targeting != on;
        model.drop_targeting = on;
        changed
    };
    if changed {
        notify();
    }
}

/// A click on a row of `hover_panel()` or `popup_panel()` (its `Action`).
pub fn perform(action: Action) {
    if let Action::Installer(choice) = action {
        crate::installer::perform(choice);
        return;
    }
    if action == Action::Paste {
        paste_clipboard();
        return;
    }
    if action == Action::CopyLast {
        copy_last();
        return;
    }
    model().perform(action);
    notify();
}

/// Ctrl+V (the hot key) or the "Paste clipboard" row: the clipboard's files, a picture, or
/// text goes to the only device nearby, or to the one picked on the card that appears.
pub fn paste_clipboard() {
    match sys::read_clipboard() {
        sys::Clip::Files(paths) => model().send(paths, None),
        sys::Clip::Image(path) => model().send(vec![path], None),
        sys::Clip::Text(text) => model().send(Vec::new(), Some(text)),
        sys::Clip::Empty => model().show_note(
            "Nothing to send",
            "The clipboard has no files or text.",
            true,
        ),
    }
    notify();
}

/// The "Copy last" row: the last message, or the last saved files, back on the clipboard.
pub fn copy_last() {
    let last = model().last.clone();
    if let Some(last) = last {
        match last.text {
            Some(text) => {
                sys::write_text(&text);
            }
            None => {
                let files: Vec<PathBuf> = last
                    .files
                    .iter()
                    .map(PathBuf::from)
                    .filter(|p| p.exists())
                    .collect();
                if !files.is_empty() {
                    sys::write_files(&files);
                }
            }
        }
    }
    notify();
}

/// Files dropped on the Send cell (after `take_drop`). False when the list is empty.
pub fn drop_files(paths: Vec<PathBuf>) -> bool {
    if paths.is_empty() {
        return false;
    }
    {
        let mut model = model();
        model.drop_targeting = false;
        model.send(paths, None);
    }
    notify();
    true
}

/// Lets a window receive dropped files (`WM_DROPFILES`): call for each notch panel window when
/// it is created. The window must not be elevated above Pulse.
pub fn accept_drops(window: isize) {
    sys::accept_drops(window);
}

/// What `take_drop` returns: the files and where they were dropped, in the window's client
/// pixels (hit-test it against the Send cell).
pub struct Dropped {
    pub paths: Vec<PathBuf>,
    pub x: i32,
    pub y: i32,
}

/// Reads and releases the `WM_DROPFILES` payload (`wparam`). Call exactly once per message.
pub fn take_drop(hdrop: isize) -> Dropped {
    let (paths, (x, y)) = sys::take_drop(hdrop);
    Dropped { paths, x, y }
}

/// Stops the transfer in flight, if any (Esc-style shortcut for callers that want one).
pub fn cancel_current() {
    model().cancel_current();
}
