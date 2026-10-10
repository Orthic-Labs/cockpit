//! The Claude account book: the last usage reading of every Claude account this PC has seen,
//! kept by account id, and the list the hub's Accounts section shows (the Windows counterpart
//! of the Mac's `ClaudeAccountBook`). Only the signed-in account can be read, so the others
//! are shown from here: their last percentages and when each window resets. Nothing is
//! invented for them. Every account folder Claude Desktop has (an account id directory under
//! `claude-code-sessions` or `local-agent-mode-sessions`) is listed too, signed in or not, so
//! it can be named before its first reading. Holds no secrets: ids, a sign-in address Claude
//! Code already wrote in its own config, a plan name and window numbers. The notch is the
//! only writer; the hub renames and forgets through commands. Stored at
//! `%LOCALAPPDATA%\Pulse\claude-account-usage.json` (the Mac's schema, epoch-second dates).

use crate::desktop;
use crate::json::{self, Value};
use crate::usage::{self, LimitWindow};
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

const FILE: &str = "claude-account-usage.json";
const FILE_MAX_BYTES: u64 = 1024 * 1024;
const COMMAND_MAX_BYTES: usize = 16 * 1024;
const NAME_MAX_CHARS: usize = 60;
/// Unchanged numbers are not rewritten more than once a minute.
const REWRITE_SECONDS: u64 = 60;
const SESSION_SECONDS: u64 = 5 * 3600;
const WEEK_SECONDS: u64 = 7 * 24 * 3600;

#[derive(Clone, Debug, PartialEq)]
struct Window {
    label: String,
    used: f64,
    resets_at: u64,
    seconds: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    id: String,
    /// Order the account was first seen in.
    ordinal: u32,
    /// Chosen in the hub; `None` means the default name.
    custom_name: Option<String>,
    email: Option<String>,
    plan: Option<String>,
    windows: Vec<Window>,
    /// Epoch 0 with no windows: named before any reading.
    captured_at: u64,
}

impl Entry {
    fn has_reading(&self) -> bool {
        !self.windows.is_empty()
    }

    fn stub(id: &str, email: Option<&str>) -> Self {
        Self {
            id: id.to_string(),
            ordinal: 0,
            custom_name: None,
            email: email.map(str::to_string),
            plan: None,
            windows: Vec::new(),
            captured_at: 0,
        }
    }

    fn name(&self) -> String {
        match (&self.custom_name, &self.email) {
            (Some(name), _) if !name.is_empty() => name.clone(),
            (_, Some(email)) if !email.is_empty() => email.clone(),
            _ => default_name(&self.id),
        }
    }
}

struct Book {
    loaded: bool,
    entries: Vec<Entry>,
}

static BOOK: Mutex<Book> = Mutex::new(Book {
    loaded: false,
    entries: Vec::new(),
});

/// An unnamed account reads as the first 8 characters of its id: Claude Desktop keeps no
/// non-secret address or name.
fn default_name(id: &str) -> String {
    format!("Claude {}", id.get(..8).unwrap_or(id))
}

fn file_path() -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    base.is_absolute().then(|| base.join("Pulse").join(FILE))
}

fn with_book<R>(action: impl FnOnce(&mut Book) -> R) -> R {
    let mut book = BOOK.lock().unwrap_or_else(PoisonError::into_inner);
    if !book.loaded {
        book.loaded = true;
        book.entries = load().unwrap_or_default();
    }
    action(&mut book)
}

// ------------------------------------------------------------------ file

fn number(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64)
}

fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|text| !text.is_empty())
}

fn load() -> Option<Vec<Entry>> {
    let bytes = desktop::read_bounded(&file_path()?, FILE_MAX_BYTES)?;
    let root = json::parse(&bytes, FILE_MAX_BYTES as usize)?;
    let mut entries = Vec::new();
    for item in root.get("accounts")?.as_array()? {
        let Some(id) = text(item.get("id")) else {
            continue;
        };
        if !desktop::is_account_id(&id) {
            continue;
        }
        let windows = item
            .get("windows")
            .and_then(Value::as_array)
            .unwrap_or(&[])
            .iter()
            .filter_map(|window| {
                Some(Window {
                    label: text(window.get("label"))?,
                    used: number(window.get("usedFraction"))?,
                    resets_at: number(window.get("resetsAt"))? as u64,
                    seconds: number(window.get("seconds")).map(|seconds| seconds as u64),
                })
            })
            .collect();
        entries.push(Entry {
            id,
            ordinal: number(item.get("ordinal")).map_or(0, |n| n as u32),
            custom_name: text(item.get("customName")),
            email: text(item.get("email")),
            plan: text(item.get("plan")),
            windows,
            captured_at: number(item.get("capturedAt")).map_or(0, |n| n as u64),
        });
    }
    Some(entries)
}

/// JSON string literal (quotes included) with the escapes the format needs.
fn esc(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
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
}

fn opt_text(out: &mut String, key: &str, value: &Option<String>) {
    if let Some(value) = value {
        out.push_str(&format!(",\"{key}\":"));
        esc(out, value);
    }
}

fn window_json(out: &mut String, window: &Window) {
    out.push_str("{\"label\":");
    esc(out, &window.label);
    let used = if window.used.is_finite() {
        window.used
    } else {
        0.0
    };
    out.push_str(&format!(
        ",\"usedFraction\":{used},\"resetsAt\":{}",
        window.resets_at
    ));
    if let Some(seconds) = window.seconds {
        out.push_str(&format!(",\"seconds\":{seconds}"));
    }
    out.push('}');
}

fn windows_json(out: &mut String, windows: &[Window]) {
    out.push('[');
    for (index, window) in windows.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        window_json(out, window);
    }
    out.push(']');
}

/// Writes the saved entries (temp file, then rename over). Best effort: the book is a cache.
fn save(entries: &[Entry]) {
    let Some(path) = file_path() else {
        return;
    };
    let mut out = String::from("{\"accounts\":[");
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("{\"capturedAt\":");
        out.push_str(&entry.captured_at.to_string());
        opt_text(&mut out, "customName", &entry.custom_name);
        opt_text(&mut out, "email", &entry.email);
        out.push_str(",\"id\":");
        esc(&mut out, &entry.id);
        out.push_str(&format!(",\"ordinal\":{}", entry.ordinal));
        opt_text(&mut out, "plan", &entry.plan);
        out.push_str(",\"windows\":");
        windows_json(&mut out, &entry.windows);
        out.push('}');
    }
    out.push_str("],\"schema\":1}\n");
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let temp = path.with_extension("json.tmp");
    if fs::write(&temp, out.as_bytes()).is_ok() && fs::rename(&temp, &path).is_err() {
        let _ = fs::remove_file(&temp);
    }
}

// ------------------------------------------------------------------ changes

fn next_ordinal(entries: &[Entry]) -> u32 {
    entries.iter().map(|entry| entry.ordinal).max().unwrap_or(0) + 1
}

/// What the card calls an account: the chosen name, else the sign-in address when there is
/// one, else `Claude <first 8 of the id>`. `email` is Claude Code's address and applies only
/// to Claude Code's own account.
pub fn label(id: &str, email: Option<&str>) -> String {
    let saved = with_book(|book| book.entries.iter().find(|e| e.id == id).map(Entry::name));
    saved
        .or_else(|| email.filter(|e| !e.is_empty()).map(str::to_string))
        .unwrap_or_else(|| default_name(id))
}

/// Saves the account's fresh reading. Windows without a reset time are left out: with none
/// there is no way to say when a saved percentage stops being true.
pub fn record(
    id: &str,
    email: Option<&str>,
    plan: Option<&str>,
    windows: &[LimitWindow],
    captured_at: u64,
) {
    let kept: Vec<Window> = windows
        .iter()
        .filter(|window| window.group.is_none())
        .filter_map(|window| {
            Some(Window {
                label: window.label.clone(),
                used: f64::from(window.fraction),
                resets_at: window.resets_at?,
                // The hub tells Session from Weekly by length; the reader knows it by key.
                seconds: match window.key.as_str() {
                    "session" => Some(SESSION_SECONDS),
                    key if key.starts_with("weekly") => Some(WEEK_SECONDS),
                    _ => None,
                },
            })
        })
        .collect();
    if kept.is_empty() {
        return;
    }
    let address = email.filter(|e| !e.is_empty()).map(str::to_string);
    let plan = plan.map(str::to_string);
    with_book(|book| {
        if let Some(entry) = book.entries.iter_mut().find(|e| e.id == id) {
            let new_email = address.or_else(|| entry.email.clone());
            let new_plan = plan.or_else(|| entry.plan.clone());
            if entry.windows == kept
                && entry.plan == new_plan
                && entry.email == new_email
                && captured_at.saturating_sub(entry.captured_at) < REWRITE_SECONDS
            {
                return;
            }
            entry.windows = kept;
            entry.plan = new_plan;
            entry.email = new_email;
            entry.captured_at = captured_at;
        } else {
            let ordinal = next_ordinal(&book.entries);
            book.entries.push(Entry {
                id: id.to_string(),
                ordinal,
                custom_name: None,
                email: address,
                plan,
                windows: kept,
                captured_at,
            });
        }
        save(&book.entries);
    });
}

/// What the book holds for an account, shaped for the ring: the windows still in force, the
/// plan and when the reading was taken.
pub struct Saved {
    pub plan: Option<String>,
    pub windows: Vec<LimitWindow>,
    pub captured_at: u64,
}

/// The account's last saved reading, restored when the notch starts so the ring is not empty
/// until the first fetch. A window whose reset time has passed is dropped (its percentage is
/// no longer true); `None` when nothing is left.
pub fn last_reading(id: &str, now: u64) -> Option<Saved> {
    with_book(|book| {
        let entry = book.entries.iter().find(|e| e.id == id)?;
        let windows: Vec<LimitWindow> = entry
            .windows
            .iter()
            .filter(|w| w.resets_at > now)
            .map(|w| {
                let key = match w.seconds {
                    Some(SESSION_SECONDS) => "session".to_string(),
                    Some(WEEK_SECONDS) if w.label == "All models" => "weekly_all".to_string(),
                    Some(WEEK_SECONDS) => {
                        format!("weekly_{}", w.label.to_lowercase().replace(' ', "_"))
                    }
                    _ => w.label.to_lowercase().replace(' ', "_"),
                };
                LimitWindow {
                    key,
                    group: None,
                    label: w.label.clone(),
                    fraction: w.used.clamp(0.0, 1.0) as f32,
                    resets_at: Some(w.resets_at),
                }
            })
            .collect();
        (!windows.is_empty()).then(|| Saved {
            plan: entry.plan.clone(),
            windows,
            captured_at: entry.captured_at,
        })
    })
}

/// "Forget reading": drops what was read for the account. A named account keeps its name;
/// an unnamed one leaves the book (an account folder on disk lists again as unread).
pub fn clear_reading(id: &str) {
    with_book(|book| {
        let Some(index) = book.entries.iter().position(|e| e.id == id) else {
            return;
        };
        if book.entries[index].custom_name.is_some() {
            let entry = &mut book.entries[index];
            entry.windows.clear();
            entry.plan = None;
            entry.captured_at = 0;
        } else {
            book.entries.remove(index);
        }
        save(&book.entries);
    });
}

/// An empty name returns the account to its default name. An account with no entry yet (a
/// folder on disk, never read) gets one so the name sticks; an id that is neither saved nor
/// on disk is ignored.
fn rename(id: &str, proposed: &str) {
    let name: String = proposed.trim().chars().take(NAME_MAX_CHARS).collect();
    let value = (!name.is_empty()).then_some(name);
    let on_disk = desktop::account_folders()
        .iter()
        .any(|(folder, _)| folder == id);
    with_book(|book| {
        let Some(index) = book.entries.iter().position(|e| e.id == id) else {
            let Some(value) = value else {
                return;
            };
            if !on_disk {
                return;
            }
            let ordinal = next_ordinal(&book.entries);
            let mut entry = Entry::stub(id, None);
            entry.ordinal = ordinal;
            entry.custom_name = Some(value);
            book.entries.push(entry);
            save(&book.entries);
            return;
        };
        if book.entries[index].custom_name == value {
            return;
        }
        book.entries[index].custom_name = value;
        // A named stub renamed back to the default has nothing left to keep.
        if book.entries[index].custom_name.is_none()
            && !book.entries[index].has_reading()
            && on_disk
        {
            book.entries.remove(index);
        }
        save(&book.entries);
    });
}

/// Drops an old account from the list. The signed-in account stays, and so does any account
/// whose folder is still on disk: it would reappear at once, and forgetting it would only
/// lose its name.
fn forget(id: &str, keeping_active: Option<&str>) {
    let on_disk = desktop::account_folders()
        .iter()
        .any(|(folder, _)| folder == id);
    if keeping_active == Some(id) || on_disk {
        return;
    }
    with_book(|book| {
        let before = book.entries.len();
        book.entries.retain(|e| e.id != id);
        if book.entries.len() != before {
            save(&book.entries);
        }
    });
}

/// Applies a hub command file when it is one of the account commands (`renameClaudeAccount`,
/// `forgetClaudeAccount`); false when it is some other command for the caller to handle.
pub fn apply_command(bytes: &[u8]) -> bool {
    let Some(root) = json::parse(bytes, COMMAND_MAX_BYTES) else {
        return false;
    };
    let name = root.get("command").and_then(Value::as_str);
    if name != Some("renameClaudeAccount") && name != Some("forgetClaudeAccount") {
        return false;
    }
    let Some(id) = root.get("id").and_then(Value::as_str) else {
        return true;
    };
    let id = id.trim().to_ascii_lowercase();
    if !desktop::is_account_id(&id) {
        return true;
    }
    if name == Some("renameClaudeAccount") {
        let proposed = root.get("name").and_then(Value::as_str).unwrap_or("");
        rename(&id, proposed);
    } else {
        forget(&id, usage::claude_ids().active.as_deref());
    }
    true
}

// ------------------------------------------------------------------ for the hub

/// Every account for `notch-state.json`: saved ones plus every account folder on disk, the
/// one tracked first, then by last reading, then by folder time. Dates are epoch seconds;
/// the hub decides "resets in" and "Reset" with its own clock. An account with no saved
/// reading has no `windows` and a null `capturedAt`; `onDisk` is false only for an account
/// whose folder is gone (the only kind the hub may forget).
pub fn published_json() -> String {
    let ids = usage::claude_ids();
    let folders = desktop::account_folders();
    let code_email = ids.email.as_deref().filter(|e| !e.is_empty());
    let mut all = with_book(|book| book.entries.clone());
    for (id, _) in &folders {
        if !all.iter().any(|entry| entry.id == *id) {
            all.push(Entry::stub(id, None));
        }
    }
    if let Some(active) = ids.active.as_deref()
        && !all.iter().any(|entry| entry.id == active)
    {
        all.push(Entry::stub(active, None));
    }
    // An unread account shows Claude Code's address when it is that account.
    for entry in &mut all {
        if entry.email.is_none() && ids.code.as_deref() == Some(entry.id.as_str()) {
            entry.email = code_email.map(str::to_string);
        }
    }
    let folder_time = |id: &str| {
        folders
            .iter()
            .find(|(folder, _)| folder == id)
            .map_or(0, |(_, modified)| *modified)
    };
    let rank = |entry: &Entry| {
        (
            ids.active.as_deref() == Some(entry.id.as_str()),
            if entry.has_reading() {
                entry.captured_at
            } else {
                0
            },
            folder_time(&entry.id),
        )
    };
    all.sort_by(|a, b| {
        let (x, y) = (rank(a), rank(b));
        y.0.cmp(&x.0)
            .then(y.1.cmp(&x.1))
            .then(y.2.cmp(&x.2))
            .then(a.id.cmp(&b.id))
    });
    let mut out = String::from("[");
    for (index, entry) in all.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("{\"id\":");
        esc(&mut out, &entry.id);
        out.push_str(",\"name\":");
        esc(&mut out, &entry.name());
        out.push_str(&format!(
            ",\"active\":{},\"onDisk\":{},\"capturedAt\":",
            ids.active.as_deref() == Some(entry.id.as_str()),
            folders.iter().any(|(folder, _)| *folder == entry.id),
        ));
        if entry.has_reading() {
            out.push_str(&entry.captured_at.to_string());
        } else {
            out.push_str("null");
        }
        out.push_str(",\"windows\":");
        windows_json(&mut out, &entry.windows);
        opt_text(&mut out, "email", &entry.email);
        opt_text(&mut out, "plan", &entry.plan);
        out.push('}');
    }
    out.push(']');
    out
}
