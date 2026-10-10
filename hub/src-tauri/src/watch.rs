//! Live refresh for the Storage index. While the hub runs, FSEvents reports the
//! folders that change under the scanned root. Each changed folder that the
//! index holds is read again (`scanner::refresh_subtree`) and the index is
//! updated in place; the page hears about it through the `storage-updated`
//! event. FSEvents reports per folder ("something in here changed"), and macOS
//! coalesces busy seconds into one delivery.
//!
//! Adapted from Petal's `src/watch.rs` (MIT, Copyright (c) 2026 Henry Dennis;
//! see `docs/donors.md`). Petal hands changes to its GPUI window; this version
//! hands them to the hub's scanner. On Windows the same updates come from
//! `ReadDirectoryChangesW` (`watch_windows.rs`).

#[cfg(target_os = "macos")]
pub use mac::{current_event_id, live_refresh, start, stop};

/// On Windows, `ReadDirectoryChangesW` feeds the same index updates (see
/// `watch_windows.rs`).
#[cfg(windows)]
pub use win::{current_event_id, live_refresh, start, stop};

/// Elsewhere there is no change source; the index is refreshed by the next scan.
#[cfg(not(any(target_os = "macos", windows)))]
pub use fallback::{current_event_id, live_refresh, start, stop};

#[cfg(windows)]
#[path = "watch_windows.rs"]
mod win;

#[cfg(not(any(target_os = "macos", windows)))]
mod fallback {
    use std::path::PathBuf;

    use tauri::AppHandle;

    pub fn current_event_id() -> u64 {
        0
    }

    pub fn live_refresh() -> bool {
        false
    }

    pub fn start(_app: AppHandle, _root: PathBuf, _since: u64, _epoch: u64) {
        crate::scanner::log("live refresh unavailable: no change journal on this platform");
    }

    pub fn stop() {}
}

#[cfg(target_os = "macos")]
mod mac {
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{AppHandle, Emitter};

use crate::scanner::{self, UPDATED_EVENT, Updated};

/// How often the changes FSEvents collected are read and applied.
const POLL: Duration = Duration::from_millis(1_000);

/// Generation of the running watch. `stop`, or a newer `start`, moves it on,
/// and the older watch then ends.
static GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct Pending {
    /// Folders FSEvents reported as changed since the last take.
    changes: Vec<PathBuf>,
    /// FSEvents lost track (the root moved, or events were dropped): nothing it
    /// reports can be trusted any more.
    lost: bool,
}

/// FSEvents is always available; the index is kept current while the hub runs.
pub fn live_refresh() -> bool {
    true
}

/// The FSEvents position now. Taken before a scan, so changes made during the
/// scan are replayed by the watch that starts after it.
pub fn current_event_id() -> u64 {
    unsafe { FSEventsGetCurrentEventId() }
}

/// A running FSEvents stream; stops when dropped.
struct Watch {
    stream: FSEventStreamRef,
    queue: DispatchQueue,
    pending: *const Mutex<Pending>,
}

impl Watch {
    /// Watch `root` and everything below it, replaying changes since `since`.
    fn start(root: &Path, since: u64) -> Option<Watch> {
        let c_root = CString::new(root.as_os_str().as_encoded_bytes()).ok()?;
        let pending = Arc::into_raw(Arc::new(Mutex::new(Pending::default())));
        unsafe {
            let path = CFStringCreateWithCString(std::ptr::null(), c_root.as_ptr(), K_CF_STRING_ENCODING_UTF8);
            let values = [path];
            let paths = CFArrayCreate(std::ptr::null(), values.as_ptr(), 1, &kCFTypeArrayCallBacks);
            let mut context = FSEventStreamContext {
                version: 0,
                info: pending as *mut c_void,
                retain: std::ptr::null(),
                release: std::ptr::null(),
                copy_description: std::ptr::null(),
            };
            let stream = FSEventStreamCreate(
                std::ptr::null(),
                callback,
                &mut context,
                paths,
                since,
                LATENCY_SECONDS,
                FLAG_NONE,
            );
            CFRelease(paths);
            CFRelease(path);
            if stream.is_null() {
                drop(Arc::from_raw(pending));
                return None;
            }
            let queue = dispatch_queue_create(c"pulse.watch".as_ptr(), std::ptr::null());
            FSEventStreamSetDispatchQueue(stream, queue);
            if FSEventStreamStart(stream) == 0 {
                FSEventStreamInvalidate(stream);
                FSEventStreamRelease(stream);
                dispatch_release(queue);
                drop(Arc::from_raw(pending));
                return None;
            }
            Some(Watch { stream, queue, pending })
        }
    }

    /// Changes reported since the last call.
    fn take(&self) -> Pending {
        let pending = unsafe { &*self.pending };
        std::mem::take(&mut *pending.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        unsafe {
            FSEventStreamStop(self.stream);
            // After this returns, the callback won't run again.
            FSEventStreamInvalidate(self.stream);
            FSEventStreamRelease(self.stream);
            dispatch_release(self.queue);
            drop(Arc::from_raw(self.pending));
        }
    }
}

/// Batching delay: macOS coalesces the events of a busy second into one delivery.
const LATENCY_SECONDS: f64 = 0.5;

extern "C" fn callback(
    _stream: FSEventStreamRef,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    _ids: *const u64,
) {
    let pending = unsafe { &*(info as *const Mutex<Pending>) };
    let paths = unsafe { std::slice::from_raw_parts(paths as *const *const c_char, count) };
    let flags = unsafe { std::slice::from_raw_parts(flags, count) };
    let Ok(mut pending) = pending.lock() else {
        return;
    };
    for (&path, &flag) in paths.iter().zip(flags) {
        if flag & (EVENT_HISTORY_DONE | EVENT_IDS_WRAPPED) != 0
            && flag & !(EVENT_HISTORY_DONE | EVENT_IDS_WRAPPED) == 0
        {
            continue;
        }
        if flag & (EVENT_ROOT_CHANGED | EVENT_USER_DROPPED | EVENT_KERNEL_DROPPED) != 0 {
            pending.lost = true;
            continue;
        }
        let text = unsafe { CStr::from_ptr(path) }.to_string_lossy();
        let trimmed = text.trim_end_matches('/');
        pending.changes.push(PathBuf::from(if trimmed.is_empty() { "/" } else { trimmed }));
    }
}

/// Start the live refresh of the index that `epoch` names, replaying FSEvents
/// changes from `since`. Ends when `stop` is called or a newer watch starts.
pub fn start(app: AppHandle, root: PathBuf, since: u64, epoch: u64) {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let spawned = std::thread::Builder::new()
        .name("pulse-watch".into())
        .spawn(move || follow(app, &root, since, epoch, generation));
    if let Err(error) = spawned {
        scanner::log(&format!("live refresh did not start: {error}"));
    }
}

/// Stop the running watch. A new scan calls this before it replaces the index.
pub fn stop() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// The watch loop: every poll, read the folders that changed in the index and
/// refresh them. The `Watch` lives on this thread only (it is not `Send`).
fn follow(app: AppHandle, root: &Path, since: u64, epoch: u64, generation: u64) {
    let Some(watch) = Watch::start(root, since) else {
        scanner::log("live refresh unavailable: FSEvents did not start");
        return;
    };
    while GENERATION.load(Ordering::SeqCst) == generation {
        std::thread::sleep(POLL);
        if GENERATION.load(Ordering::SeqCst) != generation {
            break;
        }
        let pending = watch.take();
        if pending.lost {
            scanner::log("live refresh stopped: FSEvents lost track of the scanned folder");
            let _ = app.emit(UPDATED_EVENT, Updated { folders: Vec::new(), stale: true });
            break;
        }
        // Changes wait in the scanner while the index is not in memory.
        let applied = scanner::apply_changes(pending.changes, epoch);
        if !applied.folders.is_empty() || applied.stale {
            let _ = app.emit(
                UPDATED_EVENT,
                Updated {
                    folders: applied.folders,
                    stale: applied.stale,
                },
            );
        }
        scanner::save_if_due();
    }
}

// --- FFI (CoreServices / CoreFoundation / libdispatch) -----------------------------

type FSEventStreamRef = *mut c_void;
type DispatchQueue = *mut c_void;
type CFTypeRef = *const c_void;

#[repr(C)]
#[allow(dead_code)]
struct FSEventStreamContext {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const FLAG_NONE: u32 = 0;
const EVENT_USER_DROPPED: u32 = 0x0002;
const EVENT_KERNEL_DROPPED: u32 = 0x0004;
const EVENT_IDS_WRAPPED: u32 = 0x0008;
const EVENT_HISTORY_DONE: u32 = 0x0010;
const EVENT_ROOT_CHANGED: u32 = 0x0020;

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventsGetCurrentEventId() -> u64;
    fn FSEventStreamCreate(
        allocator: *const c_void,
        callback: extern "C" fn(FSEventStreamRef, *mut c_void, usize, *mut c_void, *const u32, *const u64),
        context: *mut FSEventStreamContext,
        paths: CFTypeRef,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> FSEventStreamRef;
    fn FSEventStreamSetDispatchQueue(stream: FSEventStreamRef, queue: DispatchQueue);
    fn FSEventStreamStart(stream: FSEventStreamRef) -> u8;
    fn FSEventStreamStop(stream: FSEventStreamRef);
    fn FSEventStreamInvalidate(stream: FSEventStreamRef);
    fn FSEventStreamRelease(stream: FSEventStreamRef);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeArrayCallBacks: c_void;
    fn CFStringCreateWithCString(allocator: *const c_void, text: *const c_char, encoding: u32) -> CFTypeRef;
    fn CFArrayCreate(allocator: *const c_void, values: *const CFTypeRef, count: isize, callbacks: *const c_void) -> CFTypeRef;
    fn CFRelease(object: CFTypeRef);
}

unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> DispatchQueue;
    fn dispatch_release(object: DispatchQueue);
}
}
