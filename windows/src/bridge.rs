//! Notch to hub settings bridge (the Windows counterpart of the Mac `HubBridge`).
//!
//! The notch stays the only writer of its preferences. It publishes a JSON snapshot to
//! `%LOCALAPPDATA%\Pulse\notch-state.json` (write a temp file, then rename over it) and signals
//! the named event `Local\dev.orthic.pulse.notch.state`, which the hub waits on and turns into
//! its "notch-state" page event. The hub asks for changes by dropping JSON files into
//! `hub-commands\` and signalling `Local\dev.orthic.pulse.hub.command`; the worker thread here
//! waits on that event (and re-checks every two seconds), applies each file in name order,
//! deletes it, and publishes again.
//!
//! Commands with a Windows meaning: `set` (see `apply_set`), `connect`, `order`,
//! `resetPosition`, `refresh` (repaint), `checkUpdates` and `quit`. Mac-only commands are
//! ignored on purpose: `signIn`, `signOut`, `allowAccess` (Keychain sign-in), the three
//! `preview*Alert` and `sendTestNotification` previews, `openAccessibilitySettings`,
//! `permissionRequest`, `helperEnable`, `helperDisable`, `openLoginItems` (macOS permissions and
//! the privileged helper), `installUpdate` (the update card installs) and `driveAlert` (the
//! Windows drive health card reads the disks itself).

use crate::settings::{self, Arg, Command, PillSettings};
use crate::{autostart, diag, installer, raii, send, shot, update, usage};
use std::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

type Handle = *mut c_void;

const POLL_MS: u32 = 2000;
const STATE_EVENT: &str = "Local\\dev.orthic.pulse.notch.state";
const COMMAND_EVENT: &str = "Local\\dev.orthic.pulse.hub.command";
const STATE_FILE: &str = "notch-state.json";
const COMMANDS_DIR: &str = "hub-commands";
/// Command files handled per pass; the rest wait for the next one.
const MAX_COMMANDS: usize = 64;
const PROVIDERS: [(&str, &str); 2] = [("claude", "Claude"), ("codex", "Codex")];

#[allow(non_snake_case, clashing_extern_declarations)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateEventW(
        attributes: *const c_void,
        manual_reset: i32,
        initial_state: i32,
        name: *const u16,
    ) -> Handle;
    fn SetEvent(event: Handle) -> i32;
    fn CloseHandle(object: Handle) -> i32;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
}

/// What the bridge needs from the notch's main module (registered once, at start).
pub struct Hooks {
    /// The live settings and whether the settings file may be written.
    pub settings: fn() -> (PillSettings, bool),
    /// Replaces the live settings and saves them.
    pub commit: fn(PillSettings),
    /// "Check now" in the hub's General page.
    pub check_updates: fn(),
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Opens (or creates) an auto-reset event; both sides create the same name, whoever is first.
fn open_event(name: &str) -> usize {
    let name = wide(name);
    // SAFETY: NUL-terminated name; auto-reset, initially not signaled.
    unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) as usize }
}

/// Signals the hub's state event. A hub that is not running holds no handle, so the event is
/// destroyed again when this one closes.
fn signal_state() {
    let event = open_event(STATE_EVENT);
    if event != 0 {
        // SAFETY: a live event handle, closed right after.
        unsafe {
            SetEvent(event as Handle);
            CloseHandle(event as Handle);
        }
    }
}

fn pulse_dir() -> Option<PathBuf> {
    settings::settings_paths().ok().map(|paths| paths.dir)
}

/// Starts the worker thread. Failure to start only costs the hub its settings pages.
pub fn start(controller_key: isize, hooks: Hooks) {
    let spawned = std::thread::Builder::new()
        .name("pulse-bridge".into())
        .spawn(move || worker(controller_key, &hooks));
    if let Err(error) = spawned {
        diag::info(
            "bridge_thread_failed",
            &[("reason", error.to_string().as_str())],
        );
    }
}

fn worker(controller_key: isize, hooks: &Hooks) {
    let Some(dir) = pulse_dir() else {
        diag::info("bridge_unavailable", &[("reason", "no_base_directory")]);
        return;
    };
    let commands = dir.join(COMMANDS_DIR);
    let _ = fs::create_dir_all(&commands);
    let event = open_event(COMMAND_EVENT);
    let mut last = String::new();
    loop {
        let applied = drain(&commands, controller_key, hooks);
        publish(&dir, hooks, &mut last, applied);
        // SAFETY: a live event handle owned by this thread for the process lifetime; a zero
        // handle just makes the wait fail at once, so sleep instead to keep the polling pace.
        if event == 0 {
            std::thread::sleep(std::time::Duration::from_millis(u64::from(POLL_MS)));
        } else {
            let _ = unsafe { WaitForSingleObject(event as Handle, POLL_MS) };
        }
    }
}

// ------------------------------------------------------------------ state out

/// Writes the snapshot when it changed (or `force`), atomically, then signals the hub.
fn publish(dir: &Path, hooks: &Hooks, last: &mut String, force: bool) {
    let json = state_json(hooks);
    if !force && json == *last {
        return;
    }
    let temp = dir.join(format!("{STATE_FILE}.tmp"));
    let written =
        fs::write(&temp, json.as_bytes()).and_then(|()| fs::rename(&temp, dir.join(STATE_FILE)));
    match written {
        Ok(()) => {
            *last = json;
            signal_state();
        }
        Err(error) => {
            let _ = fs::remove_file(&temp);
            diag::info(
                "bridge_state_write_failed",
                &[("reason", error.to_string().as_str())],
            );
        }
    }
}

fn esc(out: &mut String, text: &str) {
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
}

fn opt_text(out: &mut String, value: Option<&str>) {
    match value {
        Some(text) => esc(out, text),
        None => out.push_str("null"),
    }
}

/// Provider ids in the hub's chosen order, any missing ones appended.
fn ordered(settings: &PillSettings) -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = Vec::new();
    for wanted in &settings.provider_order {
        let known = PROVIDERS
            .iter()
            .map(|(id, _)| *id)
            .find(|id| *id == wanted.as_str());
        if let Some(id) = known.filter(|id| !ids.contains(id)) {
            ids.push(id);
        }
    }
    for (id, _) in PROVIDERS {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// The edge shared by every stored monitor entry, else top.
fn shared_edge(settings: &PillSettings) -> &'static str {
    settings
        .edges
        .values()
        .next()
        .map_or("top", |edge| edge.as_str())
}

fn visibility(settings: &PillSettings) -> &'static str {
    if !settings.visible {
        "hidden"
    } else if settings.folds {
        "onHover"
    } else {
        "alwaysShow"
    }
}

/// The snapshot in the Mac's shape (`product`, `settings`, `accounts`, `providerOrder`,
/// `permissions`, `updates`, ...). Mac key names carry the settings that exist on both; the
/// Windows-only ones keep their `pill-settings.json` names.
fn state_json(hooks: &Hooks) -> String {
    let (mut s, writable) = (hooks.settings)();
    // The Alt+Shift+5 toolbar flips this at runtime without touching the stored settings.
    s.screenshot_to_desktop = shot::save_to_desktop();
    let mut out = String::with_capacity(4096);
    out.push_str("{\"schema\":1,\"product\":\"Pulse\",\"platform\":\"windows\",\"version\":");
    esc(&mut out, update::current_version());
    out.push_str(",\"settings\":{");
    let flags: [(&str, bool); 16] = [
        ("launchAtLogin", s.launch_at_login),
        ("autoUpdateCheck", s.auto_update_check),
        ("nearbyEnabled", s.nearby_enabled),
        ("nearbyAcceptKnown", s.nearby_accept_known),
        ("announceUsageReset", s.announce_usage_reset),
        ("announceSessionLimitReached", s.announce_session_limit),
        ("announceWeeklyLimitReached", s.announce_weekly_limit),
        ("mac_shortcuts", s.mac_shortcuts),
        ("screenshot_shortcuts", s.screenshot_shortcuts),
        ("screenshot_to_desktop", s.screenshot_to_desktop),
        ("installer_auto", s.installer_auto),
        ("folds", s.folds),
        ("visible", s.visible),
        ("mute_claude_alerts", s.mute_claude_alerts),
        ("mute_codex_alerts", s.mute_codex_alerts),
        ("settingsWritable", writable),
    ];
    for (name, value) in flags {
        out.push_str(&format!("\"{name}\":{value},"));
    }
    out.push_str("\"nearbyAlias\":");
    opt_text(&mut out, s.nearby_alias.as_deref());
    out.push_str(",\"nearbySaveFolder\":");
    opt_text(&mut out, s.nearby_save_folder.as_deref());
    out.push_str(&format!(",\"cadence_seconds\":{}", s.cadence_seconds));
    out.push_str(",\"notchVisibility\":");
    esc(&mut out, visibility(&s));
    out.push_str(",\"notchEdge\":");
    esc(&mut out, shared_edge(&s));
    out.push_str(",\"edges\":{");
    for (index, (key, edge)) in s.edges.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        esc(&mut out, key);
        out.push(':');
        esc(&mut out, edge.as_str());
    }
    out.push_str("}},\"options\":{\"notchEdge\":[\"top\",\"bottom\",\"left\",\"right\"],");
    out.push_str("\"notchVisibility\":[\"alwaysShow\",\"onHover\",\"hidden\"]},\"displays\":[");
    for (index, key) in s.monitors.keys().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("{\"id\":");
        esc(&mut out, key);
        out.push_str(",\"name\":");
        esc(&mut out, key);
        out.push('}');
    }
    out.push_str("],\"accounts\":[");
    let snapshot = usage::snapshot();
    let order = ordered(&s);
    for (index, id) in order.iter().enumerate() {
        let Some(position) = PROVIDERS.iter().position(|(known, _)| known == id) else {
            continue;
        };
        let name = PROVIDERS[position].1;
        let reading = &snapshot[position.min(snapshot.len() - 1)];
        if index > 0 {
            out.push(',');
        }
        let connected = !s.hidden_providers.iter().any(|hidden| hidden == id);
        out.push_str("{\"id\":");
        esc(&mut out, id);
        out.push_str(",\"name\":");
        esc(&mut out, name);
        out.push_str(&format!(
            ",\"connected\":{connected},\"usesKeychain\":false,\"refusedAccess\":{},\"needsRenewal\":{},\"signInExplanation\":",
            reading.status == usage::Status::AccessDenied,
            reading.status == usage::Status::Expired,
        ));
        esc(&mut out, reading.status.text());
        out.push_str(",\"summary\":");
        esc(&mut out, reading.status.text());
        out.push_str(",\"label\":null,\"plan\":");
        opt_text(&mut out, reading.plan.as_deref());
        out.push_str(",\"limits\":[");
        for (n, window) in reading.windows.iter().enumerate() {
            if n > 0 {
                out.push(',');
            }
            out.push_str("{\"label\":");
            // Grouped windows (Codex Spark, code review) carry their group so the hub
            // can tell "Spark · 5h limit" from the account's own "5h limit".
            let label = match &window.group {
                Some(group) => format!("{group} · {}", window.label),
                None => window.label.clone(),
            };
            esc(&mut out, &label);
            out.push_str(&format!(
                ",\"usedFraction\":{}}}",
                if window.fraction.is_finite() {
                    window.fraction
                } else {
                    0.0
                }
            ));
        }
        out.push_str("]}");
    }
    out.push_str("],\"providerOrder\":[");
    for (index, id) in order.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        esc(&mut out, id);
    }
    // The hub reads the system itself for notifications and the firewall (its own Windows
    // rows); the notch only knows what it set up.
    out.push_str("],\"permissions\":[");
    let startup = if s.launch_at_login { "granted" } else { "off" };
    let rows = [
        ("startup", startup),
        ("notifications", "unknown"),
        ("firewall", "unknown"),
    ];
    for (index, (id, status)) in rows.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"id\":\"{id}\",\"status\":\"{status}\",\"required\":false}}"
        ));
    }
    out.push_str("],\"permissionErrors\":{},\"updates\":{\"current\":");
    esc(&mut out, update::current_version());
    out.push_str(&format!(",\"autoCheck\":{}}}}}\n", s.auto_update_check));
    out
}

// ------------------------------------------------------------------ commands in

/// Reads, applies and deletes every command file in name order. True when any file was handled.
fn drain(dir: &Path, controller_key: isize, hooks: &Hooks) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    files.truncate(MAX_COMMANDS);
    let handled = !files.is_empty();
    for file in files {
        let command = fs::read(&file)
            .ok()
            .and_then(|bytes| settings::parse_command(&bytes));
        let _ = fs::remove_file(&file);
        if let Some(command) = command {
            apply(&command, controller_key, hooks);
        }
    }
    handled
}

fn apply(command: &Command, controller_key: isize, hooks: &Hooks) {
    match command.name.as_str() {
        "set" | "connect" | "order" | "resetPosition" => {
            let (mut before, writable) = (hooks.settings)();
            // The Alt+Shift+5 toolbar flips this live; do not write the stale stored value back.
            before.screenshot_to_desktop = shot::save_to_desktop();
            let mut next = before.clone();
            let changed = match command.name.as_str() {
                "set" => command
                    .key
                    .as_deref()
                    .is_some_and(|key| apply_set(&mut next, key, &command.value)),
                "connect" => apply_connect(&mut next, command),
                "order" => apply_order(&mut next, &command.value),
                _ => {
                    next.positions.clear();
                    true
                }
            };
            if changed && next != before {
                commit(before, next, writable, controller_key, hooks);
            }
        }
        // Repaint from the current readings.
        "refresh" => repaint(controller_key),
        "checkUpdates" => (hooks.check_updates)(),
        "quit" => {
            // SAFETY: posting to a window handle that may have gone is harmless.
            let _ = unsafe {
                PostMessageW(
                    Some(raii::hwnd_from_key(controller_key)),
                    WM_CLOSE,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
        }
        // Mac-only (see the module comment) or unknown: nothing to do on Windows.
        _ => {}
    }
}

/// Applies the live parts of a change, then saves it and asks the notch to redraw.
fn commit(
    before: PillSettings,
    next: PillSettings,
    writable: bool,
    controller_key: isize,
    hooks: &Hooks,
) {
    if next.launch_at_login != before.launch_at_login && writable {
        autostart::apply(next.launch_at_login);
    }
    if next.nearby_enabled != before.nearby_enabled {
        send::set_enabled(next.nearby_enabled);
    }
    if next.installer_auto != before.installer_auto {
        installer::set_auto(next.installer_auto);
    }
    // Set before the commit: saving reads the live value back from `shot`.
    shot::set_save_to_desktop(next.screenshot_to_desktop);
    // `mac_shortcuts`, `screenshot_shortcuts` and `auto_update_check` are saved here and read
    // when the notch next starts (their workers take them as start arguments).
    (hooks.commit)(next);
    repaint(controller_key);
}

/// Posts the usage-updated message: the controller redraws every panel from the settings.
fn repaint(controller_key: isize) {
    // SAFETY: posting to a window handle that may have gone is harmless.
    let _ = unsafe {
        PostMessageW(
            Some(raii::hwnd_from_key(controller_key)),
            usage::MSG_USAGE_UPDATED,
            WPARAM(0),
            LPARAM(0),
        )
    };
}

fn flag(value: &Arg, slot: &mut bool) -> bool {
    match value {
        Arg::Bool(on) => {
            *slot = *on;
            true
        }
        _ => false,
    }
}

/// Empty or blank text clears an optional text setting.
fn optional_text(value: &Arg, slot: &mut Option<String>) -> bool {
    match value {
        Arg::Text(text) if text.len() <= settings::MAX_KEY_BYTES => {
            let trimmed = text.trim();
            *slot = (!trimmed.is_empty()).then(|| trimmed.to_string());
            true
        }
        Arg::Null => {
            *slot = None;
            true
        }
        _ => false,
    }
}

/// `set`: Mac key names where the setting exists on both systems, the `pill-settings.json`
/// names for Windows-only ones. Other Mac settings (appearance, sounds, launcher, window
/// management, conveniences) have no Windows meaning and are ignored.
fn apply_set(s: &mut PillSettings, key: &str, value: &Arg) -> bool {
    match key {
        "launchAtLogin" | "launch_at_login" => flag(value, &mut s.launch_at_login),
        "autoUpdateCheck" | "auto_update_check" => flag(value, &mut s.auto_update_check),
        "nearbyEnabled" | "nearby_enabled" => flag(value, &mut s.nearby_enabled),
        "nearbyAcceptKnown" | "nearby_accept_known" => flag(value, &mut s.nearby_accept_known),
        "nearbyAlias" | "nearby_alias" => optional_text(value, &mut s.nearby_alias),
        "nearbySaveFolder" | "nearby_save_folder" => {
            optional_text(value, &mut s.nearby_save_folder)
        }
        "announceUsageReset" | "announce_usage_reset" => flag(value, &mut s.announce_usage_reset),
        "announceSessionLimitReached" | "announce_session_limit" => {
            flag(value, &mut s.announce_session_limit)
        }
        "announceWeeklyLimitReached" | "announce_weekly_limit" => {
            flag(value, &mut s.announce_weekly_limit)
        }
        "mac_shortcuts" => flag(value, &mut s.mac_shortcuts),
        "screenshot_shortcuts" => flag(value, &mut s.screenshot_shortcuts),
        "screenshot_to_desktop" => flag(value, &mut s.screenshot_to_desktop),
        "installer_auto" => flag(value, &mut s.installer_auto),
        "folds" => flag(value, &mut s.folds),
        "visible" => flag(value, &mut s.visible),
        "mute_claude_alerts" => flag(value, &mut s.mute_claude_alerts),
        "mute_codex_alerts" => flag(value, &mut s.mute_codex_alerts),
        "notchVisibility" => match value {
            Arg::Text(text) if text == "alwaysShow" => {
                s.visible = true;
                s.folds = false;
                true
            }
            Arg::Text(text) if text == "onHover" => {
                s.visible = true;
                s.folds = true;
                true
            }
            Arg::Text(text) if text == "hidden" => {
                s.visible = false;
                true
            }
            _ => false,
        },
        "notchEdge" => {
            let Arg::Text(text) = value else {
                return false;
            };
            let Some(edge) = crate::layout::Edge::parse(text) else {
                return false;
            };
            // One edge for every monitor the notch knows about.
            let keys: Vec<String> = s
                .monitors
                .keys()
                .chain(s.positions.keys())
                .chain(s.edges.keys())
                .cloned()
                .collect();
            for key in keys {
                s.set_edge(&key, edge);
            }
            true
        }
        _ => false,
    }
}

fn provider_id(command: &Command) -> Option<&'static str> {
    let wanted = command.provider.as_deref()?;
    PROVIDERS.iter().map(|(id, _)| *id).find(|id| *id == wanted)
}

fn apply_connect(s: &mut PillSettings, command: &Command) -> bool {
    let (Some(id), Arg::Bool(on)) = (provider_id(command), &command.value) else {
        return false;
    };
    s.hidden_providers.retain(|hidden| hidden != id);
    if !on {
        s.hidden_providers.push(id.to_string());
    }
    true
}

fn apply_order(s: &mut PillSettings, value: &Arg) -> bool {
    let Arg::List(ids) = value else {
        return false;
    };
    s.provider_order = ids
        .iter()
        .filter(|id| PROVIDERS.iter().any(|(known, _)| *known == id.as_str()))
        .cloned()
        .collect();
    true
}
