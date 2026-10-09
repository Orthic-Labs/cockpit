//! Win32 plumbing for nearby sharing (`send.rs`): the named events that wake the notch and the
//! hub, the clipboard (files, image, text), `WM_DROPFILES`, and the Ctrl+V hot key. Raw
//! declarations (user32, kernel32, shell32) so no `windows` crate feature has to change; each
//! `unsafe` block says what it relies on.

use std::ffi::c_void;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

type Handle = *mut c_void;

const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const CF_DIB: u32 = 8;
const GMEM_MOVEABLE: u32 = 0x0002;
const WAIT_OBJECT_0: u32 = 0;
const MOD_CONTROL: u32 = 0x0002;
const MOD_NOREPEAT: u32 = 0x4000;
const VK_V: u32 = 0x56;
/// Largest clipboard payload read (text or bitmap); anything bigger is left alone.
const MAX_CLIPBOARD: usize = 256 * 1024 * 1024;

#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}

#[allow(non_snake_case)]
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
    fn GlobalAlloc(flags: u32, bytes: usize) -> Handle;
    fn GlobalFree(memory: Handle) -> Handle;
    fn GlobalLock(memory: Handle) -> *mut c_void;
    fn GlobalUnlock(memory: Handle) -> i32;
    fn GlobalSize(memory: Handle) -> usize;
}

#[allow(non_snake_case)]
#[link(name = "user32")]
unsafe extern "system" {
    fn OpenClipboard(owner: Handle) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn GetClipboardData(format: u32) -> Handle;
    fn SetClipboardData(format: u32, memory: Handle) -> Handle;
    fn IsClipboardFormatAvailable(format: u32) -> i32;
    fn RegisterClipboardFormatW(name: *const u16) -> u32;
    fn RegisterHotKey(window: Handle, id: i32, modifiers: u32, key: u32) -> i32;
    fn UnregisterHotKey(window: Handle, id: i32) -> i32;
    fn PostMessageW(window: Handle, message: u32, wparam: usize, lparam: isize) -> i32;
}

#[allow(non_snake_case)]
#[link(name = "shell32")]
unsafe extern "system" {
    fn DragAcceptFiles(window: Handle, accept: i32);
    fn DragQueryFileW(drop: Handle, index: u32, buffer: *mut u16, length: u32) -> u32;
    fn DragQueryPoint(drop: Handle, point: *mut Point) -> i32;
    fn DragFinish(drop: Handle);
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---- named events --------------------------------------------------------------------------

/// Opens (or creates) the auto-reset event `Local\<name>`. The hub creates the same events
/// (hub/src-tauri/src/share.rs), whichever side comes first. Zero when it cannot.
pub fn open_event(name: &str) -> usize {
    let name = wide(&format!("Local\\{name}"));
    // SAFETY: NUL-terminated name; auto-reset, initially not signaled.
    unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) as usize }
}

/// Signals `Local\<name>` and lets go of it.
pub fn signal(name: &str) {
    let event = open_event(name);
    if event != 0 {
        // SAFETY: a live event handle, closed right after.
        unsafe {
            SetEvent(event as Handle);
            CloseHandle(event as Handle);
        }
    }
}

/// Waits up to `milliseconds` for an event from `open_event`; true when it was signaled.
/// A zero handle just sleeps, so a failed open still paces the caller.
pub fn wait(event: usize, milliseconds: u32) -> bool {
    if event == 0 {
        std::thread::sleep(std::time::Duration::from_millis(u64::from(milliseconds)));
        return false;
    }
    // SAFETY: a live event handle owned by the caller.
    unsafe { WaitForSingleObject(event as Handle, milliseconds) == WAIT_OBJECT_0 }
}

pub fn close(event: usize) {
    if event != 0 {
        // SAFETY: a handle from `open_event`, closed once.
        unsafe { CloseHandle(event as Handle) };
    }
}

/// Posts `message` (no parameters) to a window from any thread.
pub fn post_message(window: isize, message: u32) {
    if window != 0 {
        // SAFETY: PostMessageW tolerates a stale handle (it just fails).
        unsafe { PostMessageW(window as Handle, message, 0, 0) };
    }
}

// ---- hot key ------------------------------------------------------------------------------

/// Takes Ctrl+V for `window` (the thread that made it must be the caller). False when another
/// program already holds the combination.
pub fn register_paste_key(window: isize, id: i32) -> bool {
    // SAFETY: plain call; failure is reported by the return value.
    unsafe { RegisterHotKey(window as Handle, id, MOD_CONTROL | MOD_NOREPEAT, VK_V) != 0 }
}

pub fn unregister_paste_key(window: isize, id: i32) {
    // SAFETY: plain call; unregistering an id that is not registered just fails.
    unsafe { UnregisterHotKey(window as Handle, id) };
}

// ---- file drops ---------------------------------------------------------------------------

pub fn accept_drops(window: isize) {
    // SAFETY: plain call on a window handle; a stale handle just does nothing.
    unsafe { DragAcceptFiles(window as Handle, 1) };
}

/// The files of a `WM_DROPFILES` (its `wparam` is `hdrop`), the drop point in client pixels,
/// and releases the drop. Call once per message.
pub fn take_drop(hdrop: isize) -> (Vec<PathBuf>, (i32, i32)) {
    let handle = hdrop as Handle;
    let mut files = Vec::new();
    let mut point = Point { x: 0, y: 0 };
    // SAFETY: `handle` is the live HDROP delivered with WM_DROPFILES; every buffer is sized
    // from the length the shell reports; DragFinish releases it exactly once at the end.
    unsafe {
        DragQueryPoint(handle, &mut point);
        let count = DragQueryFileW(handle, u32::MAX, std::ptr::null_mut(), 0);
        for index in 0..count {
            let length = DragQueryFileW(handle, index, std::ptr::null_mut(), 0) as usize;
            if length == 0 {
                continue;
            }
            let mut buffer = vec![0u16; length + 1];
            let copied =
                DragQueryFileW(handle, index, buffer.as_mut_ptr(), buffer.len() as u32) as usize;
            if copied > 0 && copied <= length {
                files.push(PathBuf::from(std::ffi::OsString::from_wide(
                    &buffer[..copied],
                )));
            }
        }
        DragFinish(handle);
    }
    (files, (point.x, point.y))
}

// ---- clipboard ----------------------------------------------------------------------------

pub enum Clip {
    Files(Vec<PathBuf>),
    /// A picture, already written to a file in the temp folder.
    Image(PathBuf),
    Text(String),
    Empty,
}

/// Holds the clipboard open; closes it when dropped.
struct Open;

impl Open {
    fn acquire() -> Option<Open> {
        // Another program may hold the clipboard for a moment; try for about a quarter second.
        for _ in 0..10 {
            // SAFETY: a null owner associates the open clipboard with the current task.
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Some(Open);
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        None
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: balances the successful OpenClipboard in `acquire`.
        unsafe { CloseClipboard() };
    }
}

/// What the clipboard holds, in the Mac's order: files, a picture, text.
pub fn read_clipboard() -> Clip {
    let Some(_open) = Open::acquire() else {
        return Clip::Empty;
    };
    // SAFETY: every handle below comes from the open clipboard and is only read, never freed.
    unsafe {
        if IsClipboardFormatAvailable(CF_HDROP) != 0 {
            let drop = GetClipboardData(CF_HDROP);
            if !drop.is_null() {
                let count = DragQueryFileW(drop, u32::MAX, std::ptr::null_mut(), 0);
                let mut files = Vec::new();
                for index in 0..count {
                    let length = DragQueryFileW(drop, index, std::ptr::null_mut(), 0) as usize;
                    if length == 0 {
                        continue;
                    }
                    let mut buffer = vec![0u16; length + 1];
                    let copied =
                        DragQueryFileW(drop, index, buffer.as_mut_ptr(), buffer.len() as u32)
                            as usize;
                    if copied > 0 && copied <= length {
                        files.push(PathBuf::from(std::ffi::OsString::from_wide(
                            &buffer[..copied],
                        )));
                    }
                }
                if !files.is_empty() {
                    return Clip::Files(files);
                }
            }
        }
        if let Some(bytes) = image_bytes()
            && let Some(path) = save_image(&bytes.0, bytes.1)
        {
            return Clip::Image(path);
        }
        if IsClipboardFormatAvailable(CF_UNICODETEXT) != 0 {
            let memory = GetClipboardData(CF_UNICODETEXT);
            if !memory.is_null() {
                let size = GlobalSize(memory).min(MAX_CLIPBOARD);
                let pointer = GlobalLock(memory) as *const u16;
                if !pointer.is_null() {
                    let units = size / 2;
                    let mut length = 0usize;
                    while length < units && *pointer.add(length) != 0 {
                        length += 1;
                    }
                    let text =
                        String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length));
                    GlobalUnlock(memory);
                    if !text.is_empty() {
                        return Clip::Text(text);
                    }
                }
            }
        }
    }
    Clip::Empty
}

/// A picture on the open clipboard: the registered "PNG" format when an app offers it (file
/// extension `png`), otherwise the DIB as a complete `.bmp` file (`bmp`).
///
/// # Safety
/// The clipboard must be open on this thread.
unsafe fn image_bytes() -> Option<(Vec<u8>, &'static str)> {
    // SAFETY: caller contract; the locked pointers are read within the sizes the system gives.
    unsafe {
        let png_format = RegisterClipboardFormatW(wide("PNG").as_ptr());
        if png_format != 0 && IsClipboardFormatAvailable(png_format) != 0 {
            let memory = GetClipboardData(png_format);
            if !memory.is_null() {
                let size = GlobalSize(memory);
                let pointer = GlobalLock(memory) as *const u8;
                if !pointer.is_null() && size > 0 && size <= MAX_CLIPBOARD {
                    let bytes = std::slice::from_raw_parts(pointer, size).to_vec();
                    GlobalUnlock(memory);
                    return Some((bytes, "png"));
                }
                if !pointer.is_null() {
                    GlobalUnlock(memory);
                }
            }
        }
        if IsClipboardFormatAvailable(CF_DIB) == 0 {
            return None;
        }
        let memory = GetClipboardData(CF_DIB);
        if memory.is_null() {
            return None;
        }
        let size = GlobalSize(memory);
        let pointer = GlobalLock(memory) as *const u8;
        if pointer.is_null() {
            return None;
        }
        let dib = if (40..=MAX_CLIPBOARD).contains(&size) {
            Some(std::slice::from_raw_parts(pointer, size).to_vec())
        } else {
            None
        };
        GlobalUnlock(memory);
        dib.and_then(|dib| bmp_file(&dib)).map(|file| (file, "bmp"))
    }
}

/// A clipboard DIB (BITMAPINFOHEADER and pixels) with the 14-byte file header put in front.
fn bmp_file(dib: &[u8]) -> Option<Vec<u8>> {
    let u32_at = |at: usize| {
        dib.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let header = u32_at(0)? as usize;
    if !(40..=dib.len()).contains(&header) {
        return None;
    }
    let bits = dib.get(14..16).map(|b| u16::from_le_bytes([b[0], b[1]]))? as u32;
    let compression = u32_at(16)?;
    let used = u32_at(32)?;
    let colours: usize = if used > 0 {
        used as usize
    } else if bits <= 8 {
        1usize << bits
    } else {
        0
    };
    // BI_BITFIELDS (3) and BI_ALPHABITFIELDS (6) put the channel masks after a 40-byte header.
    let masks = if header == 40 {
        match compression {
            3 => 12,
            6 => 16,
            _ => 0,
        }
    } else {
        0
    };
    let offset = 14 + header + masks + colours * 4;
    if offset > 14 + dib.len() {
        return None;
    }
    let total = 14 + dib.len();
    let mut file = Vec::with_capacity(total);
    file.extend_from_slice(b"BM");
    file.extend_from_slice(&(total as u32).to_le_bytes());
    file.extend_from_slice(&[0, 0, 0, 0]);
    file.extend_from_slice(&(offset as u32).to_le_bytes());
    file.extend_from_slice(dib);
    Some(file)
}

fn save_image(bytes: &[u8], extension: &str) -> Option<PathBuf> {
    let folder = std::env::temp_dir().join("Pulse Clipboard");
    std::fs::create_dir_all(&folder).ok()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = folder.join(format!("Clipboard {stamp}.{extension}"));
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

/// Replaces the clipboard with `paths` as a file drop (`CF_HDROP`), so Explorer pastes them.
pub fn write_files(paths: &[PathBuf]) -> bool {
    let Some(_open) = Open::acquire() else {
        return false;
    };
    let mut units: Vec<u16> = Vec::new();
    for path in paths {
        units.extend(path.as_os_str().encode_wide());
        units.push(0);
    }
    units.push(0);
    // DROPFILES: offset of the list, a point, two flags (the last: wide characters).
    const HEADER: usize = 20;
    let bytes = HEADER + units.len() * 2;
    // SAFETY: the allocation is filled before it is handed over; ownership passes to the
    // clipboard on success and is freed here only when SetClipboardData fails.
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if memory.is_null() {
            return false;
        }
        let pointer = GlobalLock(memory) as *mut u8;
        if pointer.is_null() {
            GlobalFree(memory);
            return false;
        }
        std::ptr::write_bytes(pointer, 0, HEADER);
        pointer.cast::<u32>().write_unaligned(HEADER as u32);
        pointer.add(16).cast::<u32>().write_unaligned(1);
        std::ptr::copy_nonoverlapping(
            units.as_ptr().cast::<u8>(),
            pointer.add(HEADER),
            units.len() * 2,
        );
        GlobalUnlock(memory);
        EmptyClipboard();
        if SetClipboardData(CF_HDROP, memory).is_null() {
            GlobalFree(memory);
            return false;
        }
    }
    true
}

/// Replaces the clipboard with `text`. False when the clipboard could not be taken.
pub fn write_text(text: &str) -> bool {
    let Some(_open) = Open::acquire() else {
        return false;
    };
    let units: Vec<u16> = std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let bytes = units.len() * 2;
    // SAFETY: the allocation is filled before it is handed over; ownership passes to the
    // clipboard on success and is freed here only when SetClipboardData fails.
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if memory.is_null() {
            return false;
        }
        let pointer = GlobalLock(memory) as *mut u16;
        if pointer.is_null() {
            GlobalFree(memory);
            return false;
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), pointer, units.len());
        GlobalUnlock(memory);
        EmptyClipboard();
        if SetClipboardData(CF_UNICODETEXT, memory).is_null() {
            GlobalFree(memory);
            return false;
        }
    }
    true
}
