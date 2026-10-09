//! Windows side of the notch bridge (the counterpart of the Darwin notifications in
//! `watch_notch`). The native notch signals the manual-reset named event
//! `Local\dev.orthic.pulse.hub.show.<section>` on a running hub (windows/src/hub.rs). The
//! hub creates one such event per section, plus `hub.select.<section>` (switch the page
//! without showing the window), and a `Local\dev.orthic.pulse.hub.instance` mutex that keeps
//! a second hub from starting. Raw kernel32 declarations avoid a second `windows` crate
//! version beside the one tauri already pulls in.

use std::ffi::c_void;
use std::time::Duration;
use tauri::Emitter;

type Handle = *mut c_void;

const WAIT_OBJECT_0: u32 = 0;
const INFINITE: u32 = 0xFFFF_FFFF;
const EVENT_MODIFY_STATE: u32 = 0x0002;
const ERROR_ALREADY_EXISTS: i32 = 183;
/// WaitForMultipleObjects accepts at most MAXIMUM_WAIT_OBJECTS (64) handles.
const MAX_WAIT: usize = 64;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateEventW(attributes: *const c_void, manual_reset: i32, initial_state: i32, name: *const u16) -> Handle;
    fn OpenEventW(access: u32, inherit: i32, name: *const u16) -> Handle;
    fn SetEvent(event: Handle) -> i32;
    fn ResetEvent(event: Handle) -> i32;
    fn CloseHandle(object: Handle) -> i32;
    fn CreateMutexW(attributes: *const c_void, initial_owner: i32, name: *const u16) -> Handle;
    fn WaitForMultipleObjects(count: u32, handles: *const Handle, wait_all: i32, milliseconds: u32) -> u32;
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn show_name(section: &str) -> String {
    format!("Local\\dev.orthic.pulse.hub.show.{section}")
}

/// Signals a running hub's show event for `section`. False when it has not created it.
fn signal(section: &str) -> bool {
    let name = wide(&show_name(section));
    // SAFETY: NUL-terminated name; the opened handle is closed before returning.
    unsafe {
        let event = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
        if event.is_null() {
            return false;
        }
        let signalled = SetEvent(event) != 0;
        CloseHandle(event);
        signalled
    }
}

/// Claims the single-instance mutex. When another hub holds it, asks that hub to show the
/// requested section (unless this launch is `--background`) and returns false: the caller
/// exits. The handle is kept for the life of the process.
pub fn claim_instance() -> bool {
    let name = wide("Local\\dev.orthic.pulse.hub.instance");
    // SAFETY: NUL-terminated name; the handle is deliberately never closed.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    // Read the error before any other call can overwrite it.
    let exists = std::io::Error::last_os_error().raw_os_error() == Some(ERROR_ALREADY_EXISTS);
    if handle.is_null() {
        // Cannot tell; do not block the hub from starting.
        return true;
    }
    if !exists {
        return true;
    }
    if !std::env::args().any(|a| a == "--background") {
        let args: Vec<String> = std::env::args().collect();
        let requested = args
            .iter()
            .position(|a| a == "--section")
            .and_then(|i| args.get(i + 1))
            .filter(|s| crate::SECTIONS.contains(&s.as_str()))
            .map(String::as_str)
            .unwrap_or("overview");
        // The first hub may still be creating its events; retry for a few seconds.
        for _ in 0..50 {
            if signal(requested) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    false
}

/// An event handle that only its one waiting thread uses.
#[derive(Clone)]
struct Slot {
    handle: usize,
    show: bool,
    section: &'static str,
}

/// Creates the show/select events and forwards each signal to the page.
pub fn watch(app: tauri::AppHandle) {
    let mut slots: Vec<Slot> = Vec::new();
    for section in crate::SECTIONS {
        for (prefix, show) in [("show", true), ("select", false)] {
            let name = wide(&format!("Local\\dev.orthic.pulse.hub.{prefix}.{section}"));
            // SAFETY: NUL-terminated name; manual-reset, initially non-signaled; the handle
            // lives for the whole process.
            let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, name.as_ptr()) };
            if !handle.is_null() {
                slots.push(Slot { handle: handle as usize, show, section });
            }
        }
    }
    for chunk in slots.chunks(MAX_WAIT).map(<[Slot]>::to_vec) {
        let app = app.clone();
        std::thread::spawn(move || wait_loop(app, chunk));
    }
    // A section passed at launch (`--section`) is also sent once the page has had time to
    // load and subscribe, as on macOS.
    if let Some(section) = crate::initial_section() {
        std::thread::spawn({
            let app = app.clone();
            move || {
                std::thread::sleep(Duration::from_millis(900));
                let _ = app.emit("show-section", section);
            }
        });
    }
}

fn wait_loop(app: tauri::AppHandle, slots: Vec<Slot>) {
    let handles: Vec<Handle> = slots.iter().map(|s| s.handle as Handle).collect();
    loop {
        // SAFETY: `handles` holds at most 64 live event handles owned by this process.
        let result = unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE) };
        let index = result.wrapping_sub(WAIT_OBJECT_0) as usize;
        let Some(slot) = slots.get(index) else {
            // WAIT_FAILED or an abandoned object: stop rather than spin.
            return;
        };
        // SAFETY: the handle is a live manual-reset event.
        unsafe { ResetEvent(handles[index]) };
        if slot.show {
            crate::show_in_dock(&app);
        }
        let _ = app.emit("show-section", slot.section.to_string());
    }
}
