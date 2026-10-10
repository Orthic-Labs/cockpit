//! Change-driven upkeep of Claude Desktop's chats; nothing runs while nothing changes.
//!
//! Three folders are watched with `ReadDirectoryChangesW`, each on its own thread that
//! sleeps in the kernel until something is reported:
//!  * `~/.claude/sessions`: a chat's file appears when its CLI starts and goes when it
//!    ends. A change runs `pulse claude remember` (which chats are running, for the
//!    restart button and an account switch to open again) once the folder is quiet.
//!  * `%APPDATA%\Claude\claude-code-sessions`: a write under the signed-in account's
//!    folder runs `pulse claude mirror`, which copies the newer records into the other
//!    accounts' folders, so whichever account is signed into next already has them.
//!    Writes under any other account's folder are the mirror's own and are ignored.
//!  * `%APPDATA%\Claude\config.json`: when the signed-in account changes, Desktop has just
//!    ended every chat and loaded the new account's folder. After `SETTLE` the mirror runs
//!    once, then `pulse claude reopen` shows the chats that were running again in the new
//!    account (each resumes; nothing is sent to it). No restart is needed, because the
//!    mirror had already put their records in that account's folder.
//!
//! Each command waits for `QUIET` without a further change (at most `MAX_HOLD` from the
//! first one) so a burst of writes is one run. A lost report (the kernel's buffer
//! overflowed) counts as a change. A folder that does not exist yet (Claude not installed,
//! no chat ever started) is looked for again every `MISSING_RETRY`.

use crate::{claude_restart, desktop, diag};
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

const QUIET: Duration = Duration::from_secs(4);
const MAX_HOLD: Duration = Duration::from_secs(20);
const MISSING_RETRY: Duration = Duration::from_secs(300);
/// After an account change, Desktop is left to finish its own writes to the old folder.
const SETTLE: Duration = Duration::from_secs(5);
/// One request's buffer, in `u32`s (64 KiB).
const BUFFER_WORDS: usize = 16 * 1024;

const FILE_LIST_DIRECTORY: u32 = 0x0001;
const FILE_SHARE_ALL: u32 = 0x0007;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const NOTIFY_FILE_NAME: u32 = 0x0001;
const NOTIFY_SIZE: u32 = 0x0008;
const NOTIFY_LAST_WRITE: u32 = 0x0010;
const INVALID_HANDLE: isize = -1;

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
    fn ReadDirectoryChangesW(
        handle: *mut c_void,
        buffer: *mut c_void,
        length: u32,
        subtree: i32,
        filter: u32,
        returned: *mut u32,
        overlapped: *mut c_void,
        completion: *const c_void,
    ) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

/// Starts the watches. Called once; the threads live as long as the notch.
pub fn start() {
    let remember = debounced("claude-remember", &["claude", "remember", "--json"]);
    let mirror = debounced("claude-mirror", &["claude", "mirror", "--json"]);
    // What is running now, and anything written while the notch was not.
    let _ = remember.send(());
    let _ = mirror.send(());

    if let Some(sessions) = cli_sessions_dir() {
        spawn("claude-watch-chats", sessions, false, NOTIFY_FILE_NAME, {
            move |_names: &[String]| {
                let _ = remember.send(());
            }
        });
    }
    let Some(data) = desktop::data_dir() else {
        return;
    };
    spawn(
        "claude-watch-records",
        data.join("claude-code-sessions"),
        true,
        NOTIFY_FILE_NAME | NOTIFY_SIZE | NOTIFY_LAST_WRITE,
        move |names: &[String]| {
            // An empty list is a lost report: what changed is unknown.
            if names.is_empty() || touches_account(names, desktop::signed_in_account()) {
                let _ = mirror.send(());
            }
        },
    );
    let last_account = Mutex::new(desktop::signed_in_account());
    spawn(
        "claude-watch-account",
        data,
        false,
        NOTIFY_FILE_NAME | NOTIFY_LAST_WRITE,
        move |names: &[String]| {
            if !names.is_empty() && !names.iter().any(|n| n.eq_ignore_ascii_case("config.json")) {
                return;
            }
            let Some(now) = desktop::signed_in_account() else {
                return;
            };
            let mut last = last_account.lock().unwrap_or_else(PoisonError::into_inner);
            // The first account seen (Desktop was not running when the notch started) is
            // not a change: opening Desktop again on the same account reopens nothing.
            let changed = last.as_deref().is_some_and(|old| old != now);
            *last = Some(now);
            if changed {
                diag::info("claude_watch", &[("account", "changed")]);
                account_changed();
            }
        },
    );
}

/// Mirror once, then reopen the chats that were running, on a thread of its own (the
/// reopen waits for each chat and can take minutes).
fn account_changed() {
    let _ = std::thread::Builder::new()
        .name("claude-account-changed".into())
        .spawn(|| {
            std::thread::sleep(SETTLE);
            claude_restart::run_cli_quiet(&["claude", "mirror", "--json"]);
            claude_restart::reopen_chats();
        });
}

/// `~/.claude/sessions` (or `%CLAUDE_CONFIG_DIR%\sessions`).
fn cli_sessions_dir() -> Option<PathBuf> {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".claude"))
        })?;
    base.is_absolute().then(|| base.join("sessions"))
}

/// Whether any reported path (relative to `claude-code-sessions`) is under `account`.
fn touches_account(names: &[String], account: Option<String>) -> bool {
    let Some(account) = account else {
        return false;
    };
    names.iter().any(|name| {
        name.split(['\\', '/'])
            .next()
            .is_some_and(|first| first.eq_ignore_ascii_case(&account))
    })
}

/// A sender whose signals run the Pulse command once the signals stop for `QUIET`.
fn debounced(name: &str, args: &'static [&'static str]) -> Sender<()> {
    let (tx, rx) = mpsc::channel::<()>();
    let _ = std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            while rx.recv().is_ok() {
                let first = Instant::now();
                loop {
                    match rx.recv_timeout(QUIET) {
                        Ok(()) if first.elapsed() < MAX_HOLD => {}
                        Ok(()) | Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                claude_restart::run_cli_quiet(args);
            }
        });
    tx
}

fn spawn(
    name: &str,
    dir: PathBuf,
    subtree: bool,
    filter: u32,
    on_change: impl Fn(&[String]) + Send + 'static,
) {
    let _ = std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            loop {
                watch(&dir, subtree, filter, &on_change);
                // The folder is missing, or the watch on it ended (it was removed).
                std::thread::sleep(MISSING_RETRY);
            }
        });
}

/// Reports every change under `dir` until the watch fails. Blocks in the kernel between
/// reports.
fn watch(dir: &Path, subtree: bool, filter: u32, on_change: &impl Fn(&[String])) {
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: `wide` is a NUL-terminated path that outlives the call.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_ALL,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle.is_null() || handle as isize == INVALID_HANDLE {
        return;
    }
    let mut buffer = vec![0u32; BUFFER_WORDS];
    loop {
        let mut returned = 0u32;
        // SAFETY: `buffer` is `BUFFER_WORDS * 4` writable, u32-aligned bytes; the call is
        // synchronous (no OVERLAPPED), so nothing outlives it.
        let ok = unsafe {
            ReadDirectoryChangesW(
                handle,
                buffer.as_mut_ptr().cast(),
                (BUFFER_WORDS * 4) as u32,
                i32::from(subtree),
                filter,
                &mut returned,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if ok == 0 {
            break;
        }
        on_change(&names(&buffer, returned as usize));
    }
    // SAFETY: `handle` was opened above and is closed once.
    unsafe {
        CloseHandle(handle);
    }
}

/// The relative paths in a `FILE_NOTIFY_INFORMATION` chain of `length` bytes; empty when
/// the kernel returned nothing (its buffer overflowed).
fn names(buffer: &[u32], length: usize) -> Vec<String> {
    let mut out = Vec::new();
    let words = length.min(buffer.len() * 4) / 4;
    let mut at = 0usize;
    // Each entry: NextEntryOffset, Action, FileNameLength (bytes), then UTF-16 FileName.
    while at + 3 <= words {
        let next = buffer[at] as usize;
        let name_bytes = buffer[at + 2] as usize;
        let units = name_bytes / 2;
        let start = at + 3;
        if start * 4 + name_bytes > words * 4 {
            break;
        }
        let name: Vec<u16> = (0..units)
            .map(|i| {
                let word = buffer[start + i / 2];
                if i % 2 == 0 { word as u16 } else { (word >> 16) as u16 }
            })
            .collect();
        out.push(String::from_utf16_lossy(&name));
        if next == 0 || next % 4 != 0 {
            break;
        }
        at += next / 4;
    }
    out
}
