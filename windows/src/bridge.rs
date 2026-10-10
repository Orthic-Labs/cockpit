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
//! `resetPosition`, `refresh` (reads Claude and Codex now), `checkUpdates`, `installUpdate`,
//! `forgetReading` (and the Mac's `signOut`, the same thing: clears what Pulse read for a
//! provider), the account book's `renameClaudeAccount` / `forgetClaudeAccount`, and `quit`.
//! Mac-only commands are ignored on purpose: `signIn`, `allowAccess` (Keychain sign-in), the
//! three `preview*Alert` and `sendTestNotification` previews, `openAccessibilitySettings`,
//! `permissionRequest`, `helperEnable`, `helperDisable`, `openLoginItems` (macOS permissions and
//! the privileged helper) and `driveAlert` (the Windows drive health card reads the disks
//! itself).

use crate::layout::{self, Cell};
use crate::settings::{self, Arg, Command, PillSettings};
use crate::{
    autostart, claude_accounts, desktop, diag, installer, json, keys, raii, render, send, shot,
    update, usage,
};
use std::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

type Handle = *mut c_void;

const POLL_MS: u32 = 2000;
const STATE_EVENT: &str = "Local\\dev.orthic.pulse.notch.state";
const COMMAND_EVENT: &str = "Local\\dev.orthic.pulse.hub.command";
const STATE_FILE: &str = "notch-state.json";
const COMMANDS_DIR: &str = "hub-commands";
/// Posted to the controller when a hub command changed where or how a notch is placed
/// (edge, position, monitor set, fold, visibility): it re-plans the panels like a display change.
pub const MSG_PLACEMENT_CHANGED: u32 = 0x8002;
/// Command files handled per pass; the rest wait for the next one.
const MAX_COMMANDS: usize = 64;
const PROVIDERS: [(&str, &str); 2] = [("claude", "Claude"), ("codex", "Codex")];
/// The gauges, in the notch's order (`Cell::ALL`): id, name and the cell each one turns on or
/// off. CPU and memory are one ring here, so one gauge ("system-cpu"); the Mac's separate
/// "system-memory" and "system-tools" have no Windows cell.
const GAUGES: [(&str, &str, Cell); 5] = [
    ("claude", "Claude", Cell::Claude),
    ("codex", "Codex", Cell::Codex),
    ("system-cpu", "System", Cell::Cpu),
    ("system-disks", "Disks", Cell::Disk),
    ("system-send", "Send", Cell::Send),
];
/// How long the permission rows are reused before the registry is read again.
const PERMISSIONS_TTL: Duration = Duration::from_secs(10);
const SHARE_STATE_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// The Permissions rows the notch reports: what the hub's own Windows checks answer
/// (`granted`, `needsApproval`, `off`, `unknown`).
#[derive(Clone, Copy, PartialEq)]
struct Permissions {
    startup: &'static str,
    notifications: &'static str,
    firewall: &'static str,
}

static PERMISSIONS: Mutex<Option<(Instant, bool, Permissions)>> = Mutex::new(None);
/// A permission needs the user: the notch's amber dot.
static ATTENTION: AtomicBool = AtomicBool::new(false);

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
    /// Device name of the primary monitor: the edge published for the hub's Edge control is
    /// that monitor's.
    pub primary_monitor: fn() -> Option<String>,
    /// The latest machine sample (for the `system` readings block).
    pub machine: fn() -> Option<crate::sensors::Machine>,
}

/// Windows Firewall for Nearby sharing, as the hub's share service reports it
/// (`share-state.json` `localNetwork`); "off" while sharing is off.
fn firewall_status(nearby: bool) -> &'static str {
    if !nearby {
        return "off";
    }
    let state = pulse_dir()
        .and_then(|dir| desktop::read_bounded(&dir.join("share-state.json"), SHARE_STATE_MAX_BYTES))
        .and_then(|bytes| json::parse(&bytes, SHARE_STATE_MAX_BYTES as usize));
    match state
        .as_ref()
        .and_then(|root| root.get("localNetwork"))
        .and_then(json::Value::as_str)
    {
        Some("granted") => "granted",
        Some("blocked") => "needsApproval",
        _ => "unknown",
    }
}

/// The permission rows, read from the system at most every ten seconds (and again at once
/// when sharing is switched on or off).
fn permissions(nearby: bool) -> Permissions {
    let mut cached = PERMISSIONS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, was_nearby, rows)) = *cached
        && was_nearby == nearby
        && at.elapsed() < PERMISSIONS_TTL
    {
        return rows;
    }
    let rows = Permissions {
        startup: autostart::startup_status(),
        notifications: autostart::notifications_status(),
        firewall: firewall_status(nearby),
    };
    *cached = Some((Instant::now(), nearby, rows));
    ATTENTION.store(
        rows.notifications == "needsApproval" || rows.firewall == "needsApproval",
        Ordering::Relaxed,
    );
    rows
}

/// A permission is denied or unknown to the system: the notch shows its amber dot.
pub fn permissions_attention() -> bool {
    ATTENTION.load(Ordering::Relaxed)
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

/// Wakes the worker so it republishes now rather than at its next poll: the notch moved
/// itself (an Alt-drag or the grip took it to another edge) and the hub should follow.
pub fn wake() {
    let event = open_event(COMMAND_EVENT);
    if event != 0 {
        // SAFETY: a live event handle, closed right after; the worker holds its own.
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
    // The stored Colour settings, before the first notch is drawn.
    apply_appearance(&(hooks.settings)().0);
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
    let mut attention = false;
    loop {
        let applied = drain(&commands, controller_key, hooks);
        publish(&dir, hooks, &mut last, applied);
        // The permission rows were just read: the notch's amber dot follows them.
        if permissions_attention() != attention {
            attention = permissions_attention();
            repaint(controller_key);
        }
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

/// The edge the notch is docked to on the primary monitor: what the hub's Edge control shows.
/// A move is stored per monitor (`edges`, and only when it differs from `edge_default`) and a
/// pick in the hub is the default, so a monitor without an entry follows the default.
fn published_edge(settings: &PillSettings, primary: Option<&str>) -> &'static str {
    primary
        .map_or(settings.edge_default, |key| settings.edge(key))
        .as_str()
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

/// What the hub's account row says when there is no reading: the way to sign in (Pulse never
/// signs in itself; it reads the login the tool keeps), or where the reading stands.
fn sign_in_guidance(id: &str, reading: &usage::Usage) -> String {
    let (tool, how) = if id == "claude" {
        (
            "Claude",
            "Sign in to Claude Code (run claude, then /login) or open Claude Desktop and sign in.",
        )
    } else {
        ("Codex", "Sign in with codex login in a terminal.")
    };
    match reading.status {
        usage::Status::SignIn => {
            format!("{how} Pulse reads that login; it never signs in itself.")
        }
        usage::Status::Expired => {
            format!(
                "The {tool} sign-in expired. Open {tool} once to refresh it; Pulse then reads it again."
            )
        }
        usage::Status::AccessDenied => format!(
            "Windows refused Pulse access to the saved {tool} login. Fix the file's permissions."
        ),
        _ => reading.summary(),
    }
}

/// The `system` block the Mac publishes (`SystemReadingsStore`): network, battery, fans,
/// temperatures and the memory pressure word, each present only when this PC gave a reading.
/// Windows has no battery cycle count or health through the unelevated API, so those stay out.
fn system_json(machine: Option<&crate::sensors::Machine>) -> String {
    use crate::sensors::Reading;
    let number = |value: f64| if value.is_finite() { value } else { 0.0 };
    let mut parts: Vec<String> = Vec::new();
    if let Some(machine) = machine {
        if let Reading::Value(rate) = &machine.network {
            let mut kind = String::new();
            esc(&mut kind, &rate.kind);
            parts.push(format!(
                "\"network\":{{\"interface\":{kind},\"kind\":{kind},\"down\":{},\"up\":{}}}",
                number(rate.down),
                number(rate.up)
            ));
        }
        if let Reading::Value(fans) = &machine.fans
            && !fans.is_empty()
        {
            let rows: Vec<String> = fans
                .iter()
                .enumerate()
                .map(|(n, rpm)| format!("{{\"name\":\"Fan {}\",\"rpm\":{rpm}}}", n + 1))
                .collect();
            parts.push(format!("\"fans\":[{}]", rows.join(",")));
        }
        if let Reading::Value(temps) = &machine.temperature
            && !temps.is_empty()
        {
            let rows: Vec<String> = temps
                .iter()
                .map(|t| {
                    let mut name = String::new();
                    esc(&mut name, t.source);
                    format!(
                        "{{\"name\":{name},\"celsius\":{:.1}}}",
                        number(f64::from(t.celsius))
                    )
                })
                .collect();
            parts.push(format!("\"temperatures\":[{}]", rows.join(",")));
        }
        if let Some(pressure) = machine.memory.as_ref().and_then(|m| m.pressure()) {
            parts.push(format!("\"memoryPressure\":\"{pressure}\""));
        }
    }
    if let Reading::Value(battery) = crate::sensors::battery() {
        parts.push(format!(
            "\"battery\":{{\"percent\":{},\"charging\":{}}}",
            battery.percent, battery.charging
        ));
    }
    format!("{{{}}}", parts.join(","))
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
    let primary = (hooks.primary_monitor)();
    esc(&mut out, published_edge(&s, primary.as_deref()));
    // Size: the Mac's three named sizes plus the slider that overrides them.
    out.push_str(",\"notchSize\":");
    esc(&mut out, s.notch_size.as_str());
    out.push_str(&format!(
        ",\"usesCustomNotchScale\":{},\"customNotchScale\":{}",
        s.custom_scale_on,
        f64::from(s.custom_scale_milli) / 1000.0
    ));
    // Where alerts go (the Mac's values: `notch`, or `mac` = a Windows system toast).
    out.push_str(",\"notificationChannel\":");
    esc(&mut out, s.notification_channel.as_str());
    // Colour (the Mac's `accentColor`, `watchLimit`, `criticalLimit`); no colour transition.
    out.push_str(",\"accentColor\":");
    esc(&mut out, s.accent_color.as_str());
    out.push_str(&format!(
        ",\"watchLimit\":{},\"criticalLimit\":{}",
        f64::from(s.watch_limit_milli) / 1000.0,
        f64::from(s.critical_limit_milli) / 1000.0
    ));
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
    out.push_str("\"notchVisibility\":[\"alwaysShow\",\"onHover\",\"hidden\"],");
    out.push_str("\"notchSize\":[\"small\",\"medium\",\"large\"],");
    out.push_str("\"notificationChannel\":[\"notch\",\"mac\"],\"accentColor\":[");
    for (index, accent) in settings::AccentColor::ALL.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        esc(&mut out, accent.as_str());
    }
    out.push_str("]},\"displays\":[");
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
        // A reading exists: its line is the summary (and "Forget reading" applies). With none,
        // the hub shows the sign-in guidance instead, as for an account the Mac has no
        // reading for.
        let has_reading = !reading.windows.is_empty() || reading.status == usage::Status::Ok;
        out.push_str(&format!(
            ",\"connected\":{connected},\"usesKeychain\":false,\"refusedAccess\":{},\"needsRenewal\":{},\"signInExplanation\":",
            reading.status == usage::Status::AccessDenied,
            reading.status == usage::Status::Expired,
        ));
        esc(&mut out, &sign_in_guidance(id, reading));
        if has_reading {
            out.push_str(",\"summary\":");
            esc(&mut out, &reading.summary());
        }
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
                ",\"usedFraction\":{}",
                if window.fraction.is_finite() {
                    window.fraction
                } else {
                    0.0
                }
            ));
            // The window's length, as on the Mac, so the hub and the CLI can tell a
            // 5-hour window from a weekly one.
            if let Some(seconds) = window.seconds() {
                out.push_str(&format!(",\"seconds\":{seconds}"));
            }
            out.push('}');
        }
        out.push(']');
        // The hub's Accounts list of every Claude account on this PC.
        if *id == "claude" {
            out.push_str(",\"claudeAccounts\":");
            out.push_str(&claude_accounts::published_json());
        }
        out.push('}');
    }
    out.push_str("],\"gauges\":[");
    for (order, (id, name, cell)) in GAUGES.iter().enumerate() {
        if order > 0 {
            out.push(',');
        }
        out.push_str("{\"id\":");
        esc(&mut out, id);
        out.push_str(",\"name\":");
        esc(&mut out, name);
        out.push_str(",\"glyph\":");
        // The hub names its paper-plane icon "send"; the notch's own glyph key is "Snd".
        esc(
            &mut out,
            if *cell == Cell::Send { "send" } else { cell.glyph() },
        );
        out.push_str(&format!(
            ",\"connected\":{},\"order\":{order}}}",
            !gauge_hidden(&s, id)
        ));
    }
    out.push_str("],\"providerOrder\":[");
    for (index, id) in order.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        esc(&mut out, id);
    }
    // The same checks the hub's own Permissions page makes (hub `permissions.rs` and the
    // share service's `localNetwork`): startup entry, toasts, Windows Firewall for sharing.
    out.push_str("],\"permissions\":[");
    let rows = permissions(s.nearby_enabled);
    let rows = [
        ("startup", rows.startup),
        ("notifications", rows.notifications),
        ("firewall", rows.firewall),
    ];
    for (index, (id, status)) in rows.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"id\":\"{id}\",\"status\":\"{status}\",\"required\":false}}"
        ));
    }
    out.push_str("],\"permissionErrors\":{},\"updates\":");
    out.push_str(&update::hub_json());
    out.push_str(",\"system\":");
    out.push_str(&system_json((hooks.machine)().as_ref()));
    out.push_str("}\n");
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
        let bytes = fs::read(&file).ok();
        let _ = fs::remove_file(&file);
        let Some(bytes) = bytes else {
            continue;
        };
        // The hub's Claude account commands (rename, forget) are the account book's.
        if claude_accounts::apply_command(&bytes) {
            continue;
        }
        if let Some(command) = settings::parse_command(&bytes) {
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
        // Read both providers now (the Mac's `.fromSource` refresh), then repaint as they land.
        "refresh" => {
            usage::refresh_now();
            repaint(controller_key);
        }
        "checkUpdates" => (hooks.check_updates)(),
        "installUpdate" => update::install_now(),
        // Clears what Pulse read for the provider; the next poll reads it again.
        "forgetReading" | "signOut" => {
            if let Some(provider) = provider_id(command).and_then(|id| {
                usage::Provider::ALL
                    .into_iter()
                    .find(|p| p.name().eq_ignore_ascii_case(id))
            }) {
                usage::forget(provider);
                repaint(controller_key);
            }
        }
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
    // Both keyboard features and the update schedule change at once, without a restart: the
    // hook is installed or removed, the screenshot thread started or stopped.
    if next.mac_shortcuts != before.mac_shortcuts
        || next.screenshot_shortcuts != before.screenshot_shortcuts
    {
        keys::apply(next.mac_shortcuts, next.screenshot_shortcuts);
    }
    if next.auto_update_check != before.auto_update_check {
        update::set_auto(next.auto_update_check);
    }
    let placement_changed = next.edges != before.edges
        || next.edge_default != before.edge_default
        || next.positions != before.positions
        || next.monitors != before.monitors
        || next.folds != before.folds
        || next.visible != before.visible
        || next.notch_size != before.notch_size
        // Gauges turned on or off change the rings, so the body's width.
        || next.hidden_providers != before.hidden_providers
        || next.custom_scale_on != before.custom_scale_on
        || next.custom_scale_milli != before.custom_scale_milli
        // A colour change moves nothing, but the panels must draw again with the new colours.
        || next.accent_color != before.accent_color
        || next.watch_limit_milli != before.watch_limit_milli
        || next.critical_limit_milli != before.critical_limit_milli;
    apply_appearance(&next);
    apply_gauges(&next);
    (hooks.commit)(next);
    if placement_changed {
        // SAFETY: posting to a window handle that may have gone is harmless.
        let _ = unsafe {
            PostMessageW(
                Some(raii::hwnd_from_key(controller_key)),
                MSG_PLACEMENT_CHANGED,
                WPARAM(0),
                LPARAM(0),
            )
        };
    }
    repaint(controller_key);
}

/// Hands the Colour settings to the renderer: the ample accent and the watch and critical limits.
fn apply_appearance(s: &PillSettings) {
    render::set_appearance(
        s.accent_color.rgb(),
        s.watch_limit_milli,
        s.critical_limit_milli,
    );
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
            // One edge for every monitor, like the Mac's single setting: the default covers
            // all of them (the monitors are not known by name here), and a monitor moved by
            // hand before loses its own entry. `published_edge` reads this default back.
            s.edge_default = edge;
            s.edges.clear();
            true
        }
        "notchSize" => match value {
            Arg::Text(text) => match settings::NotchSize::parse(text) {
                Some(size) => {
                    s.notch_size = size;
                    true
                }
                None => false,
            },
            _ => false,
        },
        "notificationChannel" | "notification_channel" => match value {
            Arg::Text(text) => match settings::NotificationChannel::parse(text) {
                Some(channel) => {
                    s.notification_channel = channel;
                    true
                }
                None => false,
            },
            _ => false,
        },
        "accentColor" | "accent_color" => match value {
            Arg::Text(text) => match settings::AccentColor::parse(text) {
                Some(accent) => {
                    s.accent_color = accent;
                    true
                }
                None => false,
            },
            _ => false,
        },
        "watchLimit" | "watch_limit" => match value {
            Arg::Number(fraction) if fraction.is_finite() => {
                s.set_watch_limit(*fraction);
                true
            }
            _ => false,
        },
        "criticalLimit" | "critical_limit" => match value {
            Arg::Number(fraction) if fraction.is_finite() => {
                s.set_critical_limit(*fraction);
                true
            }
            _ => false,
        },
        "usesCustomNotchScale" | "uses_custom_notch_scale" => flag(value, &mut s.custom_scale_on),
        "customNotchScale" | "custom_notch_scale" => match value {
            Arg::Number(scale) if scale.is_finite() => {
                s.custom_scale_milli = settings::custom_scale_milli(*scale);
                true
            }
            _ => false,
        },
        _ => false,
    }
}

fn provider_id(command: &Command) -> Option<&'static str> {
    let wanted = command.provider.as_deref()?;
    PROVIDERS.iter().map(|(id, _)| *id).find(|id| *id == wanted)
}

fn gauge_hidden(s: &PillSettings, id: &str) -> bool {
    s.hidden_providers.iter().any(|hidden| hidden == id)
}

/// Hands the gauges that are on to the layout: the notch draws those cells, in its order.
/// Turning every gauge off is never honoured (`apply_connect` refuses it), and the layout keeps
/// all cells should stored settings say so.
pub fn apply_gauges(s: &PillSettings) {
    layout::set_shown(
        GAUGES
            .iter()
            .filter(|(id, _, _)| !gauge_hidden(s, id))
            .map(|(_, _, cell)| *cell)
            .collect(),
    );
}

/// `connect`: turns a gauge on or off. The last gauge on cannot be turned off.
fn apply_connect(s: &mut PillSettings, command: &Command) -> bool {
    let (Some(wanted), Arg::Bool(on)) = (command.provider.as_deref(), &command.value) else {
        return false;
    };
    let Some(id) = GAUGES
        .iter()
        .map(|(id, _, _)| *id)
        .find(|id| *id == wanted)
    else {
        return false;
    };
    s.hidden_providers.retain(|hidden| hidden != id);
    if !on {
        s.hidden_providers.push(id.to_string());
        if GAUGES.iter().all(|(known, _, _)| gauge_hidden(s, known)) {
            return false;
        }
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
