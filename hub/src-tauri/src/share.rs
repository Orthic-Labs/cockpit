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
//! * the hub publishes `share-state.json` and posts `dev.orthic.pulse.share.state`
//!   whenever devices, requests or progress change, and every few seconds so the
//!   notch can tell the hub is alive.
//!
//! The page gets the same news as Tauri events: `share-devices`, `share-incoming`,
//! `share-incoming-resolved`, `share-progress`, `share-state`.

use pulse_core::localsend::{Config, Event, SendItem, Service};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

unsafe extern "C" {
    fn notify_post(name: *const std::ffi::c_char) -> u32;
    fn notify_register_check(name: *const std::ffi::c_char, token: *mut i32) -> u32;
    fn notify_check(token: i32, changed: *mut i32) -> u32;
}

const STATE_NOTIFICATION: &str = "dev.orthic.pulse.share.state";
const COMMAND_NOTIFICATION: &str = "dev.orthic.pulse.share.command";
const NOTCH_NOTIFICATION: &str = "dev.orthic.pulse.notch.state";

static APP: OnceLock<AppHandle> = OnceLock::new();
static SERVICE: Mutex<Option<Arc<Service>>> = Mutex::new(None);
static RUNNING: Mutex<Option<Config>> = Mutex::new(None);
static ERROR: Mutex<Option<String>> = Mutex::new(None);
static NOTICE: Mutex<Option<(u64, String)>> = Mutex::new(None);
static DIRTY: AtomicBool = AtomicBool::new(true);
static NOTICE_SEQ: AtomicU64 = AtomicU64::new(0);

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn bridge_dir() -> PathBuf {
    home().join("Library/Application Support/Pulse")
}

fn post(name: &str) {
    if let Ok(name) = std::ffi::CString::new(name) {
        unsafe { notify_post(name.as_ptr()) };
    }
}

fn mac_name() -> String {
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

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if path == "~" => home(),
        None => PathBuf::from(path),
    }
}

/// What the preferences ask for, or None when sharing is switched off.
fn wanted() -> Option<Config> {
    let state: Value = std::fs::read_to_string(bridge_dir().join("notch-state.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    let settings = &state["settings"];
    if settings["nearbyEnabled"].as_bool() == Some(false) {
        return None;
    }
    let alias = settings["nearbyAlias"]
        .as_str()
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{} (Pulse)", mac_name()));
    let folder = settings["nearbySaveFolder"]
        .as_str()
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(expand_home)
        .unwrap_or_else(|| home().join("Downloads"));
    Some(Config {
        alias,
        port: pulse_core::localsend::proto::PORT,
        save_dir: folder,
        accept_known: settings["nearbyAcceptKnown"].as_bool() == Some(true),
        state_dir: bridge_dir().join("localsend"),
        device_model: "Mac".to_string(),
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

// ---- state out ---------------------------------------------------------------

fn state_value() -> Value {
    let running = RUNNING.lock().ok().map(|r| r.is_some()).unwrap_or(false);
    let error = ERROR.lock().ok().and_then(|e| e.clone());
    let notice = NOTICE.lock().ok().and_then(|n| n.clone());
    let mut value = match current_service() {
        Some(service) => serde_json::to_value(service.snapshot()).unwrap_or(Value::Null),
        None => json!({
            "devices": [], "incoming": [], "transfers": [], "warnings": [],
            "localNetwork": "unknown",
        }),
    };
    if let Some(object) = value.as_object_mut() {
        object.insert("schema".into(), json!(1));
        object.insert("running".into(), json!(running));
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
            if let Err(message) = send(&service, to, paths, text) {
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
        _ => {}
    }
}

fn send(service: &Service, to: &str, paths: Vec<String>, text: Option<String>) -> Result<String, String> {
    let mut items: Vec<SendItem> = paths.into_iter().map(|p| SendItem::Path(PathBuf::from(p))).collect();
    if let Some(text) = text.filter(|t| !t.is_empty()) {
        items.push(SendItem::Text(text));
    }
    if items.is_empty() {
        return Err("There is nothing to send.".to_string());
    }
    service.send(to, items)
}

// ---- the loop ------------------------------------------------------------------

fn register(name: &str) -> Option<i32> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut token = 0i32;
    if unsafe { notify_register_check(name.as_ptr(), &mut token) } != 0 {
        return None;
    }
    // The first check after registering always reports a change.
    let mut changed = 0i32;
    unsafe { notify_check(token, &mut changed) };
    Some(token)
}

fn fired(token: Option<i32>) -> bool {
    let Some(token) = token else { return false };
    let mut changed = 0i32;
    unsafe { notify_check(token, &mut changed) == 0 && changed != 0 }
}

/// Run sharing for as long as the hub lives.
pub fn start_background(app: AppHandle) {
    let _ = APP.set(app);
    std::thread::spawn(|| {
        let commands = register(COMMAND_NOTIFICATION);
        let notch = register(NOTCH_NOTIFICATION);
        let mut last_write = Instant::now() - Duration::from_secs(10);
        let mut last_reconcile = Instant::now() - Duration::from_secs(60);
        let mut last_drain = Instant::now();
        reconcile();
        loop {
            std::thread::sleep(Duration::from_millis(100));
            if fired(commands) || last_drain.elapsed() >= Duration::from_secs(1) {
                last_drain = Instant::now();
                drain_commands();
            }
            let failing = ERROR.lock().ok().is_some_and(|e| e.is_some());
            if fired(notch) || (failing && last_reconcile.elapsed() >= Duration::from_secs(10)) {
                last_reconcile = Instant::now();
                reconcile();
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
    send(&service, &to, paths, text)
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

/// Open System Settings at Local Network. Changes no permission.
#[tauri::command]
pub fn open_local_network_settings() -> Result<(), String> {
    std::process::Command::new("/usr/bin/open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork")
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}
