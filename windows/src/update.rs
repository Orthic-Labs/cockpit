//! The Windows notch's updater: the same release feed and the same card as the Mac's
//! `Updater` (`mac/Notch/Sources/App/Updater.swift`, `UpdateCard.swift`).
//!
//! GitHub's latest release of Orthic-Labs/pulse is read over WinHTTP (`http.rs`) at most every
//! six hours while "Automatically check" (`auto_update_check`) is on. A newer version is offered
//! once as a notch card (Update, Later); nothing is downloaded or run until the user clicks
//! Update. Update downloads the release's `Pulse-Setup-x64.exe` (the per-user NSIS installer the
//! release lane builds; no elevation) into `%LOCALAPPDATA%\Pulse\updates`, checks its size and
//! the SHA-256 GitHub reports for the asset (the release API's `digest` field, when present),
//! verifies its Authenticode signature with `WinVerifyTrust`, then runs it silently (`/S`) and
//! exits so the installer can replace the files. A failure at any step deletes the download,
//! leaves the installed notch as it was and is logged.
//!
//! State is a global model like `send.rs`: the worker threads change it and post
//! `MSG_UPDATE` to the controller window, which redraws the card on the UI thread.

use crate::card::{CardContent, Row};
use crate::diag;
use crate::http;
use crate::json::{self, Value};
use crate::raii::hwnd_from_key;
use crate::send::{Action, Panel};
use crate::usage::now_secs;
use std::ffi::c_void;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP, WM_CLOSE};
use windows::core::{GUID, PCWSTR};

/// Posted to the controller window when the card or its phase changed.
pub const MSG_UPDATE: u32 = WM_APP + 0x75;

/// The release asset the updater installs. Defined with the release lane: a per-user NSIS
/// installer, Authenticode signed, attached to the GitHub release of every version.
pub const ASSET_NAME: &str = "Pulse-Setup-x64.exe";
const FEED_HOST: &str = "api.github.com";
const FEED_PATH: &str = "/repos/Orthic-Labs/pulse/releases/latest";
const DOWNLOAD_HOST: &str = "github.com";
const CHECK_INTERVAL_SECS: u64 = 6 * 60 * 60;
const TICK: Duration = Duration::from_secs(30 * 60);
const TIMEOUT_MS: i32 = 20_000;
const DOWNLOAD_TIMEOUT_MS: i32 = 60_000;
const MAX_INSTALLER_BYTES: u64 = 512 * 1024 * 1024;
const NOTES_CHARS: usize = 200;
const NOTES_LINE: usize = 38;
const NOTES_LINES: usize = 3;
const DEFAULT_NOTES: &str = "A new version of Pulse is ready to install.";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// How far along the card is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    Available,
    /// Share of the installer received, when the size is known.
    Downloading(Option<f32>),
    Preparing,
    Installing,
}

#[derive(Clone, Debug)]
struct Release {
    version: String,
    notes: String,
    size: u64,
    /// Lower-case hex SHA-256 from the feed, when it gave one.
    sha256: Option<String>,
    path: String,
}

struct State {
    controller: isize,
    auto: bool,
    release: Option<Release>,
    prompt: Option<Phase>,
    /// A check or an install is running.
    busy: bool,
    offered: Option<String>,
    last_attempt: u64,
    started: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    controller: 0,
    auto: true,
    release: None,
    prompt: None,
    busy: false,
    offered: None,
    last_attempt: 0,
    started: false,
});
static STOP: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn notify() {
    let controller = state().controller;
    if controller != 0 {
        // SAFETY: posting to a window handle that may have gone is harmless.
        let _ = unsafe {
            PostMessageW(
                Some(hwnd_from_key(controller)),
                MSG_UPDATE,
                WPARAM(0),
                LPARAM(0),
            )
        };
    }
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

// ---- the card ---------------------------------------------------------------------------------

/// Markdown release notes as one plain line of at most `NOTES_CHARS` characters.
pub fn summary(notes: &str) -> String {
    let plain: String = notes
        .chars()
        .map(|c| {
            if matches!(c, '#' | '*' | '`' | '_' | '>') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let joined = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.chars().take(NOTES_CHARS).collect()
}

/// `text` broken at spaces into at most `NOTES_LINES` lines, the last ending in an ellipsis
/// when the rest does not fit.
fn wrap(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cut = false;
    for word in text.split_whitespace() {
        let fits = lines
            .last()
            .is_some_and(|line| line.chars().count() + 1 + word.chars().count() <= NOTES_LINE);
        if fits {
            if let Some(line) = lines.last_mut() {
                line.push(' ');
                line.push_str(word);
            }
        } else if lines.len() == NOTES_LINES {
            cut = true;
            break;
        } else {
            lines.push(word.to_string());
        }
    }
    if cut && let Some(last) = lines.last_mut() {
        last.push('\u{2026}');
    }
    lines
}

/// The card for `version` in `phase`. In `Available` the buttons are rows with an action:
/// `Accept` is Update and `Close` is Later.
pub fn card(version: &str, notes: &str, phase: Phase) -> Panel {
    let installing = phase != Phase::Available;
    let title = if installing {
        format!("Installing Pulse {version}")
    } else {
        format!("Pulse {version} is available")
    };
    let mut rows = Vec::new();
    let mut actions: Vec<Option<Action>> = Vec::new();
    let mut push = |row: Row, action: Option<Action>| {
        rows.push(row);
        actions.push(action);
    };
    let button = |label: &str| Row::Pair {
        label: label.to_string(),
        value: String::new(),
    };
    match phase {
        Phase::Available => {
            let text = if notes.trim().is_empty() {
                DEFAULT_NOTES.to_string()
            } else {
                summary(notes)
            };
            for line in wrap(&text) {
                push(Row::Note(line), None);
            }
            push(button("Update"), Some(Action::Accept));
            push(button("Later"), Some(Action::Close));
        }
        _ => {
            // The bar fills with the download (85%), then preparing, then installing.
            let (label, fraction) = match phase {
                Phase::Downloading(None) => ("Downloading\u{2026}".to_string(), 0.0),
                Phase::Downloading(Some(share)) => (
                    format!(
                        "Downloading\u{2026} {}%",
                        (share.clamp(0.0, 1.0) * 100.0).round() as u32
                    ),
                    share.clamp(0.0, 1.0) * 0.85,
                ),
                Phase::Preparing => ("Preparing\u{2026}".to_string(), 0.9),
                _ => ("Installing\u{2026}".to_string(), 1.0),
            };
            push(
                Row::Bar {
                    label,
                    value: String::new(),
                    fraction: Some(fraction),
                },
                None,
            );
        }
    }
    Panel {
        content: CardContent {
            title,
            accessory: None,
            rows,
        },
        actions,
    }
}

/// The card to show now, if the user has an update offered or under way.
pub fn panel() -> Option<Panel> {
    let st = state();
    let phase = st.prompt?;
    let release = st.release.as_ref()?;
    Some(card(&release.version, &release.notes, phase))
}

/// A click on row `row` of `panel()`.
pub fn click(row: usize) {
    let action = panel().and_then(|p| p.actions.get(row).cloned().flatten());
    match action {
        Some(Action::Accept) => install(),
        Some(Action::Close) => {
            state().prompt = None;
            notify();
        }
        _ => {}
    }
}

// ---- checking ---------------------------------------------------------------------------------

fn data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("Pulse"))
}

fn state_file() -> Option<PathBuf> {
    Some(data_dir()?.join("update-state.txt"))
}

fn load_persisted(st: &mut State) {
    let Some(text) = state_file().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return;
    };
    for line in text.lines().take(8) {
        match line.split_once('=') {
            Some(("last_attempt", v)) => st.last_attempt = v.trim().parse().unwrap_or(0),
            Some(("offered", v)) if v.len() < 64 => st.offered = Some(v.trim().to_string()),
            _ => {}
        }
    }
}

fn persist(st: &State) {
    let (Some(dir), Some(file)) = (data_dir(), state_file()) else {
        return;
    };
    let _ = std::fs::create_dir_all(dir);
    let text = format!(
        "last_attempt={}\noffered={}\n",
        st.last_attempt,
        st.offered.as_deref().unwrap_or("")
    );
    let _ = std::fs::write(file, text);
}

/// Starts the schedule: a check now if one is due, then a look every half hour. Each look
/// asks GitHub only when six hours have passed since the last try and `auto` is on.
pub fn start(controller_key: isize, auto: bool) {
    {
        let mut st = state();
        if st.started {
            return;
        }
        st.started = true;
        st.controller = controller_key;
        st.auto = auto;
        load_persisted(&mut st);
    }
    *STOP.0.lock().unwrap_or_else(PoisonError::into_inner) = false;
    let _ = std::thread::Builder::new()
        .name("pulse-update".into())
        .spawn(worker);
}

pub fn stop() {
    *STOP.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
    STOP.1.notify_all();
}

/// "Check now": asks GitHub regardless of the schedule and always offers a newer release.
#[allow(dead_code)] // the hub's General "Check now" has no Windows command channel yet
pub fn check_now() {
    let _ = std::thread::Builder::new()
        .name("pulse-update-check".into())
        .spawn(|| check(true));
}

fn worker() {
    loop {
        let due = {
            let st = state();
            st.auto && now_secs().saturating_sub(st.last_attempt) >= CHECK_INTERVAL_SECS
        };
        if due {
            check(false);
        }
        let guard = STOP.0.lock().unwrap_or_else(PoisonError::into_inner);
        let (stopped, _) = STOP
            .1
            .wait_timeout_while(guard, TICK, |stopped| !*stopped)
            .unwrap_or_else(PoisonError::into_inner);
        if *stopped {
            return;
        }
    }
}

/// One look at the feed. `offering` is "Check now", which shows a newer release even if it
/// was offered before; a scheduled check offers each version once.
fn check(offering: bool) {
    {
        let mut st = state();
        if st.busy {
            return;
        }
        st.busy = true;
        st.last_attempt = now_secs();
        persist(&st);
    }
    let found = fetch_release();
    let mut offer = false;
    {
        let mut st = state();
        st.busy = false;
        match found {
            Ok(Some(release)) => {
                let version = release.version.clone();
                st.release = Some(release);
                if st.prompt.is_none() && (offering || st.offered.as_deref() != Some(&version)) {
                    st.prompt = Some(Phase::Available);
                    st.offered = Some(version);
                    persist(&st);
                    offer = true;
                }
            }
            Ok(None) => diag::info("update_check", &[("result", "up_to_date")]),
            Err(reason) => diag::info("update_check", &[("result", "failed"), ("reason", reason)]),
        }
    }
    if offer {
        notify();
    }
}

/// `Ok(Some)` for a newer release with an installer; `Ok(None)` when up to date.
fn fetch_release() -> Result<Option<Release>, &'static str> {
    let agent = format!("Pulse-Windows/{}", current_version());
    let response = http::get(
        FEED_HOST,
        FEED_PATH,
        &[
            ("Accept", "application/vnd.github+json"),
            ("X-GitHub-Api-Version", "2022-11-28"),
            ("User-Agent", agent.as_str()),
        ],
        TIMEOUT_MS,
    )
    .map_err(|_| "unreachable")?;
    match response.status {
        200 => {}
        403 | 429 => return Err("rate_limited"),
        _ => return Err("bad_status"),
    }
    let root = json::parse(&response.body, 512 * 1024).ok_or("unreadable")?;
    let tag = root
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or("no_tag")?;
    let version = tag.strip_prefix('v').unwrap_or(tag);
    let (Some(latest), Some(current)) = (parse_version(version), parse_version(current_version()))
    else {
        return Err("bad_version");
    };
    if latest <= current {
        return Ok(None);
    }
    let asset = root
        .get("assets")
        .and_then(Value::as_array)
        .unwrap_or(&[])
        .iter()
        .find(|a| a.get("name").and_then(Value::as_str) == Some(ASSET_NAME))
        .ok_or("no_installer")?;
    let url = asset
        .get("browser_download_url")
        .and_then(Value::as_str)
        .ok_or("no_installer")?;
    let path = url
        .strip_prefix(&format!("https://{DOWNLOAD_HOST}/"))
        .ok_or("foreign_host")?;
    let sha256 = asset
        .get("digest")
        .and_then(Value::as_str)
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase);
    Ok(Some(Release {
        version: version.to_string(),
        notes: root
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        size: asset
            .get("size")
            .and_then(Value::as_f64)
            .map_or(0, |s| s as u64),
        sha256,
        path: format!("/{path}"),
    }))
}

/// `1.2.3` as numbers; a pre-release (`-rc1`) or anything odd is not a version to offer.
fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
}

// ---- installing -------------------------------------------------------------------------------

/// Update: download, verify, run the installer and leave.
fn install() {
    let release = {
        let mut st = state();
        if st.busy || st.prompt != Some(Phase::Available) {
            return;
        }
        let Some(release) = st.release.clone() else {
            return;
        };
        st.busy = true;
        st.prompt = Some(Phase::Downloading(None));
        release
    };
    notify();
    let _ = std::thread::Builder::new()
        .name("pulse-update-install".into())
        .spawn(move || {
            if let Err(reason) = run_install(&release) {
                diag::info(
                    "update_install",
                    &[("result", "failed"), ("reason", reason)],
                );
                let mut st = state();
                st.busy = false;
                st.prompt = None;
                drop(st);
                notify();
            }
        });
}

fn set_phase(phase: Phase) {
    let mut st = state();
    if st.prompt.is_some() {
        st.prompt = Some(phase);
    }
    drop(st);
    notify();
}

fn run_install(release: &Release) -> Result<(), &'static str> {
    let dir = data_dir().ok_or("no_data_dir")?.join("updates");
    // Only the one installer is kept.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|_| "no_updates_dir")?;
    let part = dir.join(format!("{ASSET_NAME}.part"));
    let file = dir.join(ASSET_NAME);
    let result = download_and_launch(release, &part, &file);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    result
}

fn download_and_launch(release: &Release, part: &Path, file: &Path) -> Result<(), &'static str> {
    let agent = format!("Pulse-Windows/{}", current_version());
    {
        let mut out = std::fs::File::create(part).map_err(|_| "cannot_write")?;
        let mut last_percent = u32::MAX;
        let received = http::download(
            DOWNLOAD_HOST,
            &release.path,
            &[
                ("User-Agent", agent.as_str()),
                ("Accept", "application/octet-stream"),
            ],
            DOWNLOAD_TIMEOUT_MS,
            MAX_INSTALLER_BYTES,
            &mut out,
            |done, total| {
                let share = total.filter(|t| *t > 0).map(|t| done as f32 / t as f32);
                let percent = share.map_or(u32::MAX - 1, |s| (s * 100.0) as u32);
                if percent != last_percent {
                    last_percent = percent;
                    set_phase(Phase::Downloading(share));
                }
            },
        )
        .map_err(|_| "download_failed")?;
        if release.size > 0 && received != release.size {
            return Err("size_mismatch");
        }
    }
    set_phase(Phase::Preparing);
    if let Some(expected) = &release.sha256 {
        let actual = sha256_file(part).ok_or("cannot_read")?;
        if &actual != expected {
            return Err("hash_mismatch");
        }
    }
    std::fs::rename(part, file).map_err(|_| "cannot_rename")?;
    if !authenticode_ok(file) {
        return Err("signature_invalid");
    }
    // A valid chain is not enough: the installer must be signed by the same publisher
    // as this running Pulse.exe (one Azure signing profile signs both). An unsigned
    // build of the notch never installs updates.
    let ours = std::env::current_exe().ok().and_then(|exe| crate::installer::signer_name(&exe));
    match (ours, crate::installer::signer_name(file)) {
        (Some(ours), Some(theirs)) if ours == theirs => {}
        _ => return Err("signer_mismatch"),
    }
    set_phase(Phase::Installing);
    use std::os::windows::process::CommandExt;
    std::process::Command::new(file)
        .arg("/S")
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|_| "cannot_run")?;
    diag::info("update_install", &[("result", "installer_started")]);
    // The installer replaces the notch's files, so the notch leaves (the installer starts the
    // new one).
    std::thread::sleep(Duration::from_secs(1));
    let controller = state().controller;
    // SAFETY: posting to a window handle that may have gone is harmless.
    let _ = unsafe {
        PostMessageW(
            Some(hwnd_from_key(controller)),
            WM_CLOSE,
            WPARAM(0),
            LPARAM(0),
        )
    };
    Ok(())
}

// ---- Authenticode -----------------------------------------------------------------------------

#[repr(C)]
#[allow(dead_code)] // read by WinVerifyTrust, not by Rust
struct WinTrustFileInfo {
    cb_struct: u32,
    file_path: PCWSTR,
    file_handle: *mut c_void,
    known_subject: *mut GUID,
}

#[repr(C)]
#[allow(dead_code)] // read by WinVerifyTrust, not by Rust
struct WinTrustData {
    cb_struct: u32,
    policy_callback_data: *mut c_void,
    sip_client_data: *mut c_void,
    ui_choice: u32,
    revocation_checks: u32,
    union_choice: u32,
    file: *mut WinTrustFileInfo,
    state_action: u32,
    state_data: *mut c_void,
    url_reference: PCWSTR,
    prov_flags: u32,
    ui_context: u32,
    signature_settings: *mut c_void,
}

#[allow(non_snake_case)]
#[link(name = "wintrust")]
unsafe extern "system" {
    fn WinVerifyTrust(window: *mut c_void, action: *mut GUID, data: *mut c_void) -> i32;
}

const WTD_UI_NONE: u32 = 2;
const WTD_REVOKE_NONE: u32 = 0;
const WTD_CHOICE_FILE: u32 = 1;
const WTD_STATEACTION_VERIFY: u32 = 1;
const WTD_STATEACTION_CLOSE: u32 = 2;
const WTD_CACHE_ONLY_URL_RETRIEVAL: u32 = 0x1000;
const WTD_REVOCATION_CHECK_NONE: u32 = 0x10;
/// `WINTRUST_ACTION_GENERIC_VERIFY_V2`.
const ACTION_GENERIC_VERIFY_V2: GUID = GUID::from_u128(0x00aac56b_cd44_11d0_8cc2_00c04fc295ee);

/// True when the file carries a valid Authenticode signature that chains to a trusted root.
/// The publisher is not pinned here: the download is from this project's own release, over
/// HTTPS, against the digest the feed gave.
fn authenticode_ok(path: &Path) -> bool {
    let wide: Vec<u16> = path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut info = WinTrustFileInfo {
        cb_struct: std::mem::size_of::<WinTrustFileInfo>() as u32,
        file_path: PCWSTR(wide.as_ptr()),
        file_handle: std::ptr::null_mut(),
        known_subject: std::ptr::null_mut(),
    };
    let mut data = WinTrustData {
        cb_struct: std::mem::size_of::<WinTrustData>() as u32,
        policy_callback_data: std::ptr::null_mut(),
        sip_client_data: std::ptr::null_mut(),
        ui_choice: WTD_UI_NONE,
        revocation_checks: WTD_REVOKE_NONE,
        union_choice: WTD_CHOICE_FILE,
        file: &mut info,
        state_action: WTD_STATEACTION_VERIFY,
        state_data: std::ptr::null_mut(),
        url_reference: PCWSTR::null(),
        prov_flags: WTD_CACHE_ONLY_URL_RETRIEVAL | WTD_REVOCATION_CHECK_NONE,
        ui_context: 0,
        signature_settings: std::ptr::null_mut(),
    };
    let mut action = ACTION_GENERIC_VERIFY_V2;
    // INVALID_HANDLE_VALUE as the window: no UI.
    let no_window = usize::MAX as *mut c_void;
    // SAFETY: `data`, `info` and `wide` outlive both calls; the layouts match WINTRUST_DATA
    // and WINTRUST_FILE_INFO; the second call releases the state the first one made.
    let status = unsafe {
        WinVerifyTrust(
            no_window,
            &mut action,
            (&mut data as *mut WinTrustData).cast(),
        )
    };
    data.state_action = WTD_STATEACTION_CLOSE;
    // SAFETY: as above.
    unsafe {
        WinVerifyTrust(
            no_window,
            &mut action,
            (&mut data as *mut WinTrustData).cast(),
        )
    };
    status == 0
}

// ---- SHA-256 ----------------------------------------------------------------------------------

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

struct Sha256 {
    h: [u32; 8],
    buffer: [u8; 64],
    filled: usize,
    total: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            h: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: [0; 64],
            filled: 0,
            total: 0,
        }
    }

    fn block(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, add) in self.h.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(add);
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.total += data.len() as u64;
        while !data.is_empty() {
            let take = (64 - self.filled).min(data.len());
            self.buffer[self.filled..self.filled + take].copy_from_slice(&data[..take]);
            self.filled += take;
            data = &data[take..];
            if self.filled == 64 {
                let block = self.buffer;
                self.block(&block);
                self.filled = 0;
            }
        }
    }

    fn finish(mut self) -> String {
        let bits = self.total * 8;
        self.update(&[0x80]);
        while self.filled != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        self.h.iter().map(|word| format!("{word:08x}")).collect()
    }
}

/// Lower-case hex SHA-256 of a file, or `None` if it cannot be read.
fn sha256_file(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hash = Sha256::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut chunk).ok()?;
        if read == 0 {
            return Some(hash.finish());
        }
        hash.update(&chunk[..read]);
    }
}
