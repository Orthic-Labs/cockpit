//! Runs and opens the shared Tauri hub from the notch (the Windows counterpart of the Mac
//! notch's `HubLauncher`). The hub is Pulse's daemon: `supervise` starts it with
//! `--background` (no window) when the notch starts, whether or not Nearby sharing is on, and
//! starts it again, at most once every 30 seconds, if it exits. Only quitting the notch
//! (`terminate`) stops it; closing the hub's window never does. A window is asked for with
//! `--section <name>`. A running hub is told which section to show through a named event
//! (`Local\dev.orthic.pulse.hub.show.<section>`, which the hub must open and wait on; see
//! windows/README.md). Until the hub listens, a visible hub window is brought forward, and
//! a hidden one is restarted with the requested section.

use crate::diag;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GetWindow, GetWindowThreadProcessId, IsWindowVisible, SW_RESTORE,
    SetForegroundWindow, ShowWindow,
};
use windows::core::{BOOL, PCWSTR};

const HUB_EXE: &str = "pulse-hub.exe";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const EVENT_MODIFY_STATE: u32 = 0x0002;
/// Sections the hub understands for `--section` and the show-section events.
const SECTIONS: [&str; 9] = [
    "overview",
    "settings",
    "storage",
    "monitor",
    "cleanup",
    "apps",
    "accounts",
    "general",
    "permissions",
];

#[allow(non_snake_case)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenEventW(access: u32, inherit: BOOL, name: PCWSTR) -> HANDLE;
    fn SetEvent(event: HANDLE) -> BOOL;
}

static CHILD: Mutex<Option<Child>> = Mutex::new(None);
/// Set by `terminate`: the notch is quitting, so the hub is not started again.
static QUITTING: AtomicBool = AtomicBool::new(false);
static SUPERVISING: AtomicBool = AtomicBool::new(false);
/// When the hub was last started, for the 30-second spacing between starts.
static LAST_LAUNCH: Mutex<Option<Instant>> = Mutex::new(None);
const SUPERVISE_TICK: Duration = Duration::from_secs(5);
const RESTART_SPACING: Duration = Duration::from_secs(30);

fn locate() -> Option<PathBuf> {
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

/// Signals a running hub's show-section event. False when the hub has not created it.
fn signal(section: &str) -> bool {
    let name: Vec<u16> = format!("Local\\dev.orthic.pulse.hub.show.{section}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: NUL-terminated name; the handle is closed before returning.
    unsafe {
        let event = OpenEventW(EVENT_MODIFY_STATE, BOOL(0), PCWSTR(name.as_ptr()));
        if event.0.is_null() {
            return false;
        }
        let signalled = SetEvent(event).as_bool();
        let _ = CloseHandle(event);
        signalled
    }
}

/// Whether a running hub has created the show-section event (without signalling it).
fn listening(section: &str) -> bool {
    let name: Vec<u16> = format!("Local\\dev.orthic.pulse.hub.show.{section}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: NUL-terminated name; the handle is closed before returning.
    unsafe {
        let event = OpenEventW(EVENT_MODIFY_STATE, BOOL(0), PCWSTR(name.as_ptr()));
        if event.0.is_null() {
            return false;
        }
        let _ = CloseHandle(event);
        true
    }
}

struct Raise {
    pid: u32,
    found: bool,
}

unsafe extern "system" fn raise_window(hwnd: HWND, data: LPARAM) -> BOOL {
    // EnumWindows passes live handles; `data` is the caller's stack `Raise`.
    unsafe {
        let state = &mut *(data.0 as *mut Raise);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        // GetWindow reports "no owner" as an error as well as a null handle.
        let owned = match GetWindow(hwnd, GW_OWNER) {
            Ok(owner) => !owner.0.is_null(),
            Err(_) => false,
        };
        if pid == state.pid && !owned && IsWindowVisible(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
            state.found = true;
            return BOOL(0);
        }
    }
    BOOL(1)
}

/// Brings a visible window of `pid` forward; false when it has none.
fn raise(pid: u32) -> bool {
    let mut state = Raise { pid, found: false };
    // The callback only reads window attributes; `state` outlives the synchronous call.
    // EnumWindows reports an error when the callback stops it early, which is expected.
    let _ = unsafe {
        EnumWindows(
            Some(raise_window),
            LPARAM(&mut state as *mut Raise as isize),
        )
    };
    state.found
}

/// `Some(section)` starts the hub with its window on that section; `None` starts it as the
/// windowless daemon.
fn spawn(section: Option<&str>) -> Option<Child> {
    // Counted even when it fails, so a missing hub is looked for (and logged) once in 30 s.
    *LAST_LAUNCH.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
    let Some(path) = locate() else {
        diag::info("hub_missing", &[("action", "not_started")]);
        return None;
    };
    let mut command = Command::new(&path);
    match section {
        Some(section) => command.arg("--section").arg(section),
        None => command.arg("--background"),
    };
    let spawned = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    match spawned {
        Ok(child) => Some(child),
        Err(error) => {
            diag::info(
                "hub_spawn_failed",
                &[("reason", error.to_string().as_str())],
            );
            None
        }
    }
}

/// Opens the hub on `section`. Returns false when no hub is installed or it cannot start.
pub fn open(section: &str) -> bool {
    if !SECTIONS.contains(&section) {
        return false;
    }
    if QUITTING.load(Ordering::Relaxed) {
        return false;
    }
    let mut child = CHILD.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(running) = child.as_mut() {
        if let Ok(None) = running.try_wait() {
            if signal(section) || raise(running.id()) {
                return true;
            }
            // Alive but hidden and not listening: restart it on the section.
            let _ = running.kill();
            let _ = running.wait();
        }
        *child = None;
    }
    // A hub this notch did not start may already be listening: ask it rather than start a second.
    if signal(section) {
        return true;
    }
    *child = spawn(Some(section));
    child.is_some()
}

/// Keeps the hub running for as long as the notch does: starts it now (unless a hub already
/// answers on its events), then looks every few seconds and starts it again if it has
/// exited, never more than once in 30 seconds (a hub that dies at once must not spin).
pub fn supervise() {
    if SUPERVISING.swap(true, Ordering::Relaxed) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("pulse-hub-supervisor".into())
        .spawn(|| {
            let mut first = true;
            while !QUITTING.load(Ordering::Relaxed) {
                ensure_running(first);
                first = false;
                std::thread::sleep(SUPERVISE_TICK);
            }
        });
    if let Err(error) = spawned {
        diag::info(
            "hub_supervisor_failed",
            &[("reason", error.to_string().as_str())],
        );
    }
}

fn ensure_running(first: bool) {
    let mut child = CHILD.lock().unwrap_or_else(PoisonError::into_inner);
    if QUITTING.load(Ordering::Relaxed) {
        return;
    }
    let mut exited = None;
    if let Some(running) = child.as_mut() {
        match running.try_wait() {
            Ok(None) => return,
            Ok(Some(status)) => {
                exited = Some(
                    status
                        .code()
                        .map_or_else(|| "signal".to_string(), |code| code.to_string()),
                );
            }
            Err(_) => exited = Some("unknown".to_string()),
        }
        *child = None;
    }
    if let Some(code) = &exited {
        diag::info("hub_exited", &[("code", code.as_str())]);
    }
    // A hub this notch did not start (for example one the user opened before) is the daemon.
    if listening("overview") {
        return;
    }
    let recent = LAST_LAUNCH
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_some_and(|at| at.elapsed() < RESTART_SPACING);
    if recent {
        return;
    }
    let reason = if first { "start" } else { "restart" };
    *child = spawn(None);
    diag::info(
        "hub_launch",
        &[
            ("mode", "background"),
            ("reason", reason),
            ("started", if child.is_some() { "true" } else { "false" }),
        ],
    );
}

/// Ends the hub this notch started (called when the notch quits: the only way the hub stops).
pub fn terminate() {
    QUITTING.store(true, Ordering::Relaxed);
    let mut child = CHILD.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(mut running) = child.take() {
        let _ = running.kill();
        let _ = running.wait();
    }
}
