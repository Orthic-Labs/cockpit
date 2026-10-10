//! Windows side of the notch bridge (the counterpart of the Darwin notifications in
//! `watch_notch`). The native notch signals the manual-reset named event
//! `Local\dev.orthic.pulse.hub.show.<section>` on a running hub (windows/src/hub.rs). The
//! hub creates one such event per section, plus `hub.select.<section>` (switch the page
//! without showing the window), and a user-scoped `Local\pulse-hub-<hash>` mutex (current-user-only
//! security descriptor) that keeps a second hub from starting. Raw kernel32 declarations avoid a second `windows` crate
//! version beside the one tauri already pulls in.

use std::ffi::c_void;
use std::sync::Mutex;
use std::time::Duration;
use tauri::Emitter;

type Handle = *mut c_void;

const WAIT_OBJECT_0: u32 = 0;
const INFINITE: u32 = 0xFFFF_FFFF;
const EVENT_MODIFY_STATE: u32 = 0x0002;
const ERROR_ALREADY_EXISTS: i32 = 183;
/// WaitForMultipleObjects accepts at most MAXIMUM_WAIT_OBJECTS (64) handles.
const MAX_WAIT: usize = 64;

/// Why the singleton could not be secured; the bridge stays off while this is set.
static SINGLETON_ERROR: Mutex<Option<String>> = Mutex::new(None);

pub fn singleton_error() -> Option<String> {
    SINGLETON_ERROR.lock().ok().and_then(|e| e.clone())
}

/// Win32 `SECURITY_ATTRIBUTES`; handed to the kernel as raw memory, never read in Rust.
#[repr(C)]
#[allow(dead_code)]
struct SecurityAttributes {
    length: u32,
    descriptor: *mut c_void,
    inherit: i32,
}

/// The start of Win32 `TOKEN_USER` (`SID_AND_ATTRIBUTES`).
#[repr(C)]
#[allow(dead_code)]
struct TokenUser {
    sid: *mut c_void,
    attributes: u32,
}

const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_USER_CLASS: u32 = 1;
const SDDL_REVISION_1: u32 = 1;

#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn GetTokenInformation(token: Handle, class: u32, info: *mut c_void, len: u32, out: *mut u32) -> i32;
    fn ConvertSidToStringSidW(sid: *mut c_void, text: *mut *mut u16) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        sddl: *const u16,
        revision: u32,
        descriptor: *mut *mut c_void,
        size: *mut u32,
    ) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateEventW(attributes: *const c_void, manual_reset: i32, initial_state: i32, name: *const u16) -> Handle;
    fn OpenEventW(access: u32, inherit: i32, name: *const u16) -> Handle;
    fn SetEvent(event: Handle) -> i32;
    fn ResetEvent(event: Handle) -> i32;
    fn CloseHandle(object: Handle) -> i32;
    fn GetCurrentProcess() -> Handle;
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
    fn CreateMutexW(attributes: *const c_void, initial_owner: i32, name: *const u16) -> Handle;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn WaitForMultipleObjects(count: u32, handles: *const Handle, wait_all: i32, milliseconds: u32) -> u32;
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn show_name(section: &str) -> String {
    format!("Local\\dev.orthic.pulse.hub.show.{section}")
}

/// Signals the auto-reset event `Local\<name>` (created when absent, so a notch that starts
/// later still sees the request). Used for `dev.orthic.pulse.hub.command`, which the notch
/// waits on after it drains `hub-commands\`; the Mac posts a Darwin notification instead.
pub fn post(name: &str) {
    let event_name = wide(&format!("Local\\{name}"));
    // SAFETY: NUL-terminated name; auto-reset, initially non-signaled. The handle is closed
    // right after the signal; a waiting notch keeps the event alive.
    unsafe {
        let event = CreateEventW(std::ptr::null(), 0, 0, event_name.as_ptr());
        if !event.is_null() {
            SetEvent(event);
            CloseHandle(event);
        }
    }
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

/// The current user's SID as text (`S-1-5-21-...`).
fn user_sid() -> Result<String, String> {
    // SAFETY: standard token query; every handle and buffer is released before returning.
    unsafe {
        let mut token: Handle = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let mut buffer = vec![0u8; 512];
        let mut needed = 0u32;
        let ok = GetTokenInformation(
            token,
            TOKEN_USER_CLASS,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &mut needed,
        );
        let failure = std::io::Error::last_os_error().to_string();
        CloseHandle(token);
        if ok == 0 {
            return Err(failure);
        }
        let user = buffer.as_ptr().cast::<TokenUser>().read_unaligned();
        let mut text: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.sid, &mut text) == 0 || text.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let mut len = 0;
        while *text.add(len) != 0 {
            len += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        LocalFree(text.cast());
        Ok(sid)
    }
}

fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

/// Creates the user-scoped singleton mutex with a descriptor that grants only the current
/// user. Ok(true) when this process owns it, Ok(false) when another hub already does.
fn create_singleton() -> Result<bool, String> {
    let sid = user_sid()?;
    let name = wide(&format!("Local\\pulse-hub-{:016x}", fnv1a(&sid)));
    let sddl = wide(&format!("D:P(A;;GA;;;{sid})"));
    let mut descriptor: *mut c_void = std::ptr::null_mut();
    // SAFETY: NUL-terminated SDDL; the descriptor it allocates is freed after the mutex call,
    // and the mutex handle is deliberately never closed.
    let (handle, exists) = unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let attributes = SecurityAttributes {
            length: std::mem::size_of::<SecurityAttributes>() as u32,
            descriptor,
            inherit: 0,
        };
        let handle = CreateMutexW((&attributes as *const SecurityAttributes).cast(), 0, name.as_ptr());
        // Read the error before any other call can overwrite it.
        let error = std::io::Error::last_os_error();
        LocalFree(descriptor);
        if handle.is_null() {
            return Err(error.to_string());
        }
        (handle, error.raw_os_error() == Some(ERROR_ALREADY_EXISTS))
    };
    let _ = handle;
    Ok(!exists)
}

/// Claims the single-instance mutex. When another hub holds it, asks that hub to show the
/// requested section (unless this launch is `--background`) and returns false: the caller
/// exits. When the mutex cannot be created securely the hub still opens but fails closed:
/// the agent bridge stays off and the page shows why.
pub fn claim_instance() -> bool {
    match create_singleton() {
        Ok(true) => return true,
        Ok(false) => {}
        Err(error) => {
            if let Ok(mut slot) = SINGLETON_ERROR.lock() {
                *slot = Some(format!("could not secure the hub singleton: {error}"));
            }
            return true;
        }
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
    // The notch signals `Local\dev.orthic.pulse.notch.state` after each atomic write of
    // `notch-state.json`; the page refreshes through the same "notch-state" event as on the Mac.
    let state_name = wide("Local\\dev.orthic.pulse.notch.state");
    // SAFETY: NUL-terminated name; auto-reset, initially non-signaled; the handle lives for
    // the whole process.
    let state_event = unsafe { CreateEventW(std::ptr::null(), 0, 0, state_name.as_ptr()) } as usize;
    if state_event != 0 {
        std::thread::spawn({
            let app = app.clone();
            move || {
                // SAFETY: a live event handle owned by this thread for the process lifetime.
                while unsafe { WaitForSingleObject(state_event as Handle, INFINITE) } == WAIT_OBJECT_0 {
                    let _ = app.emit("notch-state", String::new());
                }
            }
        });
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
