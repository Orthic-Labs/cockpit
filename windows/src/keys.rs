//! Global keyboard layer: Mac-style editing shortcuts and the screenshot hotkeys.
//!
//! One `WH_KEYBOARD_LL` hook on a dedicated thread with its own message loop (the hook
//! callback runs inside that thread's `GetMessageW`, so it must return quickly and never
//! block). Physical events only: anything flagged `LLKHF_INJECTED` or carrying our own
//! `dwExtraInfo` tag is passed through untouched, so the keys we send never loop back.
//!
//! * Alt+A / C / V / X / Z act as Ctrl+A / C / V / X / Z; Alt+Shift+Z acts as Ctrl+Y.
//!   The physical key is swallowed and one atomic `SendInput` batch is injected: a dummy
//!   `VK_E8` press (so the Alt release that follows never opens a menu), Alt up (and Shift
//!   up when held), Ctrl+key, then Shift/Alt back down and another dummy `VK_E8` press, so
//!   the user's still-held Alt is consistent and its physical release arrives as a normal
//!   Alt-up that no longer counts as a bare Alt tap. Alt is never swallowed itself.
//! * AltGr (reported as Ctrl+Alt), any real Ctrl+Alt, and anything with the Windows key
//!   down pass through, as do Alt+Tab, Alt+F4 and Alt+Space (only the keys listed above
//!   are handled).
//! * Alt+Shift+4 / Alt+Shift+5 request the screenshot region picker / toolbar from
//!   `shot.rs`. While a screenshot session is active, Esc and Space are swallowed and
//!   forwarded to it (the overlay windows never take focus, so the hook is how they see
//!   the keyboard).
//! * `set_extra_handler` is the hook point for other Alt chords (e.g. a send shortcut): the
//!   handler runs on the hook thread for every Alt+key press nothing else claimed and
//!   returns true to swallow it.

use crate::{diag, shot};
use std::mem::size_of;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN, LLKHF_INJECTED, MSG,
    PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_USER,
};

const VK_ESCAPE: u32 = 0x1B;
const VK_SPACE: u32 = 0x20;
const VK_SHIFT: u32 = 0x10;
const VK_CONTROL: u32 = 0x11;
const VK_MENU: u32 = 0x12;
const VK_LSHIFT: u32 = 0xA0;
const VK_RSHIFT: u32 = 0xA1;
const VK_LCONTROL: u32 = 0xA2;
const VK_RCONTROL: u32 = 0xA3;
const VK_LMENU: u32 = 0xA4;
const VK_RMENU: u32 = 0xA5;
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;
const VK_4: u32 = 0x34;
const VK_5: u32 = 0x35;
const VK_A: u32 = 0x41;
const VK_C: u32 = 0x43;
const VK_V: u32 = 0x56;
const VK_X: u32 = 0x58;
const VK_Y: u32 = 0x59;
const VK_Z: u32 = 0x5A;
/// Unassigned virtual key used to break the "Alt pressed and released alone" sequence.
const VK_DUMMY: u32 = 0xE8;
/// `dwExtraInfo` marker on every event this module injects ("PULS").
const INJECT_TAG: usize = 0x5055_4C53;

static MAC_SHORTCUTS: AtomicBool = AtomicBool::new(true);
static SCREENSHOT_KEYS: AtomicBool = AtomicBool::new(true);

// Physical modifier state, maintained from non-injected events on the hook thread.
static CTRL_DOWN: AtomicBool = AtomicBool::new(false);
static SHIFT_DOWN: AtomicBool = AtomicBool::new(false);
static WIN_DOWN: AtomicBool = AtomicBool::new(false);
static ALT_VK: AtomicU32 = AtomicU32::new(VK_LMENU);
static SHIFT_VK: AtomicU32 = AtomicU32::new(VK_LSHIFT);
/// Keys whose press we swallowed and whose release (and auto-repeat) must be swallowed too.
static SWALLOWED: [AtomicBool; 256] = [const { AtomicBool::new(false) }; 256];

/// An Alt+key press offered to the extra handler.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // read by handlers registered through `set_extra_handler`
pub struct Chord {
    pub vk: u32,
    pub shift: bool,
    /// Auto-repeat of a key already reported (the handler decided about it before).
    pub repeat: bool,
}

pub type ExtraHandler = fn(Chord) -> bool;

static EXTRA: OnceLock<ExtraHandler> = OnceLock::new();

/// Registers the one extra Alt-chord handler (first registration wins). Must be called
/// before `start`; the handler runs on the hook thread and must return immediately.
#[allow(dead_code)] // hook point: the LocalSend/send module registers here
pub fn set_extra_handler(handler: ExtraHandler) -> bool {
    EXTRA.set(handler).is_ok()
}

pub struct Keys {
    thread_id: u32,
    join: Option<JoinHandle<()>>,
}

impl Drop for Keys {
    fn drop(&mut self) {
        // The loop ends on WM_QUIT and then removes the hook.
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Starts the hook thread when at least one feature is on. None when nothing is enabled or
/// the hook could not be installed (reported).
pub fn start(mac_shortcuts: bool, screenshot_keys: bool) -> Option<Keys> {
    if !mac_shortcuts && !screenshot_keys {
        return None;
    }
    MAC_SHORTCUTS.store(mac_shortcuts, Ordering::Relaxed);
    SCREENSHOT_KEYS.store(screenshot_keys, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel::<Option<u32>>();
    let join = thread::Builder::new()
        .name("pulse-keys".to_string())
        .spawn(move || hook_thread(&tx))
        .ok()?;
    match rx.recv() {
        Ok(Some(thread_id)) => Some(Keys {
            thread_id,
            join: Some(join),
        }),
        _ => {
            let _ = join.join();
            None
        }
    }
}

fn hook_thread(ready: &mpsc::Sender<Option<u32>>) {
    let module = match unsafe { GetModuleHandleW(None) } {
        Ok(module) => module,
        Err(error) => {
            diag::win32_error("GetModuleHandleW", &error, "keys");
            let _ = ready.send(None);
            return;
        }
    };
    // SAFETY: `hook_proc` has the HOOKPROC signature and lives for the whole process.
    let hook = match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), Some(module.into()), 0) } {
        Ok(hook) => hook,
        Err(error) => {
            diag::win32_error("SetWindowsHookExW", &error, "keys");
            let _ = ready.send(None);
            return;
        }
    };
    let mut message = MSG::default();
    // Forces creation of this thread's message queue before the id is published, so
    // `Keys::drop` can always post WM_QUIT.
    let _ = unsafe { PeekMessageW(&mut message, None, WM_USER, WM_USER, PM_NOREMOVE) };
    let _ = ready.send(Some(unsafe { GetCurrentThreadId() }));
    // The hook callback is invoked from inside GetMessageW; nothing here is dispatched.
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {}
    if let Err(error) = unsafe { UnhookWindowsHookEx(hook) } {
        diag::win32_error("UnhookWindowsHookEx", &error, "keys");
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: for WH_KEYBOARD_LL with a non-negative code, `lparam` points at a
        // KBDLLHOOKSTRUCT that is valid for the duration of this call.
        let event = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let message = wparam.0 as u32;
        let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
        let up = matches!(message, WM_KEYUP | WM_SYSKEYUP);
        if (down || up) && handle(event, down) {
            return LRESULT(1);
        }
    }
    // SAFETY: forwards the unchanged hook arguments.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

enum Plan {
    /// Inject Ctrl+<key>.
    CtrlKey(u32),
    Screenshot(shot::Request),
}

fn modifier_state(vk: u32) -> Option<(&'static AtomicBool, bool)> {
    match vk {
        VK_LCONTROL | VK_RCONTROL | VK_CONTROL => Some((&CTRL_DOWN, false)),
        VK_LSHIFT | VK_RSHIFT | VK_SHIFT => Some((&SHIFT_DOWN, true)),
        VK_LWIN | VK_RWIN => Some((&WIN_DOWN, false)),
        _ => None,
    }
}

/// True to swallow the event.
fn handle(event: &KBDLLHOOKSTRUCT, down: bool) -> bool {
    if event.flags.0 & LLKHF_INJECTED.0 != 0 || event.dwExtraInfo == INJECT_TAG {
        return false;
    }
    let vk = event.vkCode;
    if vk >= 256 {
        return false;
    }
    // Alt: remember which physical key to restore; never swallowed.
    if matches!(vk, VK_LMENU | VK_RMENU | VK_MENU) {
        if down && vk != VK_MENU {
            ALT_VK.store(vk, Ordering::Relaxed);
        }
        return false;
    }
    if let Some((flag, is_shift)) = modifier_state(vk) {
        flag.store(down, Ordering::Relaxed);
        if down && is_shift && vk != VK_SHIFT {
            SHIFT_VK.store(vk, Ordering::Relaxed);
        }
        return false;
    }
    let swallowed = &SWALLOWED[vk as usize];
    if !down {
        return swallowed.swap(false, Ordering::Relaxed);
    }

    // Screenshot session: Esc cancels, Space switches region/window mode.
    if (vk == VK_ESCAPE || vk == VK_SPACE) && shot::active() {
        if !swallowed.swap(true, Ordering::Relaxed) {
            if vk == VK_ESCAPE {
                shot::key_escape();
            } else {
                shot::key_space();
            }
        }
        return true;
    }

    let alt = event.flags.0 & LLKHF_ALTDOWN.0 != 0;
    let repeat = swallowed.load(Ordering::Relaxed);
    if !alt || CTRL_DOWN.load(Ordering::Relaxed) || WIN_DOWN.load(Ordering::Relaxed) {
        // Alt was released while a swallowed key is still auto-repeating: keep it away
        // from the application.
        return repeat;
    }
    let shift = SHIFT_DOWN.load(Ordering::Relaxed);
    let mac = MAC_SHORTCUTS.load(Ordering::Relaxed);
    let shots = SCREENSHOT_KEYS.load(Ordering::Relaxed);
    let plan = match vk {
        VK_A | VK_C | VK_V | VK_X if mac && !shift => Some(Plan::CtrlKey(vk)),
        VK_Z if mac && !shift => Some(Plan::CtrlKey(VK_Z)),
        VK_Z if mac => Some(Plan::CtrlKey(VK_Y)),
        VK_4 if shots && shift => Some(Plan::Screenshot(shot::Request::Region)),
        VK_5 if shots && shift => Some(Plan::Screenshot(shot::Request::Toolbar)),
        _ => None,
    };
    match plan {
        Some(Plan::CtrlKey(target)) => {
            swallowed.store(true, Ordering::Relaxed);
            send_ctrl_chord(target, shift);
            true
        }
        Some(Plan::Screenshot(request)) => {
            if !swallowed.swap(true, Ordering::Relaxed) {
                // Alt+Shift pressed with a key in between: no menu, no layout switch.
                send(&[key(VK_DUMMY, false), key(VK_DUMMY, true)]);
                shot::request(request);
            }
            true
        }
        None => {
            let claimed = EXTRA
                .get()
                .is_some_and(|handler| handler(Chord { vk, shift, repeat }));
            if claimed {
                swallowed.store(true, Ordering::Relaxed);
                if !repeat {
                    send(&[key(VK_DUMMY, false), key(VK_DUMMY, true)]);
                }
            }
            claimed || repeat
        }
    }
}

fn key(vk: u32, up: bool) -> INPUT {
    let mut flags = if up {
        KEYEVENTF_KEYUP
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
    if matches!(vk, VK_RMENU | VK_RCONTROL) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk as u16),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECT_TAG,
            },
        },
    }
}

fn send(events: &[INPUT]) {
    // SAFETY: `events` is a valid slice of fully initialised INPUT structures.
    let sent = unsafe { SendInput(events, size_of::<INPUT>() as i32) };
    if sent as usize != events.len() {
        diag::last_error("SendInput", "keys");
    }
}

/// Ctrl+`target` while the user physically holds Alt (and possibly Shift). Order matters:
/// see the module comment.
fn send_ctrl_chord(target: u32, shift: bool) {
    let alt = ALT_VK.load(Ordering::Relaxed);
    let shift_vk = SHIFT_VK.load(Ordering::Relaxed);
    let mut events = Vec::with_capacity(14);
    events.push(key(VK_DUMMY, false));
    events.push(key(VK_DUMMY, true));
    events.push(key(alt, true));
    if shift {
        events.push(key(shift_vk, true));
    }
    events.push(key(VK_LCONTROL, false));
    events.push(key(target, false));
    events.push(key(target, true));
    events.push(key(VK_LCONTROL, true));
    if shift {
        events.push(key(shift_vk, false));
    }
    events.push(key(alt, false));
    events.push(key(VK_DUMMY, false));
    events.push(key(VK_DUMMY, true));
    send(&events);
}
