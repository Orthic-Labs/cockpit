//! The Claude hover card's "Restart Claude and sync chats" button (the Windows counterpart of
//! the Mac's `ClaudeRestart`). The owner presses it after signing in to a new account: Claude
//! Desktop is closed politely (never force-killed, up to 20 s), the bundled
//! `Helpers\pulse.exe claude sync --apply --json` merges the Code-session metadata of every
//! account, and Claude is opened again whatever the sync said. It all runs on a worker thread;
//! the card shows the phase (spinner, a check for about two seconds, a red mark with the
//! reason), and `usage` is told to read Desktop's account at once and every second for a
//! minute.
//!
//! Chats: quitting Desktop (or switching account inside it) ends every Code chat's CLI, and
//! Desktop starts one again only when its page is shown. `pulse claude remember` notes the
//! running chats whenever one starts or ends (`claude_watch`) and once more just before the close;
//! after the reopen, `pulse claude reopen` shows the last running set one by one with
//! Desktop's own `claude://code/continue` link, so each resumes without being sent anything.
//!
//! Only Claude Desktop is ever touched. Claude Code's command line is also named `claude.exe`,
//! so a process counts only when its image path is Desktop's (`%LOCALAPPDATA%\AnthropicClaude\`,
//! or another known Desktop install folder), never by its name.
//!
//! Closing: `WM_CLOSE` goes to Desktop's own top-level windows. Desktop quits on that only
//! while its tray icon is off (`preferences.menuBarEnabled` is false); with the tray on, the
//! close only hides the window and the app keeps running. Rather than hide the window for
//! nothing, the button says so and leaves Desktop alone.

use crate::json::{self, Value};
use crate::{desktop, diag, usage};
use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
};
use windows::core::{BOOL, PWSTR};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TH32CS_SNAPPROCESS: u32 = 2;
const CLOSE_WAIT_STEPS: u32 = 80;
const CLOSE_WAIT_STEP: Duration = Duration::from_millis(250);
const DONE_SHOWN: Duration = Duration::from_secs(2);
/// The window class of Chromium's (Electron's) top-level frames.
const FRAME_CLASS: &str = "Chrome_WidgetWin_1";
const ERROR_MAX_BYTES: usize = 64 * 1024;
const REOPEN_GAP: Duration = Duration::from_secs(90);
const SYNC_ATTEMPTS: u32 = 12;
const SYNC_RETRY_STEP: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Idle,
    Running,
    /// Shown for about two seconds, then `Idle`.
    Done,
    /// The reason, shown on the card until the button is pressed again.
    Failed(String),
}

static PHASE: Mutex<Phase> = Mutex::new(Phase::Idle);

pub fn phase() -> Phase {
    PHASE.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

fn set(phase: Phase) {
    *PHASE.lock().unwrap_or_else(PoisonError::into_inner) = phase;
    usage::notify();
}

/// Starts the restart unless one is already running. Returns at once.
pub fn start() {
    {
        let mut phase = PHASE.lock().unwrap_or_else(PoisonError::into_inner);
        if *phase == Phase::Running {
            return;
        }
        *phase = Phase::Running;
    }
    usage::notify();
    let spawned = std::thread::Builder::new()
        .name("claude-restart".into())
        .spawn(|| {
            let outcome = std::panic::catch_unwind(sequence)
                .unwrap_or_else(|_| Some("The restart stopped unexpectedly".to_string()));
            match outcome {
                None => {
                    diag::info("claude_restart", &[("result", "done")]);
                    set(Phase::Done);
                    std::thread::sleep(DONE_SHOWN);
                    if phase() == Phase::Done {
                        set(Phase::Idle);
                    }
                }
                Some(reason) => {
                    diag::info(
                        "claude_restart",
                        &[("result", "failed"), ("reason", &reason)],
                    );
                    set(Phase::Failed(reason));
                }
            }
        });
    if spawned.is_err() {
        set(Phase::Failed(
            "The restart could not be started".to_string(),
        ));
    }
}

/// `None` on success, else the reason the card shows.
fn sequence() -> Option<String> {
    let running = desktop_pids();
    let launch = launch_target(&running);
    if !running.is_empty() {
        if desktop::tray_enabled() {
            return Some(
                "Claude keeps running in the system tray. Quit it from its tray icon \
                 (or turn the tray off in Claude's settings), then press again"
                    .to_string(),
            );
        }
        remember_chats();
        close_windows(&running);
        let mut closed = false;
        for _ in 0..CLOSE_WAIT_STEPS {
            std::thread::sleep(CLOSE_WAIT_STEP);
            if desktop_pids().is_empty() {
                closed = true;
                break;
            }
        }
        if !closed {
            return Some("Claude is still open".to_string());
        }
    }
    // Desktop's helper processes (crash handler, GPU) can outlive its windows by a few
    // seconds, and the CLI refuses while any of them runs: retry.
    let mut failure = None;
    for attempt in 0..SYNC_ATTEMPTS {
        failure = run_cli();
        match &failure {
            Some(f) if f.code.as_deref() == Some("claude_running") => {
                diag::info(
                    "claude_restart",
                    &[("sync", "claude_running"), ("attempt", &(attempt + 1).to_string())],
                );
                std::thread::sleep(SYNC_RETRY_STEP);
            }
            _ => break,
        }
    }
    let failure = failure.map(|f| f.reason);
    // Claude comes back whatever the sync said.
    let reopened = launch.as_deref().is_some_and(reopen);
    if reopened {
        reopen_chats();
    }
    usage::refetch_claude_soon();
    match failure {
        Some(reason) => Some(reason),
        None if !reopened => Some("Synced, but Claude could not be found to reopen".to_string()),
        None => None,
    }
}

// ------------------------------------------------------------------ the chats

/// Runs a Pulse command hidden and waits for it; its output is not read.
pub fn run_cli_quiet(args: &[&str]) {
    if let Some(mut command) = hidden_cli(args) {
        let _ = command.status();
    }
}

fn hidden_cli(args: &[&str]) -> Option<Command> {
    let mut command = Command::new(cli_path()?);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    Some(command)
}

fn remember_chats() {
    run_cli_quiet(&["claude", "remember", "--json"]);
}

/// Runs on its own: it waits for Desktop to come up and for each chat's CLI, which can take
/// minutes, and the button is done once Desktop is open. A second call within
/// `REOPEN_GAP` (the button's reopen and the account change it causes) is dropped.
pub fn reopen_chats() {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    {
        let mut last = LAST.lock().unwrap_or_else(PoisonError::into_inner);
        if last.is_some_and(|at| at.elapsed() < REOPEN_GAP) {
            return;
        }
        *last = Some(Instant::now());
    }
    if let Some(mut command) = hidden_cli(&["claude", "reopen", "--json"]) {
        let _ = command.spawn();
    }
}

// ------------------------------------------------------------------ the sync

fn cli_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [dir.join("Helpers").join("pulse.exe"), dir.join("pulse.exe")]
        .into_iter()
        .find(|path| path.is_file())
}

/// Why the sync failed: the reason the card shows, and the CLI's stable `code` when it
/// gave one (`claude_running` is retried).
struct SyncFailure {
    reason: String,
    code: Option<String>,
}

impl SyncFailure {
    fn plain(reason: impl Into<String>) -> Self {
        Self { reason: reason.into(), code: None }
    }
}

/// Runs `pulse claude sync --apply --json` hidden; `None` on success, else the failure (the
/// CLI's own `error` text and `code` when it gave them).
fn run_cli() -> Option<SyncFailure> {
    let Some(cli) = cli_path() else {
        return Some(SyncFailure::plain("The Pulse command line tool is missing"));
    };
    let output = Command::new(cli)
        .args(["claude", "sync", "--apply", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            return Some(SyncFailure::plain(format!(
                "Could not run the Pulse command line tool: {error}"
            )));
        }
    };
    if output.status.success() {
        return None;
    }
    // The CLI prints `{"error": ..., "code": ...}`; which stream carries it is not relied on.
    let printed = [&output.stdout, &output.stderr]
        .into_iter()
        .find_map(|bytes| json::parse(bytes, ERROR_MAX_BYTES));
    let text = |key: &str| {
        printed
            .as_ref()
            .and_then(|value| value.get(key))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    Some(SyncFailure {
        reason: text("error").unwrap_or_else(|| "Claude sync failed".to_string()),
        code: text("code"),
    })
}

// ------------------------------------------------------------------ Claude Desktop

/// What to start to open Desktop again: the Squirrel launcher in its install folder (it
/// finds the newest version), another known install, else the program that was running.
fn launch_target(running: &[u32]) -> Option<PathBuf> {
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    let known = [
        local.join("AnthropicClaude").join("claude.exe"),
        local.join("Programs").join("Claude").join("claude.exe"),
    ];
    if let Some(path) = known.into_iter().find(|path| path.is_file()) {
        return Some(path);
    }
    running
        .iter()
        .find_map(|pid| image_path(*pid))
        .map(PathBuf::from)
}

fn reopen(path: &Path) -> bool {
    let mut command = Command::new(path);
    if let Some(dir) = path.parent() {
        command.current_dir(dir);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

/// Whether an executable image path is Claude Desktop's: its Squirrel install folder, its
/// Store package, or the per-user program folder. Never judged by the name `claude.exe`:
/// Claude Code's command line is called that too.
fn is_desktop_exe(path: &str) -> bool {
    let lower = path.to_lowercase().replace('/', "\\");
    lower.contains("\\anthropicclaude\\")
        || lower.contains("\\windowsapps\\claude_")
        || (lower.contains("\\programs\\claude\\") && lower.ends_with("\\claude.exe"))
}

fn image_path(pid: u32) -> Option<String> {
    // SAFETY: a plain OpenProcess for limited query rights; the handle is closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = [0u16; 1024];
    let mut size = buffer.len() as u32;
    // SAFETY: `buffer` holds `size` UTF-16 units for the call.
    let queried = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
    };
    // SAFETY: the handle came from OpenProcess above and is not used again.
    unsafe { CloseHandle(process.0) };
    queried.ok()?;
    Some(String::from_utf16_lossy(&buffer[..size as usize]))
}

#[repr(C)]
struct ProcessEntry {
    size: u32,
    usage: u32,
    process_id: u32,
    heap_id: usize,
    module_id: u32,
    threads: u32,
    parent_id: u32,
    priority: i32,
    flags: u32,
    exe_file: [u16; 260],
}

#[allow(non_snake_case, clashing_extern_declarations)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> *mut c_void;
    fn Process32FirstW(snapshot: *mut c_void, entry: *mut ProcessEntry) -> i32;
    fn Process32NextW(snapshot: *mut c_void, entry: *mut ProcessEntry) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

/// The ids of Claude Desktop's processes: `claude.exe` processes whose image path is
/// Desktop's. Claude Code's command line never matches.
fn desktop_pids() -> Vec<u32> {
    let mut pids = Vec::new();
    // SAFETY: a process snapshot handle, closed below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot.is_null() || snapshot as isize == -1 {
        return pids;
    }
    let mut entry = ProcessEntry {
        size: size_of::<ProcessEntry>() as u32,
        usage: 0,
        process_id: 0,
        heap_id: 0,
        module_id: 0,
        threads: 0,
        parent_id: 0,
        priority: 0,
        flags: 0,
        exe_file: [0; 260],
    };
    // SAFETY: `entry` is a PROCESSENTRY32W with its size set, valid for each call.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while more {
        let length = entry
            .exe_file
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(entry.exe_file.len());
        let name = String::from_utf16_lossy(&entry.exe_file[..length]);
        if name.eq_ignore_ascii_case("claude.exe")
            && image_path(entry.process_id).is_some_and(|path| is_desktop_exe(&path))
        {
            pids.push(entry.process_id);
        }
        // SAFETY: as above.
        more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    // SAFETY: the snapshot handle is not used again.
    unsafe { CloseHandle(snapshot) };
    pids
}

struct Search {
    pids: Vec<u32>,
    found: Vec<HWND>,
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the pointer to the `Search` that `close_windows` keeps alive for the
    // duration of the EnumWindows call, which runs this on the same thread.
    let search = unsafe { &mut *(lparam.0 as *mut Search) };
    let mut pid = 0u32;
    // SAFETY: `pid` outlives the call.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if search.pids.contains(&pid) {
        let mut class = [0u16; 64];
        // SAFETY: `class` is a valid buffer for the call.
        let length = unsafe { GetClassNameW(hwnd, &mut class) };
        if length > 0 && String::from_utf16_lossy(&class[..length as usize]) == FRAME_CLASS {
            search.found.push(hwnd);
        }
    }
    BOOL::from(true)
}

/// Asks Desktop's frames to close (`WM_CLOSE`), the polite way: Desktop runs its own quit
/// cleanup when its last window closes (tray off). Nothing is terminated.
fn close_windows(pids: &[u32]) {
    let mut search = Search {
        pids: pids.to_vec(),
        found: Vec::new(),
    };
    // SAFETY: the callback only runs during this call; `search` lives past it.
    let _ = unsafe { EnumWindows(Some(collect), LPARAM(&mut search as *mut Search as isize)) };
    for hwnd in search.found {
        // SAFETY: posting to a window handle that may have gone is harmless.
        let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
}
