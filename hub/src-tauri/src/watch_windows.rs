//! Live refresh of the Storage index on Windows. `ReadDirectoryChangesW` reports,
//! recursively, every file or folder that is created, removed, renamed or written
//! under the scanned root. The reports are folded into the set of folders whose
//! contents changed, and that set is handed to `scanner::apply_changes`, which
//! reads those folders again and updates the index in place, exactly as it does
//! for the FSEvents folders on macOS (see `watch.rs`).
//!
//! Why not the NTFS change journal (USN): `FSCTL_READ_USN_JOURNAL` needs a handle
//! to the volume (`\\.\C:`) opened for reading, which Windows refuses to a process
//! that is not elevated. The hub runs as the user, so the per-directory watch is
//! the source that works without administrator rights. It needs no volume handle,
//! no journal and no setup, and it never leaves the scanned root.
//!
//! Two threads. The reader (`pulse-watch-read`) keeps one overlapped request
//! pending and drains it at once, so a slow re-read of a folder never lets the
//! kernel's buffer fill up and drop events. The applier (`pulse-watch`) takes the
//! folders the reader collected every `POLL` and applies them, at background
//! priority so the re-reads stay out of the way of the user's apps.
//!
//! How events become re-reads:
//!  * created, removed or renamed entry: the folder that holds it changed, so that
//!    folder is re-read (a new entry has to be added to its parent's rows);
//!  * written file: the folder that holds it is re-read, for its new size;
//!  * "written" folder: ignored. A folder's own write time moves when an entry is
//!    created or removed in it, and that entry has its own report. Re-reading the
//!    folder's parent for it would re-read a much larger subtree for nothing;
//!  * the hub's own state folder, and the profile's `NTUSER.DAT*` registry hive
//!    files (written all day), are ignored.
//! `scanner::apply_changes` maps each folder to the nearest one the index holds,
//! drops folders inside another listed folder, and keeps its own budgets: it reads
//! the folders in order of priority (a folder that keeps changing is read less and
//! less often), stops starting reads after a time budget per pass, tells the page
//! as soon as each folder's numbers change, and bounds every read in entries
//! (`LIVE_MAX_ENTRIES`) and time. A folder too large to read again (the home root,
//! for one) is never read whole: a folder created or removed directly in it is
//! attached or detached on its own.
//!
//! Lost events. When the kernel buffer overflows, `ReadDirectoryChangesW` completes
//! with zero bytes (or `ERROR_NOTIFY_ENUM_DIR`), and nothing says what changed. The
//! scanner is then told events were lost: it re-reads the root once, bounded (at
//! most `LIVE_MAX_ENTRIES` entries; a root that is larger is not read, and the page
//! is told the index is stale), at most once per `OVERFLOW_REWALK_GAP`; inside that
//! gap the page is only told the index is stale. The watch keeps running, because
//! events after the overflow are valid.
//! If the request itself fails (the root went away, the volume was dismounted) the
//! watch ends and the page is told the index is stale, as on macOS.
//!
//! Junctions and links are never followed: the scanner does not index below them,
//! and `ReadDirectoryChangesW` does not report changes inside a mount point or
//! junction. A scan root that is itself a reparse point is not watched.
//!
//! Not replayed: changes made while the first walk was running (Windows has no
//! event id to replay from; `since` is unused). They show on the next re-read of
//! the folders concerned or the next scan.

use std::collections::BTreeSet;
use std::ffi::c_void;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};

use crate::scanner::{self, UPDATED_EVENT, Updated};

/// How often the folders the reader collected are applied. The scanner holds back
/// folders that keep changing, so a short poll costs little, and it is most of the
/// wait for a change in a folder that was quiet.
const POLL: Duration = Duration::from_millis(1_000);
/// Most folders collected between two applications; a burst beyond this (an
/// install, a checkout) is treated like a buffer overflow.
const MAX_DIRS: usize = 20_000;
/// A buffer overflow queues the root for a bounded re-read at most this often.
const OVERFLOW_REWALK_GAP: Duration = Duration::from_secs(300);
/// `symlink_metadata` calls made for "written" reports in one batch; past this,
/// the holding folder is simply re-read.
const STAT_BUDGET: usize = 4_096;
/// Kernel buffer for one request, in `u32`s (256 KiB). Network shares only
/// accept 64 KiB, so a refused request is retried with `SMALL_BUFFER`.
const BUFFER_WORDS: usize = 64 * 1024;
const SMALL_BUFFER: usize = 16 * 1024;
/// How long the reader waits for a report before it looks at `GENERATION` again.
const WAIT_MS: u32 = 500;

/// Generation of the running watch. `stop`, or a newer `start`, moves it on, and
/// the older watch then ends.
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// The watch could not be opened for the current scan root.
static FAILED: AtomicBool = AtomicBool::new(false);

/// There is no event id to replay from; kept so the scanner's call is the same on
/// every platform.
pub fn current_event_id() -> u64 {
    0
}

/// Whether the index is kept current while the hub runs: true unless the watch
/// could not be opened for the last scan.
pub fn live_refresh() -> bool {
    !FAILED.load(Ordering::SeqCst)
}

#[derive(Default)]
struct Pending {
    /// Folders whose contents changed since the last take.
    dirs: BTreeSet<PathBuf>,
    /// Events were dropped (kernel buffer, or more folders than `MAX_DIRS`):
    /// what changed is unknown.
    overflow: bool,
    /// The request failed for good; nothing more will be reported.
    lost: bool,
}

/// Start the live refresh of the index that `epoch` names. Ends when `stop` is
/// called or a newer watch starts.
pub fn start(app: AppHandle, root: PathBuf, _since: u64, epoch: u64) {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let spawned = std::thread::Builder::new()
        .name("pulse-watch".into())
        .spawn(move || follow(app, root, epoch, generation));
    if let Err(error) = spawned {
        FAILED.store(true, Ordering::SeqCst);
        scanner::log(&format!("live refresh did not start: {error}"));
    }
}

/// Stop the running watch. A new scan calls this before it replaces the index.
pub fn stop() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

fn current(generation: u64) -> bool {
    GENERATION.load(Ordering::SeqCst) == generation
}

fn take(shared: &Mutex<Pending>) -> Pending {
    std::mem::take(&mut *shared.lock().unwrap_or_else(|e| e.into_inner()))
}

/// The applier loop: every `POLL`, hand the folders that changed to the scanner.
fn follow(app: AppHandle, root: PathBuf, epoch: u64, generation: u64) {
    background_priority();
    if std::fs::symlink_metadata(&root)
        .map(|m| m.file_type().is_symlink() || m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .unwrap_or(true)
    {
        FAILED.store(true, Ordering::SeqCst);
        scanner::log("live refresh unavailable: the scanned folder is a link or cannot be read");
        return;
    }
    let shared = Arc::new(Mutex::new(Pending::default()));
    let (tx, rx) = mpsc::channel();
    let reader = {
        let (shared, root) = (shared.clone(), root.clone());
        std::thread::Builder::new()
            .name("pulse-watch-read".into())
            .spawn(move || read_changes(&root, &shared, generation, &tx))
    };
    if let Err(error) = reader {
        FAILED.store(true, Ordering::SeqCst);
        scanner::log(&format!("live refresh did not start: {error}"));
        return;
    }
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => FAILED.store(false, Ordering::SeqCst),
        Ok(Err(error)) => {
            FAILED.store(true, Ordering::SeqCst);
            scanner::log(&format!("live refresh unavailable: {error}"));
            return;
        }
        Err(_) => {
            // The reader never answered; make it end whenever it wakes.
            let _ = GENERATION.compare_exchange(generation, generation + 1, Ordering::SeqCst, Ordering::SeqCst);
            FAILED.store(true, Ordering::SeqCst);
            scanner::log("live refresh unavailable: the change watch did not start");
            return;
        }
    }
    scanner::log("live refresh started (ReadDirectoryChangesW)");
    let mut last_rewalk: Option<Instant> = None;
    while current(generation) {
        std::thread::sleep(POLL);
        if !current(generation) {
            break;
        }
        let pending = take(&shared);
        if pending.lost {
            scanner::log("live refresh stopped: the change watch on the scanned folder failed");
            let _ = app.emit(UPDATED_EVENT, Updated { folders: Vec::new(), stale: true });
            break;
        }
        let changes: Vec<PathBuf> = pending.dirs.into_iter().collect();
        let mut lost = false;
        let mut lost_events = false;
        if pending.overflow {
            if last_rewalk.is_none_or(|at| at.elapsed() >= OVERFLOW_REWALK_GAP) {
                last_rewalk = Some(Instant::now());
                scanner::log("live refresh: change reports were dropped, re-reading the scanned folder (bounded)");
                lost = true;
            } else {
                lost_events = true;
            }
        }
        // Changes wait in the scanner while the index is not in memory. The page is
        // told as each folder is applied, not after the whole batch.
        scanner::apply_changes(changes, lost, epoch, &mut |updated| {
            let _ = app.emit(UPDATED_EVENT, updated);
        });
        if lost_events {
            let _ = app.emit(UPDATED_EVENT, Updated { folders: Vec::new(), stale: true });
        }
        scanner::save_if_due();
    }
}

// --- Reader ---------------------------------------------------------------------------

/// An open directory handle with one overlapped `ReadDirectoryChangesW` pending.
struct Reader {
    handle: *mut c_void,
    event: *mut c_void,
    overlapped: Box<Overlapped>,
    buffer: Vec<u32>,
    /// A request is in flight and the kernel may write to `buffer`.
    armed: bool,
}

impl Reader {
    fn open(root: &Path) -> Result<Reader, String> {
        let name: Vec<u16> = root.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        // SAFETY: NUL-terminated name; the handle is closed in `Drop`.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                FILE_LIST_DIRECTORY,
                FILE_SHARE_ALL,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if handle.is_null() || handle as isize == INVALID_HANDLE_VALUE {
            return Err(format!("the folder could not be opened ({})", std::io::Error::last_os_error()));
        }
        // SAFETY: a manual-reset, initially unsignalled, unnamed event; closed in `Drop`.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            // SAFETY: `handle` was opened above and is closed once.
            unsafe { CloseHandle(handle) };
            return Err("no event could be created".into());
        }
        Ok(Reader {
            handle,
            event,
            overlapped: Box::new(Overlapped::new(event)),
            buffer: vec![0u32; BUFFER_WORDS],
            armed: false,
        })
    }

    /// Issue the next request (the previous one must have completed).
    fn arm(&mut self) -> Result<(), std::io::Error> {
        *self.overlapped = Overlapped::new(self.event);
        let mut returned = 0u32;
        // SAFETY: `buffer` and `overlapped` are heap blocks owned by `self`; they are not
        // moved or freed while `armed` (see `Drop`). The handle is open for listing with
        // overlapped I/O, and `returned` is a live u32 (unused for overlapped calls).
        let ok = unsafe {
            ReadDirectoryChangesW(
                self.handle,
                self.buffer.as_mut_ptr().cast(),
                (self.buffer.len() * 4) as u32,
                1,
                NOTIFY_FILTER,
                &mut returned,
                &mut *self.overlapped,
                std::ptr::null(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        self.armed = true;
        Ok(())
    }

    /// The first request; a share that refuses the large buffer gets the 64 KiB one.
    fn arm_first(&mut self) -> Result<(), std::io::Error> {
        match self.arm() {
            Err(error) if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER) => {
                self.buffer = vec![0u32; SMALL_BUFFER];
                self.arm()
            }
            other => other,
        }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        if self.armed {
            // SAFETY: the request is ours; cancelling it makes the kernel finish with `buffer`.
            unsafe { CancelIoEx(self.handle, &mut *self.overlapped) };
            // SAFETY: `event` is open. The wait is bounded: if the kernel still has not
            // finished with the buffer, leak it rather than free memory it may write to.
            let done = unsafe { WaitForSingleObject(self.event, 5_000) } == WAIT_OBJECT_0;
            if !done {
                std::mem::forget(std::mem::take(&mut self.buffer));
                std::mem::forget(std::mem::replace(&mut self.overlapped, Box::new(Overlapped::new(std::ptr::null_mut()))));
            }
        }
        // SAFETY: both handles came from the calls in `open` and are closed once.
        unsafe {
            CloseHandle(self.event);
            CloseHandle(self.handle);
        }
    }
}

/// Drain change reports into `shared` until the watch is stopped or fails.
fn read_changes(root: &Path, shared: &Mutex<Pending>, generation: u64, started: &mpsc::Sender<Result<(), String>>) {
    let mut reader = match Reader::open(root) {
        Ok(reader) => reader,
        Err(error) => {
            let _ = started.send(Err(error));
            return;
        }
    };
    if let Err(error) = reader.arm_first() {
        let _ = started.send(Err(format!("the change watch was refused ({error})")));
        return;
    }
    let _ = started.send(Ok(()));
    let filter = Filter {
        root,
        skip: crate::cache::dir(),
    };
    while current(generation) {
        // SAFETY: `event` is open.
        match unsafe { WaitForSingleObject(reader.event, WAIT_MS) } {
            WAIT_OBJECT_0 => {}
            WAIT_TIMEOUT => continue,
            _ => {
                fail(shared);
                return;
            }
        }
        reader.armed = false;
        let mut bytes = 0u32;
        // SAFETY: the request on this handle and overlapped block has completed (the event
        // is signalled); `bytes` is a live u32.
        let ok = unsafe { GetOverlappedResult(reader.handle, &mut *reader.overlapped, &mut bytes, 0) };
        let mut overflow = false;
        let mut data = Vec::new();
        if ok == 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(ERROR_NOTIFY_ENUM_DIR) {
                overflow = true;
            } else {
                fail(shared);
                return;
            }
        } else if bytes == 0 {
            // The reports did not fit the kernel's buffer: nothing says what changed.
            overflow = true;
        } else {
            // SAFETY: `bytes` of `buffer` were just written by the kernel.
            let written = unsafe { std::slice::from_raw_parts(reader.buffer.as_ptr().cast::<u8>(), (bytes as usize).min(reader.buffer.len() * 4)) };
            data.extend_from_slice(written);
        }
        // Ask for the next reports before reading these, so the gap is as short as a copy.
        if reader.arm().is_err() {
            fail(shared);
            return;
        }
        let mut batch = BTreeSet::new();
        let capped = filter.parse(&data, &mut batch);
        let mut pending = shared.lock().unwrap_or_else(|e| e.into_inner());
        if overflow || capped {
            pending.overflow = true;
        }
        if !pending.overflow {
            pending.dirs.extend(batch);
            if pending.dirs.len() > MAX_DIRS {
                pending.dirs.clear();
                pending.overflow = true;
            }
        }
    }
}

fn fail(shared: &Mutex<Pending>) {
    shared.lock().unwrap_or_else(|e| e.into_inner()).lost = true;
}

/// What to ignore, and how a batch of reports becomes folders.
struct Filter<'a> {
    root: &'a Path,
    /// The hub's own state folder: its index saves would otherwise re-trigger themselves.
    skip: PathBuf,
}

impl Filter<'_> {
    /// Fold the `FILE_NOTIFY_INFORMATION` records in `data` into `batch`. Returns true
    /// when the batch grew past `MAX_DIRS` (treat as lost events).
    fn parse(&self, data: &[u8], batch: &mut BTreeSet<PathBuf>) -> bool {
        let mut offset = 0usize;
        let mut stats = 0usize;
        // The last folder already queued (or ignored), to skip its repeats without building a path.
        let mut last_parent: Vec<u16> = Vec::new();
        let mut have_last = false;
        let word = |at: usize| -> Option<u32> {
            data.get(at..at.checked_add(4)?).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        loop {
            let (Some(next), Some(action), Some(length)) = (word(offset), word(offset + 4), word(offset + 8)) else {
                break;
            };
            let name_at = offset + 12;
            let Some(raw) = data.get(name_at..name_at.saturating_add(length as usize)) else {
                break;
            };
            let name: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            if !name.is_empty() {
                self.record(action, &name, batch, &mut stats, &mut last_parent, &mut have_last);
            }
            if batch.len() > MAX_DIRS {
                return true;
            }
            if next == 0 {
                break;
            }
            offset += next as usize;
        }
        false
    }

    fn record(
        &self,
        action: u32,
        name: &[u16],
        batch: &mut BTreeSet<PathBuf>,
        stats: &mut usize,
        last_parent: &mut Vec<u16>,
        have_last: &mut bool,
    ) {
        let split = name.iter().rposition(|&unit| unit == u16::from(b'\\'));
        let (parent_units, leaf) = match split {
            Some(at) => (&name[..at], &name[at + 1..]),
            None => (&name[..0], name),
        };
        if is_registry_hive(leaf) {
            return;
        }
        if *have_last && last_parent.as_slice() == parent_units {
            return;
        }
        let parent = if parent_units.is_empty() {
            self.root.to_path_buf()
        } else {
            self.root.join(std::ffi::OsString::from_wide(parent_units))
        };
        let remember = |last: &mut Vec<u16>, have: &mut bool| {
            last.clear();
            last.extend_from_slice(parent_units);
            *have = true;
        };
        if parent.starts_with(&self.skip) {
            remember(last_parent, have_last);
            return;
        }
        if action == ACTION_MODIFIED {
            // A folder's own write time moves with its entries, which report themselves.
            let path = self.root.join(std::ffi::OsString::from_wide(name));
            if *stats < STAT_BUDGET {
                *stats += 1;
                if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_dir()) {
                    return;
                }
            }
        }
        batch.insert(parent);
        remember(last_parent, have_last);
    }
}

/// `NTUSER.DAT`, its `.LOG1`/`.LOG2` and `{guid}.TM.blf` companions: written constantly.
fn is_registry_hive(leaf: &[u16]) -> bool {
    const PREFIX: &[u8] = b"ntuser.dat";
    leaf.len() >= PREFIX.len()
        && leaf
            .iter()
            .zip(PREFIX)
            .all(|(&unit, &want)| unit < 128 && (unit as u8).to_ascii_lowercase() == want)
}

/// Run the calling thread, and the re-reads it makes, below normal activity: lower
/// CPU priority, and background I/O and memory priority.
fn background_priority() {
    // SAFETY: GetCurrentThread is a pseudo handle; THREAD_MODE_BACKGROUND_BEGIN only
    // affects the calling thread and ends with it.
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
    }
}

// --- FFI (kernel32) ------------------------------------------------------------------------

const FILE_LIST_DIRECTORY: u32 = 0x0001;
const FILE_SHARE_ALL: u32 = 0x0000_0007;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
const INVALID_HANDLE_VALUE: isize = -1;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
/// FILE_NOTIFY_CHANGE_FILE_NAME | DIR_NAME | SIZE | LAST_WRITE.
const NOTIFY_FILTER: u32 = 0x0001 | 0x0002 | 0x0008 | 0x0010;
const ACTION_MODIFIED: u32 = 3;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 0x0102;
const ERROR_INVALID_PARAMETER: i32 = 87;
const ERROR_NOTIFY_ENUM_DIR: i32 = 1022;
const THREAD_MODE_BACKGROUND_BEGIN: i32 = 0x0001_0000;

/// `OVERLAPPED` (the offset/pointer union is unused for a directory request).
#[repr(C)]
#[allow(dead_code)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: *mut c_void,
}

impl Overlapped {
    fn new(event: *mut c_void) -> Overlapped {
        Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            event,
        }
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *const c_void,
        creation: u32,
        flags: u32,
        template: *mut c_void,
    ) -> *mut c_void;
    fn CreateEventW(attributes: *const c_void, manual_reset: i32, initial: i32, name: *const u16) -> *mut c_void;
    fn ReadDirectoryChangesW(
        handle: *mut c_void,
        buffer: *mut c_void,
        length: u32,
        subtree: i32,
        filter: u32,
        returned: *mut u32,
        overlapped: *mut Overlapped,
        completion: *const c_void,
    ) -> i32;
    fn GetOverlappedResult(handle: *mut c_void, overlapped: *mut Overlapped, transferred: *mut u32, wait: i32) -> i32;
    fn CancelIoEx(handle: *mut c_void, overlapped: *mut Overlapped) -> i32;
    fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn GetCurrentThread() -> *mut c_void;
    fn SetThreadPriority(thread: *mut c_void, priority: i32) -> i32;
}
