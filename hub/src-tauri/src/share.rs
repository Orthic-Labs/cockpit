//! Nearby sharing in the hub process: LocalSend-compatible sending and
//! receiving (`pulse_core::localsend`) for as long as the hub runs, with the
//! window open or not.
//!
//! The notch is the only writer of its preferences and the hub's only link to
//! it is files plus payload-free Darwin notifications, so sharing follows that:
//!
//! * settings come from `notch-state.json` (`nearbyEnabled`, `nearbyAlias`,
//!   `nearbySaveFolder`, `nearbyAcceptKnown`), re-read when the notch posts
//!   `dev.orthic.pulse.notch.state`;
//! * the notch asks for things (send, accept, decline, cancel) by dropping JSON
//!   files into `share-commands/` and posting `dev.orthic.pulse.share.command`;
//!   A `send` command carries `to` (device fingerprint), `paths` (files), `text`
//!   and an optional `clipboard: true`, which asks the receiving Pulse to put
//!   what was sent on its clipboard (paste and screenshot sends);
//! * the hub publishes `share-state.json` and posts `dev.orthic.pulse.share.state`
//!   whenever devices, requests or progress change, and every few seconds so the
//!   notch can tell the hub is alive.
//!
//! On Windows the same files live in `%LOCALAPPDATA%\Pulse`, the Darwin
//! notifications become named auto-reset events (`Local\dev.orthic.pulse.share.state`
//! and `...share.command`, created by whichever side comes first), and the
//! settings are read from the notch's `pill-settings.json` (`nearby_enabled`,
//! `nearby_alias`, `nearby_save_folder`, `nearby_accept_known`), re-read when its
//! modified time changes. The notch (windows/src/send.rs) is the other end.
//!
//! The agent bridge (chats on this computer talking to chats on linked computers,
//! `pulse_core::bridge`) runs whenever the bridge is on (the core store policy,
//! hub-owned, default on), independent of sharing, over ssh links:
//! `agent_bridge` below starts the reply socket and the heartbeat tick.
//!
//! The page gets the same news as Tauri events: `share-devices`, `share-incoming`,
//! `share-incoming-resolved`, `share-progress`, `share-state`.

use pulse_core::bridge::deliver_claude::{self, ReplyHub};
use pulse_core::localsend::{Config, Event, SendItem, Service};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

const STATE_NOTIFICATION: &str = "dev.orthic.pulse.share.state";
const COMMAND_NOTIFICATION: &str = "dev.orthic.pulse.share.command";
#[cfg(target_os = "macos")]
const NOTCH_NOTIFICATION: &str = "dev.orthic.pulse.notch.state";

/// Payload-free wake-ups between the notch and the hub: Darwin notifications on
/// macOS, named auto-reset events on Windows, nothing elsewhere (the loop's own
/// one-second polling still runs).
#[cfg(target_os = "macos")]
mod bridge {
    unsafe extern "C" {
        fn notify_post(name: *const std::ffi::c_char) -> u32;
        fn notify_register_check(name: *const std::ffi::c_char, token: *mut i32) -> u32;
        fn notify_check(token: i32, changed: *mut i32) -> u32;
    }

    pub struct Watch(Option<i32>);

    pub fn post(name: &str) {
        if let Ok(name) = std::ffi::CString::new(name) {
            unsafe { notify_post(name.as_ptr()) };
        }
    }

    pub fn watch(name: &str) -> Watch {
        let Ok(name) = std::ffi::CString::new(name) else { return Watch(None) };
        let mut token = 0i32;
        if unsafe { notify_register_check(name.as_ptr(), &mut token) } != 0 {
            return Watch(None);
        }
        // The first check after registering always reports a change.
        let mut changed = 0i32;
        unsafe { notify_check(token, &mut changed) };
        Watch(Some(token))
    }

    impl Watch {
        pub fn fired(&self) -> bool {
            let Some(token) = self.0 else { return false };
            let mut changed = 0i32;
            unsafe { notify_check(token, &mut changed) == 0 && changed != 0 }
        }
    }
}

#[cfg(target_os = "windows")]
mod bridge {
    use std::ffi::c_void;

    type Handle = *mut c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateEventW(attributes: *const c_void, manual_reset: i32, initial_state: i32, name: *const u16) -> Handle;
        fn SetEvent(event: Handle) -> i32;
        fn CloseHandle(object: Handle) -> i32;
        fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    }

    /// An event this process keeps open so it outlives a moment with no peer.
    pub struct Watch(usize);

    fn open(name: &str) -> Handle {
        let wide: Vec<u16> = format!("Local\\{name}").encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: NUL-terminated name; auto-reset, initially not signaled. CreateEventW
        // opens the event when the other side made it first.
        unsafe { CreateEventW(std::ptr::null(), 0, 0, wide.as_ptr()) }
    }

    pub fn post(name: &str) {
        let event = open(name);
        if !event.is_null() {
            // SAFETY: a live event handle, closed right after.
            unsafe {
                SetEvent(event);
                CloseHandle(event);
            }
        }
    }

    pub fn watch(name: &str) -> Watch {
        Watch(open(name) as usize)
    }

    impl Watch {
        pub fn fired(&self) -> bool {
            let handle = self.0 as Handle;
            // SAFETY: the handle lives for the whole process; a zero timeout only polls.
            !handle.is_null() && unsafe { WaitForSingleObject(handle, 0) } == 0
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod bridge {
    pub struct Watch;
    pub fn post(_name: &str) {}
    pub fn watch(_name: &str) -> Watch {
        Watch
    }
    impl Watch {
        pub fn fired(&self) -> bool {
            false
        }
    }
}

static APP: OnceLock<AppHandle> = OnceLock::new();
static SERVICE: Mutex<Option<Arc<Service>>> = Mutex::new(None);
static RUNNING: Mutex<Option<Config>> = Mutex::new(None);
static ERROR: Mutex<Option<String>> = Mutex::new(None);
static NOTICE: Mutex<Option<(u64, String)>> = Mutex::new(None);
static BRIDGE_ENABLED: AtomicBool = AtomicBool::new(true);
static BRIDGE_ACTIVE: AtomicBool = AtomicBool::new(false);
static DIRTY: AtomicBool = AtomicBool::new(true);
static NOTICE_SEQ: AtomicU64 = AtomicU64::new(0);

fn home() -> PathBuf {
    #[cfg(windows)]
    let variable = "USERPROFILE";
    #[cfg(not(windows))]
    let variable = "HOME";
    std::env::var_os(variable).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// Where the notch and the hub meet: `~/Library/Application Support/Pulse`, or
/// `%LOCALAPPDATA%\Pulse` on Windows (the notch's settings directory).
fn bridge_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join("AppData").join("Local"))
            .join("Pulse")
    }
    #[cfg(not(windows))]
    {
        home().join("Library/Application Support/Pulse")
    }
}

fn post(name: &str) {
    bridge::post(name);
}

#[cfg(target_os = "macos")]
fn computer_name() -> String {
    for (program, args) in [("/usr/sbin/scutil", vec!["--get", "ComputerName"]), ("/bin/hostname", vec![])] {
        if let Ok(out) = std::process::Command::new(program).args(args).output() {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if out.status.success() && !text.is_empty() {
                return text;
            }
        }
    }
    "Mac".to_string()
}

#[cfg(not(target_os = "macos"))]
fn computer_name() -> String {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| "PC".to_string())
}

fn device_model() -> &'static str {
    if cfg!(target_os = "macos") {
        "Mac"
    } else if cfg!(windows) {
        "Windows"
    } else {
        "Computer"
    }
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if path == "~" => home(),
        None => PathBuf::from(path),
    }
}

/// The user's Downloads folder: the Known Folder on Windows (it can be moved),
/// `~/Downloads` elsewhere.
fn downloads_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(path) = windows_downloads() {
            return path;
        }
    }
    home().join("Downloads")
}

#[cfg(windows)]
fn windows_downloads() -> Option<PathBuf> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStringExt;

    #[repr(C)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }
    // FOLDERID_Downloads {374DE290-123F-4565-9164-39C4925E467B}
    static FOLDERID_DOWNLOADS: Guid = Guid {
        data1: 0x374D_E290,
        data2: 0x123F,
        data3: 0x4565,
        data4: [0x91, 0x64, 0x39, 0xC4, 0x92, 0x5E, 0x46, 0x7B],
    };
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHGetKnownFolderPath(id: *const Guid, flags: u32, token: *mut c_void, path: *mut *mut u16) -> i32;
    }
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoTaskMemFree(memory: *mut c_void);
    }
    let mut raw: *mut u16 = std::ptr::null_mut();
    // SAFETY: KF_FLAG_DEFAULT with the current user's token (null); on success `raw` is a
    // NUL-terminated string owned by the shell, copied out and freed with CoTaskMemFree.
    unsafe {
        let hr = SHGetKnownFolderPath(&FOLDERID_DOWNLOADS, 0, std::ptr::null_mut(), &mut raw);
        let path = if hr >= 0 && !raw.is_null() {
            let mut length = 0usize;
            while *raw.add(length) != 0 {
                length += 1;
            }
            let wide = std::slice::from_raw_parts(raw, length);
            Some(PathBuf::from(std::ffi::OsString::from_wide(wide)))
        } else {
            None
        };
        if !raw.is_null() {
            CoTaskMemFree(raw.cast());
        }
        path.filter(|p| !p.as_os_str().is_empty())
    }
}

/// The notch's sharing preferences as one object with the keys the Mac writes.
#[cfg(not(windows))]
fn settings_object() -> Value {
    let state: Value = std::fs::read_to_string(bridge_dir().join("notch-state.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    state["settings"].clone()
}

/// The Windows notch keeps its preferences in `pill-settings.json` (snake_case
/// keys, all optional); this maps the sharing ones onto the Mac's names.
#[cfg(windows)]
fn settings_object() -> Value {
    let file: Value = std::fs::read_to_string(bridge_dir().join("pill-settings.json"))
        .ok()
        .map(|text| text.trim_start_matches('\u{feff}').to_string())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    json!({
        "nearbyEnabled": file["nearby_enabled"],
        "nearbyAlias": file["nearby_alias"],
        "nearbySaveFolder": file["nearby_save_folder"],
        "nearbyAcceptKnown": file["nearby_accept_known"],
    })
}

/// Changes when the file the preferences come from does.
#[cfg(windows)]
fn settings_stamp() -> Option<std::time::SystemTime> {
    std::fs::metadata(bridge_dir().join("pill-settings.json"))
        .and_then(|m| m.modified())
        .ok()
}

/// What the preferences ask for, or None when sharing is switched off.
fn wanted() -> Option<Config> {
    let settings = settings_object();
    if settings["nearbyEnabled"].as_bool() == Some(false) {
        return None;
    }
    let alias = settings["nearbyAlias"]
        .as_str()
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string)
        .unwrap_or_else(computer_name);
    let folder = settings["nearbySaveFolder"]
        .as_str()
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(expand_home)
        .unwrap_or_else(downloads_dir);
    Some(Config {
        alias,
        port: pulse_core::localsend::proto::PORT,
        save_dir: folder,
        accept_known: settings["nearbyAcceptKnown"].as_bool() == Some(true),
        state_dir: bridge_dir().join("localsend"),
        device_model: device_model().to_string(),
    })
}

fn current_service() -> Option<Arc<Service>> {
    SERVICE.lock().ok().and_then(|s| s.clone())
}

fn on_event(event: Event) {
    DIRTY.store(true, Ordering::Relaxed);
    let Some(app) = APP.get() else { return };
    match event {
        Event::Devices => {
            let devices = current_service().map(|s| s.devices()).unwrap_or_default();
            let _ = app.emit("share-devices", devices);
        }
        Event::Incoming(incoming) => {
            let _ = app.emit("share-incoming", incoming);
        }
        Event::IncomingResolved(id) => {
            let _ = app.emit("share-incoming-resolved", id);
        }
        Event::Progress(transfer) | Event::Finished(transfer) => {
            let _ = app.emit("share-progress", transfer);
        }
        Event::Changed => {}
    }
}

fn notice(text: String) {
    let seq = NOTICE_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    if let Ok(mut slot) = NOTICE.lock() {
        *slot = Some((seq, text));
    }
    DIRTY.store(true, Ordering::Relaxed);
}

fn same(a: &Config, b: &Config) -> bool {
    a.alias == b.alias && a.port == b.port && a.state_dir == b.state_dir
}

/// Start, stop or restart the service so it matches the preferences.
fn reconcile() {
    let want = wanted();
    let running = RUNNING.lock().ok().and_then(|r| r.clone());
    match (want, running) {
        (None, None) => {}
        (None, Some(_)) => {
            stop();
        }
        (Some(want), Some(running)) if same(&want, &running) => {
            if let Some(service) = current_service() {
                service.set_accept_known(want.accept_known);
                service.set_save_dir(want.save_dir.clone());
            }
            if let Ok(mut r) = RUNNING.lock() {
                *r = Some(want);
            }
            DIRTY.store(true, Ordering::Relaxed);
        }
        (Some(want), previous) => {
            if previous.is_some() {
                stop();
                std::thread::sleep(Duration::from_millis(300));
            }
            let mut result = Service::start(want.clone(), Arc::new(on_event));
            for _ in 0..3 {
                if result.is_ok() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
                result = Service::start(want.clone(), Arc::new(on_event));
            }
            match result {
                Ok(service) => {
                    if let Ok(mut s) = SERVICE.lock() {
                        *s = Some(Arc::new(service));
                    }
                    if let Ok(mut r) = RUNNING.lock() {
                        *r = Some(want);
                    }
                    if let Ok(mut e) = ERROR.lock() {
                        *e = None;
                    }
                }
                Err(message) => {
                    if let Ok(mut e) = ERROR.lock() {
                        *e = Some(message);
                    }
                }
            }
            DIRTY.store(true, Ordering::Relaxed);
        }
    }
}

fn stop() {
    let old = SERVICE.lock().ok().and_then(|mut s| s.take());
    if let Some(service) = old {
        service.stop();
    }
    if let Ok(mut r) = RUNNING.lock() {
        *r = None;
    }
    DIRTY.store(true, Ordering::Relaxed);
}

// ---- agent bridge ----------------------------------------------------------------

mod agent_bridge {
    use super::*;

    fn settings_path() -> PathBuf {
        bridge_dir().join("bridge-settings.json")
    }

    /// The persisted policy in the core bridge store (the CLI and every adapter read it).
    fn core_policy() -> Option<bool> {
        pulse_core::bridge::Store::open_default().ok().map(|s| s.bridge_enabled())
    }

    fn set_core_policy(on: bool) {
        if let Ok(store) = pulse_core::bridge::Store::open_default() {
            let _ = store.set_bridge_enabled(on);
        }
    }

    /// Startup: the core policy is the source of truth. A legacy `bridge-settings.json` is
    /// migrated into it once (then renamed), so an old Off survives the upgrade.
    pub fn load() {
        let legacy = std::fs::read_to_string(settings_path())
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["enabled"].as_bool());
        if let Some(on) = legacy {
            set_core_policy(on);
            let migrated = bridge_dir().join("bridge-settings.json.migrated");
            let _ = std::fs::rename(settings_path(), migrated);
        }
        BRIDGE_ENABLED.store(core_policy().or(legacy).unwrap_or(true), Ordering::Relaxed);
    }

    pub fn set_enabled(on: bool) {
        BRIDGE_ENABLED.store(on, Ordering::Relaxed);
        set_core_policy(on);
        DIRTY.store(true, Ordering::Relaxed);
    }

    /// Why the hub refuses to run the bridge, when it does.
    #[cfg(target_os = "windows")]
    fn blocked() -> Option<String> {
        crate::win_bridge::singleton_error()
    }

    #[cfg(not(target_os = "windows"))]
    fn blocked() -> Option<String> {
        None
    }

    /// Start or stop the bridge so it matches the switch.
    pub fn sync() {
        // The CLI can flip the policy too; follow the core store.
        if let Some(on) = core_policy() {
            if on != BRIDGE_ENABLED.swap(on, Ordering::Relaxed) {
                DIRTY.store(true, Ordering::Relaxed);
            }
        }
        let want = BRIDGE_ENABLED.load(Ordering::Relaxed) && blocked().is_none();
        let have = BRIDGE_ACTIVE.load(Ordering::Relaxed);
        if want && !have {
            pulse_core::bridge::hub::start(&computer_name());
            let on_reply = Arc::new(|reply: deliver_claude::ReplyMessage| {
                std::thread::spawn(move || pulse_core::bridge::hub::on_local_reply(reply));
            });
            let is_known = Arc::new(|id: &str| pulse_core::bridge::hub::is_known_local_session(id));
            deliver_claude::set_reply_hub(Some(ReplyHub::new(on_reply, is_known)));
            BRIDGE_ACTIVE.store(true, Ordering::Relaxed);
            DIRTY.store(true, Ordering::Relaxed);
        } else if !want && have {
            deliver_claude::set_reply_hub(None);
            pulse_core::bridge::hub::stop();
            BRIDGE_ACTIVE.store(false, Ordering::Relaxed);
            DIRTY.store(true, Ordering::Relaxed);
        }
    }

    /// Heartbeat and session rescan, every couple of seconds, off the main loop.
    pub fn start_tick() {
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(Duration::from_secs(2));
                if !BRIDGE_ACTIVE.load(Ordering::Relaxed) {
                    continue;
                }
                pulse_core::bridge::hub::tick(&computer_name());
                DIRTY.store(true, Ordering::Relaxed);
            }
        });
    }

    /// Requests from the `pulse bridge` CLI (`pulse_core::bridge::control`), polled on their own
    /// thread; each runs on its own thread too, since delivery over ssh can take seconds.
    pub fn start_control() {
        std::thread::spawn(|| {
            let Ok(store) = pulse_core::bridge::Store::open_default() else { return };
            loop {
                std::thread::sleep(Duration::from_millis(250));
                if !BRIDGE_ACTIVE.load(Ordering::Relaxed) {
                    continue;
                }
                for request in pulse_core::bridge::control::take_requests(&store) {
                    let store = store.clone();
                    std::thread::spawn(move || {
                        let reply = pulse_core::bridge::hub::handle_control(&request);
                        pulse_core::bridge::control::reply(&store, &request.id, &reply);
                        DIRTY.store(true, Ordering::Relaxed);
                    });
                }
            }
        });
    }

    /// `{enabled, active, device, localChats, links, lastError}` for the page.
    pub fn state() -> Value {
        let mut value = serde_json::to_value(pulse_core::bridge::hub::status()).unwrap_or(Value::Null);
        if !value.is_object() {
            value = json!({});
        }
        let object = value.as_object_mut().expect("object");
        object.insert("enabled".into(), json!(BRIDGE_ENABLED.load(Ordering::Relaxed)));
        object.insert("active".into(), json!(BRIDGE_ACTIVE.load(Ordering::Relaxed)));
        object.insert("device".into(), json!(computer_name()));
        if let Some(error) = blocked() {
            object.insert("lastError".into(), json!(error));
        }
        // Links carry `lastOkMs`/`lastErrorMs` and chats `liveness` straight from core status;
        // `activity.lastOutcome` passes through the serialized Activity.
        value
    }
}

// ---- state out ---------------------------------------------------------------

fn state_value() -> Value {
    let running = RUNNING.lock().ok().map(|r| r.is_some()).unwrap_or(false);
    let error = ERROR.lock().ok().and_then(|e| e.clone());
    let notice = NOTICE.lock().ok().and_then(|n| n.clone());
    let mut value = match current_service() {
        Some(service) => serde_json::to_value(service.snapshot()).unwrap_or(Value::Null),
        None => json!({
            "devices": [], "incoming": [], "transfers": [], "warnings": [],
            "localNetwork": "unknown", "scanning": false,
        }),
    };
    if let Some(object) = value.as_object_mut() {
        object.insert("schema".into(), json!(1));
        object.insert("running".into(), json!(running));
        // "port_in_use" when another program (the LocalSend app) holds the port; the notch and
        // the page show it in words instead of a silent "Starting…".
        let kind = error
            .as_deref()
            .map(|e| if e.contains(pulse_core::localsend::PORT_IN_USE) { "port_in_use" } else { "start_failed" });
        object.insert("errorKind".into(), kind.map(Value::from).unwrap_or(Value::Null));
        // The Windows notch reads this file with a reader that has no booleans, so the
        // few it needs also come as 0/1 numbers (the Mac notch ignores them).
        object.insert("runningN".into(), json!(running as u8));
        let scanning = object.get("scanning").and_then(Value::as_bool).unwrap_or(false);
        object.insert("scanningN".into(), json!(scanning as u8));
        if let Some(Value::Array(list)) = object.get_mut("incoming") {
            for item in list {
                let message = item["isMessage"].as_bool().unwrap_or(false);
                item["isMessageN"] = json!(message as u8);
            }
        }
        object.insert("bridge".into(), agent_bridge::state());
        object.insert("error".into(), error.map(Value::from).unwrap_or(Value::Null));
        object.insert(
            "notice".into(),
            notice
                .map(|(id, text)| json!({"id": id, "text": text}))
                .unwrap_or(Value::Null),
        );
        object.insert(
            "updatedAt".into(),
            json!(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0)
            ),
        );
    }
    value
}

fn write_state() {
    let value = state_value();
    let dir = bridge_dir();
    let _ = std::fs::create_dir_all(&dir);
    let temp = dir.join("share-state.json.tmp");
    let Ok(bytes) = serde_json::to_vec(&value) else { return };
    if std::fs::write(&temp, bytes).is_ok() && std::fs::rename(&temp, dir.join("share-state.json")).is_ok() {
        post(STATE_NOTIFICATION);
    }
}

// ---- commands in -------------------------------------------------------------

fn drain_commands() {
    let dir = bridge_dir().join("share-commands");
    let Ok(listing) = std::fs::read_dir(&dir) else { return };
    let mut files: Vec<PathBuf> = listing
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    for file in files {
        let command: Option<Value> = std::fs::read(&file)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let _ = std::fs::remove_file(&file);
        if let Some(command) = command {
            apply(&command);
        }
    }
}

fn apply(command: &Value) {
    let Some(service) = current_service() else {
        if command["command"] == "send" {
            notice("Nearby sharing isn't running.".to_string());
        }
        return;
    };
    let id = command["id"].as_str().unwrap_or("");
    match command["command"].as_str() {
        Some("send") => {
            let to = command["to"].as_str().unwrap_or("");
            let paths: Vec<String> = command["paths"]
                .as_array()
                .map(|a| a.iter().filter_map(|p| p.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            let text = command["text"].as_str().map(str::to_string);
            let clipboard = command["clipboard"].as_bool().unwrap_or(false);
            if let Err(message) = send(&service, to, paths, text, clipboard) {
                notice(message);
            }
        }
        Some("accept") => {
            service.respond(id, true);
        }
        Some("decline") => {
            service.respond(id, false);
        }
        Some("cancel") => {
            service.cancel(id);
        }
        Some("dismiss") => service.dismiss(id),
        Some("refresh") => service.refresh(),
        _ => {}
    }
}

fn send(
    service: &Service,
    to: &str,
    paths: Vec<String>,
    text: Option<String>,
    clipboard: bool,
) -> Result<String, String> {
    let mut items: Vec<SendItem> = paths.into_iter().map(|p| SendItem::Path(PathBuf::from(p))).collect();
    if let Some(text) = text.filter(|t| !t.is_empty()) {
        items.push(SendItem::Text(text));
    }
    if items.is_empty() {
        return Err("There is nothing to send.".to_string());
    }
    service.send(to, items, clipboard)
}

// ---- the loop ------------------------------------------------------------------

/// Run sharing for as long as the hub lives.
pub fn start_background(app: AppHandle) {
    let _ = APP.set(app);
    std::thread::spawn(|| {
        let commands = bridge::watch(COMMAND_NOTIFICATION);
        #[cfg(target_os = "macos")]
        let notch = bridge::watch(NOTCH_NOTIFICATION);
        #[cfg(windows)]
        let mut settings_seen = settings_stamp();
        #[cfg(windows)]
        let mut settings_checked = Instant::now();
        let mut last_write = Instant::now() - Duration::from_secs(10);
        let mut last_reconcile = Instant::now() - Duration::from_secs(60);
        let mut last_drain = Instant::now();
        agent_bridge::load();
        agent_bridge::start_tick();
        agent_bridge::start_control();
        reconcile();
        let mut last_bridge_sync = Instant::now() - Duration::from_secs(10);
        loop {
            std::thread::sleep(Duration::from_millis(100));
            if commands.fired() || last_drain.elapsed() >= Duration::from_secs(1) {
                last_drain = Instant::now();
                drain_commands();
            }
            let failing = ERROR.lock().ok().is_some_and(|e| e.is_some());
            #[cfg(target_os = "macos")]
            let settings_changed = notch.fired();
            #[cfg(windows)]
            let settings_changed = settings_checked.elapsed() >= Duration::from_secs(2) && {
                settings_checked = Instant::now();
                let stamp = settings_stamp();
                let changed = stamp != settings_seen;
                settings_seen = stamp;
                changed
            };
            #[cfg(not(any(target_os = "macos", windows)))]
            let settings_changed = false;
            if settings_changed || (failing && last_reconcile.elapsed() >= Duration::from_secs(10)) {
                last_reconcile = Instant::now();
                reconcile();
            }
            if last_bridge_sync.elapsed() >= Duration::from_secs(1) {
                last_bridge_sync = Instant::now();
                agent_bridge::sync();
            }
            let due = DIRTY.load(Ordering::Relaxed) && last_write.elapsed() >= Duration::from_millis(120);
            if due || last_write.elapsed() >= Duration::from_secs(4) {
                DIRTY.store(false, Ordering::Relaxed);
                last_write = Instant::now();
                write_state();
            }
        }
    });
}

// ---- page commands -------------------------------------------------------------

/// Nearby devices, incoming requests and transfers.
#[tauri::command]
pub fn share_state() -> Value {
    state_value()
}

#[tauri::command]
pub fn share_devices() -> Value {
    serde_json::to_value(current_service().map(|s| s.devices()).unwrap_or_default()).unwrap_or(Value::Null)
}

/// Send files and/or text to the device with this fingerprint. Returns the
/// transfer id; progress arrives as `share-progress` events.
#[tauri::command]
pub fn share_send(to: String, paths: Vec<String>, text: Option<String>) -> Result<String, String> {
    let service = current_service().ok_or_else(|| "Nearby sharing isn't running.".to_string())?;
    send(&service, &to, paths, text, false)
}

#[tauri::command]
pub fn share_accept(id: String) -> bool {
    current_service().is_some_and(|s| s.respond(&id, true))
}

#[tauri::command]
pub fn share_decline(id: String) -> bool {
    current_service().is_some_and(|s| s.respond(&id, false))
}

#[tauri::command]
pub fn share_cancel(id: String) -> bool {
    current_service().is_some_and(|s| s.cancel(&id))
}

#[tauri::command]
pub fn share_dismiss(id: String) {
    if let Some(service) = current_service() {
        service.dismiss(&id);
    }
}

/// Turn the agent bridge on or off (hub-owned switch, default on).
#[tauri::command]
pub fn bridge_set_enabled(on: bool) {
    agent_bridge::set_enabled(on);
}

/// Install the Pulse bridge skill for Claude and Codex; returns what was written.
#[tauri::command]
pub fn bridge_install_skill() -> Result<Value, String> {
    let report = pulse_core::bridge::install::apply(false, &Default::default())?;
    serde_json::to_value(report).map_err(|e| e.to_string())
}

/// Open System Settings at Local Network (macOS) or the Windows Firewall's
/// app list (Windows). Changes no permission.
#[tauri::command]
pub fn open_local_network_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork")
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("control.exe")
            .arg("firewall.cpl")
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Err("Not needed on this system.".to_string())
    }
}
