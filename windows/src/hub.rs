//! Opens the shared Tauri hub from the notch (the Windows counterpart of the Mac notch's
//! `HubLauncher`). The hub runs as a child process of the notch, started with
//! `--section <name>`. A running hub is told which section to show through a named event
//! (`Local\dev.orthic.pulse.hub.show.<section>`, which the hub must open and wait on; see
//! windows/README.md). Until the hub listens, a visible hub window is brought forward, and
//! a hidden one is restarted with the requested section.

use crate::diag;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, PoisonError};
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

fn spawn(section: &str) -> Option<Child> {
    let Some(path) = locate() else {
        diag::info("hub_missing", &[("action", "click_ignored")]);
        return None;
    };
    let spawned = Command::new(&path)
        .arg("--section")
        .arg(section)
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
    *child = spawn(section);
    child.is_some()
}

/// Ends the hub this notch started (called when the notch quits).
pub fn terminate() {
    let mut child = CHILD.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(mut running) = child.take() {
        let _ = running.kill();
        let _ = running.wait();
    }
}
