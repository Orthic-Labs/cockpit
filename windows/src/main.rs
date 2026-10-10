#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Native Windows notch (M1).
//!
//! Ownership: a hidden controller window owns the sampling timer and receives
//! WM_DISPLAYCHANGE and usage updates; per-monitor notch panels (layered, per-pixel alpha,
//! never activated) are RAII `OwnedWindow`s stored in `STATE`, plus one lazily created card
//! window shared by the hover card and the Quit menu.
//! Locking rule: `STATE`/`SAMPLER` guards are only held for plain data access. Any call that
//! can re-enter a wndproc (SetWindowPos, UpdateLayeredWindow, ShowWindow, DestroyWindow,
//! CreateWindowExW, EnumWindows, EnumDisplayMonitors) runs with no guard held, and panels
//! are removed from `STATE` before they are dropped (destroyed).

mod alerts;
mod autostart;
mod bridge;
mod canvas;
mod card;
mod claude_accounts;
mod claude_restart;
mod claude_watch;
mod desktop;
mod diag;
mod drive_health;
mod fileicon;
mod glyphs;
mod http;
mod hub;
mod installer;
mod json;
mod keys;
mod layout;
mod lifecycle;
mod notify;
mod raii;
mod render;
mod runtime;
mod send;
mod sensors;
mod settings;
mod shot;
mod surface;
mod update;
mod usage;
mod viewshots;
mod visibility;
mod zstd;

use layout::{Badges, Cell, CellView, Edge, Handle, SEND_CELL};
use lifecycle::{Bounds, HIDDEN_INTERVAL_MS, MonitorSpec, ReconcileGate};
use raii::{ClassGuard, OwnedWindow, TimerGuard, hwnd_from_key, hwnd_key};
use runtime::{
    InstanceError, InstanceLock, Placed, Placement, Retry, RetryReport, Slot, UserSecurity,
    desired_hidden, instance_mutex_name, interval_ms, notch_bounds, plan_placements,
};
use sensors::{Machine, Sampler};
use settings::PillSettings;
use std::cell::RefCell;
use std::mem::size_of;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use surface::{TextPainter, present};
use usage::Usage;
use visibility::{Occupancy, classify_window, is_shell_class_name, is_tool_window_ex_style};
use windows::Win32::Foundation::{
    E_FAIL, ERROR_SUCCESS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT,
    SetLastError, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MONITORINFOEXW, MonitorFromRect, ScreenToClient, ValidateRect,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, VK_LBUTTON, VK_MBUTTON, VK_MENU, VK_RBUTTON,
};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, Error, IUnknown, PCWSTR, s, w};

const PANEL_CLASS: PCWSTR = w!("PulseM1Notch");
const CONTROLLER_CLASS: PCWSTR = w!("PulseM1Controller");
const CARD_CLASS: PCWSTR = w!("PulseM1Card");
const TIMER_ID: usize = 7;
const MENU_TIMER_ID: usize = 8;
const MENU_POLL_MS: u32 = 80;
/// Grace period for the pointer to cross the gap from the Send cell to its card.
const HOVER_TIMER_ID: usize = 9;
/// Polls the pointer while the notch is open, to fold it again once the pointer has left.
const FOLD_TIMER_ID: usize = 21;
const FOLD_POLL_MS: u32 = 160;
/// Polls the pointer must be outside before the notch folds (about half a second).
const FOLD_GRACE_TICKS: u32 = 3;
const HOVER_GRACE_MS: u32 = 220;
/// Hub section the settings button opens: the Mac notch's own (`HubLauncher.open(section:
/// "settings")`), which the hub shows as Accounts, the first page of its settings.
const SETTINGS_SECTION: &str = "settings";
/// One-second tick while an alert or update card is up (alerts put themselves away).
const NOTICE_TIMER_ID: usize = 10;
static NOTICE_TIMER_ARMED: AtomicBool = AtomicBool::new(false);
/// Redraws a card that moves (a spinner, a travelling progress bar) while one is up.
const ANIM_TIMER_ID: usize = 22;
const ANIM_MS: u32 = 50;
/// One turn of a spinner or one sweep of a travelling bar.
const ANIM_PERIOD_MS: u128 = 1200;
static ANIM_TIMER_ARMED: AtomicBool = AtomicBool::new(false);
/// Posted by `send` when the Send ring, its hover card or a popup changed.
const WM_SEND: u32 = WM_APP + 0x5E;
static COORDINATES_COMPARABLE: AtomicBool = AtomicBool::new(false);

struct Panel {
    id: String,
    bounds: Bounds, // monitor bounds this panel was placed for (updated only after a move succeeds)
    slot: Slot,     // position/DPI this panel was placed with (updated only after a move succeeds)
    // Last successfully applied hidden state. Created hidden; a failed show/hide leaves it
    // unchanged so the next refresh retries while the panel keeps owning its HWND.
    visibility: Retry<bool>,
    /// What the layered bitmap currently shows (cell views + DPI); `None` forces a redraw.
    drawn: Option<Drawn>,
    /// When the layered bitmap was last published successfully (self-heal staleness clock).
    published: Option<Instant>,
    /// When the self-heal last re-homed or raised this window (rate limit).
    healed: Option<Instant>,
    window: OwnedWindow,
}

/// What a notch's bitmap was drawn from: cells, edge, folded, DPI, badges and the settings
/// handle part shown out.
type Drawn = (
    Vec<CellView>,
    Edge,
    bool,
    u32,
    Badges,
    Option<Handle>,
    Option<render::Press>,
);

struct Drag {
    panel: isize,
    id: String,
    /// Pointer offset from the notch's top-left along the edge's axis.
    grab: i32,
    /// Edge the notch is docked to right now (the pointer can carry it to another).
    edge: Edge,
    folded: bool,
    dpi: u32,
    monitor: Bounds,
    left: i32,
    top: i32,
    moved: bool,
}

struct Interaction {
    /// Panel and cell currently under the pointer.
    hover: Option<(isize, usize)>,
    /// Panel with an armed WM_MOUSELEAVE request.
    tracking: Option<isize>,
    /// Panel and cell where a plain left press started (click on release in the same cell).
    press: Option<(isize, usize)>,
    /// Panel and part of the settings handle under the pointer (the grip is out while any is).
    handle: Option<(isize, Handle)>,
    /// Panel where a left press started on the settings button (click on release on it).
    press_orb: Option<isize>,
    /// Panel and part the button is held down on (drawn pressed until release or leave).
    pressed: Option<(isize, render::Press)>,
    drag: Option<Drag>,
    /// Quit menu: owning panel and its screen rectangle.
    menu: Option<(isize, RECT)>,
    /// The pointer is over the Quit menu (its plate is drawn lit).
    menu_hot: bool,
    /// What the card window shows.
    card_shown: Option<Shown>,
    /// Pointer is over the card window (only the Send card takes the pointer).
    card_hover: bool,
    /// Leave notification armed on the card window.
    card_tracking: bool,
    /// The live control on the card under the pointer (drawn with a hover plate).
    card_hit: Option<card::Hit>,
    /// `send::popup_hover(true)` was sent for the popup under the pointer.
    popup_hover_sent: bool,
    /// Consecutive fold polls with the pointer away from every open notch and card.
    fold_outside: u32,
}

/// The card on screen: owning panel, cell, content and whether it is a sharing popup.
struct Shown {
    key: isize,
    cell: usize,
    panel: send::Panel,
    popup: bool,
    /// A usage alert or the update card (`alerts.rs`, `update.rs`), not a hover card.
    notice: bool,
}

impl Interaction {
    const fn new() -> Self {
        Self {
            hover: None,
            tracking: None,
            press: None,
            handle: None,
            press_orb: None,
            pressed: None,
            drag: None,
            menu: None,
            menu_hot: false,
            card_shown: None,
            card_hover: false,
            card_tracking: false,
            card_hit: None,
            popup_hover_sent: false,
            fold_outside: 0,
        }
    }
}

struct AppState {
    panels: Vec<Panel>,
    machine: Option<Machine>,
    card: Option<OwnedWindow>,
    controller: isize,
    ui: Interaction,
    timer_ms: u32,
    shutting_down: bool,
    reconcile_retry: bool,
    gate: ReconcileGate,
    settings: PillSettings,
    /// Last settings known to be on disk (or defaults when nothing was loaded).
    settings_baseline: PillSettings,
    /// False when the stored file was unusable: never overwrite it.
    settings_writable: bool,
    /// Update-available and permission-needed dots on the notch.
    badges: Badges,
}

impl AppState {
    const fn new() -> Self {
        Self {
            panels: Vec::new(),
            machine: None,
            card: None,
            controller: 0,
            ui: Interaction::new(),
            timer_ms: 0,
            shutting_down: false,
            reconcile_retry: false,
            gate: ReconcileGate::new(),
            settings: PillSettings::new(),
            settings_baseline: PillSettings::new(),
            settings_writable: true,
            badges: Badges {
                update: false,
                permissions: false,
            },
        }
    }
}

static STATE: Mutex<AppState> = Mutex::new(AppState::new());

fn lock_state() -> MutexGuard<'static, AppState> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

static SAMPLER: Mutex<Sampler> = Mutex::new(Sampler::new());

thread_local! {
    /// GDI text rasteriser; only ever used on the UI thread.
    static TEXT: RefCell<Option<TextPainter>> = const { RefCell::new(None) };
}

fn with_text<R>(f: impl FnOnce(&mut TextPainter) -> R) -> Option<R> {
    TEXT.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = TextPainter::new();
        }
        slot.as_mut().map(f)
    })
}

fn monitor_info() -> MONITORINFOEXW {
    MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn main() -> ExitCode {
    // CI only: render every notch view to PNG and exit before any window, hook or lock.
    if let Some(code) = viewshots::run_if_requested() {
        return code;
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            diag::win32_error("run", &error, "fatal");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), Error> {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let text = info.to_string();
        diag::write_line(&diag::format_line("error", "panic", &[("info", &text)]));
        default_hook(info);
    }));
    // Declared first, so it is released last (after windows, timer, classes and the final
    // settings write). Held for the lifetime of the process.
    let _instance = match acquire_newest_wins() {
        Ok(lock) => lock,
        Err(InstanceError::AlreadyRunning) => {
            diag::info(
                "instance_already_running",
                &[("mutex", "Local\\Pulse.Pill.v1.<user-sid>")],
            );
            return Ok(());
        }
        Err(InstanceError::Failed(error)) => {
            diag::win32_error("CreateMutexW", &error, "single_instance");
            return Err(error);
        }
    };
    load_settings();
    let (launch, writable) = {
        let app = lock_state();
        (app.settings.launch_at_login, app.settings_writable)
    };
    if writable {
        autostart::apply(launch);
    }
    let result = run_pill();
    // run_pill has returned: timer killed, panels destroyed, classes unregistered.
    usage::stop();
    update::stop();
    send::stop();
    installer::stop();
    hub::terminate();
    persist_settings();
    result
}

fn load_settings() {
    let paths = match settings::settings_paths() {
        Ok(paths) => paths,
        Err(error) => {
            diag::info(
                "settings_unavailable",
                &[
                    ("reason", error.describe().as_str()),
                    ("action", "defaults_no_write"),
                ],
            );
            lock_state().settings_writable = false;
            return;
        }
    };
    let outcome = settings::load(&paths);
    match &outcome.problem {
        Some(problem) => diag::info(
            "settings_defaulted",
            &[
                ("reason", problem.as_str()),
                ("action", "defaults_never_overwrite"),
            ],
        ),
        None if outcome.file_found => diag::info("settings_loaded", &[]),
        None => diag::info("settings_absent", &[("action", "defaults")]),
    }
    let mut app = lock_state();
    app.settings_baseline = outcome.settings.clone();
    app.settings = outcome.settings;
    app.settings_writable = outcome.writable;
}

/// Writes only when settings differ from what is on disk and the stored file was usable.
fn persist_settings() {
    // The Alt+Shift+5 toolbar can flip the screenshot destination at runtime.
    lock_state().settings.screenshot_to_desktop = shot::save_to_desktop();
    let (current, writable, changed) = {
        let app = lock_state();
        (
            app.settings.clone(),
            app.settings_writable,
            app.settings != app.settings_baseline,
        )
    };
    if !changed {
        return;
    }
    if !writable {
        diag::info(
            "settings_persist_skipped",
            &[("reason", "unusable_or_unavailable")],
        );
        return;
    }
    let result = settings::settings_paths().and_then(|paths| settings::save(&paths, &current));
    match result {
        Ok(()) => {
            lock_state().settings_baseline = current;
            diag::info("settings_saved", &[]);
        }
        Err(error) => diag::info(
            "settings_save_failed",
            &[("reason", error.describe().as_str())],
        ),
    }
}

/// The controller window's title: carries the per-user instance name, so a starting notch
/// finds the older notch of its own user (and never another user's) to retire it.
fn controller_title() -> String {
    match UserSecurity::current() {
        Ok(identity) => format!(
            "Pulse Notch Controller {}",
            instance_mutex_name(identity.sid_string())
        ),
        Err(_) => "Pulse Notch Controller".to_string(),
    }
}

/// Newest wins, as on the Mac (`retireOlderInstances`): a notch that finds an older one asks
/// it to quit (`WM_CLOSE` to its controller window, which tears down and releases the
/// instance mutex) and waits for the mutex to come free before taking over. If the older one
/// has not let go within the wait, this one gives way as before (`AlreadyRunning`).
fn acquire_newest_wins() -> Result<InstanceLock, InstanceError> {
    match InstanceLock::acquire() {
        Err(InstanceError::AlreadyRunning) => {}
        other => return other,
    }
    let title = wide(&controller_title());
    // SAFETY: both strings are NUL-terminated and outlive the call.
    let older = unsafe { FindWindowW(CONTROLLER_CLASS, PCWSTR(title.as_ptr())) };
    if let Ok(older) = older {
        diag::info("instance_retiring_older", &[]);
        // SAFETY: a plain post to another process's window; it may fail if that window is gone.
        let _ = unsafe { PostMessageW(Some(older), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match InstanceLock::acquire() {
            Err(InstanceError::AlreadyRunning) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            other => return other,
        }
    }
}

fn run_pill() -> Result<(), Error> {
    configure_dpi_awareness();
    let instance: HINSTANCE = unsafe { GetModuleHandleW(None) }?.into();

    // Drop order is reverse of declaration: timer, controller window (tears down panels and
    // the card), then the classes.
    let _controller_class =
        ClassGuard::register(CONTROLLER_CLASS, Some(controller_proc), instance)?;
    let _panel_class = ClassGuard::register(PANEL_CLASS, Some(panel_proc), instance)?;
    let _card_class = ClassGuard::register(CARD_CLASS, Some(card_proc), instance)?;
    let controller = OwnedWindow::create(
        WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
        CONTROLLER_CLASS,
        &wide(&controller_title()),
        WS_POPUP, // never shown; still a top-level window, so it receives WM_DISPLAYCHANGE
        (0, 0, 0, 0),
        instance,
    )?;
    lock_state().controller = controller.key();
    let _timer = TimerGuard::new(controller.hwnd(), TIMER_ID);
    if !arm_timer(controller.hwnd(), HIDDEN_INTERVAL_MS) {
        return Err(Error::from_win32());
    }

    // Global keyboard layer (Mac-style Alt shortcuts, screenshot hotkeys). Declared after the
    // timer so the hook thread stops first.
    let (mac_shortcuts, screenshot_shortcuts) = {
        let app = lock_state();
        (
            app.settings.mac_shortcuts,
            app.settings.screenshot_shortcuts,
        )
    };
    shot::set_save_to_desktop(lock_state().settings.screenshot_to_desktop);
    // The keys module owns the screenshot thread too and changes both live (`keys::apply`).
    let _keys = keys::start(mac_shortcuts, screenshot_shortcuts);

    usage::start(controller.key());
    claude_watch::start();
    bridge::start(
        controller.key(),
        bridge::Hooks {
            settings: || {
                let app = lock_state();
                (app.settings.clone(), app.settings_writable)
            },
            commit: |next| {
                lock_state().settings = next;
                persist_settings();
            },
            check_updates: update::check_now,
            primary_monitor: primary_monitor_id,
            machine: || lock_state().machine.clone(),
        },
    );
    update::start(controller.key(), lock_state().settings.auto_update_check);
    // The hub is the daemon: running from the notch's launch to its Quit, with or without
    // Nearby sharing, started again if it exits.
    hub::supervise();
    drive_health::start();
    let nearby = lock_state().settings.nearby_enabled;
    send::set_enabled(nearby);
    send::start(controller.key(), WM_SEND);
    let installer_auto = lock_state().settings.installer_auto;
    installer::start(controller.key(), WM_SEND, installer_auto);
    reconcile_panels();
    let interval = refresh_panels(Some(sample_once()));
    arm_timer(controller.hwnd(), interval);

    message_loop()
}

fn message_loop() -> Result<(), Error> {
    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
        match result {
            0 => return Ok(()), // WM_QUIT
            -1 => return Err(Error::from_win32()),
            _ => unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            },
        }
    }
}

/// Per-monitor V2 so window rects and monitor rects are in the same physical-pixel space.
/// Resolved dynamically (user32 exports; Win10 1703+ for V2) so no extra crate feature is
/// needed; falls back to system DPI awareness, then to unaware (reported).
fn configure_dpi_awareness() {
    type SetContext = unsafe extern "system" fn(isize) -> BOOL;
    type SetAware = unsafe extern "system" fn() -> BOOL;
    const PER_MONITOR_AWARE_V2: isize = -4;
    unsafe {
        let user32 = match GetModuleHandleW(w!("user32.dll")) {
            Ok(module) => module,
            Err(error) => {
                diag::win32_error("GetModuleHandleW", &error, "dpi user32");
                diag::info("dpi_mode", &[("mode", "unaware")]);
                return;
            }
        };
        if let Some(proc) = GetProcAddress(user32, s!("SetProcessDpiAwarenessContext")) {
            let set: SetContext = std::mem::transmute(proc);
            if set(PER_MONITOR_AWARE_V2).as_bool() {
                COORDINATES_COMPARABLE.store(true, Ordering::Relaxed);
                diag::info("dpi_mode", &[("mode", "per_monitor_v2")]);
                return;
            }
            diag::last_error("SetProcessDpiAwarenessContext", "dpi per_monitor_v2");
        }
        if let Some(proc) = GetProcAddress(user32, s!("SetProcessDPIAware")) {
            let set: SetAware = std::mem::transmute(proc);
            if set().as_bool() {
                diag::info("dpi_mode", &[("mode", "system_aware")]);
                return;
            }
            diag::last_error("SetProcessDPIAware", "dpi system");
        }
    }
    // Rects may not be comparable on mixed-DPI setups: fullscreen detection is unverified there.
    diag::info("dpi_mode", &[("mode", "unaware")]);
}

// ---------------------------------------------------------------- timer / sampling

/// Applies `ms` to the controller timer if it differs from the applied interval.
/// Returns false only when SetTimer failed.
fn arm_timer(controller: HWND, ms: u32) -> bool {
    let need = {
        let mut app = lock_state();
        if app.shutting_down || app.timer_ms == ms {
            false
        } else {
            app.timer_ms = ms;
            true
        }
    };
    if !need {
        return true;
    }
    if unsafe { SetTimer(Some(controller), TIMER_ID, ms, None) } == 0 {
        diag::last_error("SetTimer", "arm_timer");
        lock_state().timer_ms = 0; // force a retry on next arm
        return false;
    }
    true
}

fn controller_hwnd() -> Option<HWND> {
    let key = lock_state().controller;
    (key != 0).then(|| hwnd_from_key(key))
}

fn on_timer(controller: HWND) {
    let (down, retry) = {
        let app = lock_state();
        (app.shutting_down, app.reconcile_retry)
    };
    if down {
        return;
    }
    if retry {
        reconcile_panels(); // self-heal after a failed enumeration/create/move
    }
    let interval = refresh_panels(Some(sample_once()));
    arm_timer(controller, interval);
}

fn on_display_change() {
    reconcile_panels();
    let interval = refresh_panels(None);
    if let Some(controller) = controller_hwnd() {
        arm_timer(controller, interval);
    }
}

fn sample_once() -> Machine {
    SAMPLER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .sample()
}

// ---------------------------------------------------------------- monitors / panels

struct MonitorScan {
    found: Vec<MonitorSpec>,
    failed: bool,
}

unsafe extern "system" fn enum_monitor(
    monitor: HMONITOR,
    _: HDC,
    _: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // Win32 invokes this synchronously with a valid monitor; `data` is the caller's MonitorScan.
    // It only records data: no windows are created or destroyed inside the callback.
    unsafe {
        let scan = &mut *(data.0 as *mut MonitorScan);
        let mut info = monitor_info();
        if GetMonitorInfoW(monitor, &mut info.monitorInfo).as_bool() {
            scan.found.push(MonitorSpec {
                id: monitor_id(&info),
                bounds: Bounds::from(info.monitorInfo.rcMonitor),
            });
        } else {
            scan.failed = true;
            diag::last_error("GetMonitorInfoW", "enum_monitor");
        }
    }
    true.into()
}

fn enumerate_monitors() -> Option<Vec<MonitorSpec>> {
    let mut scan = MonitorScan {
        found: Vec::new(),
        failed: false,
    };
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor),
            LPARAM(&mut scan as *mut _ as isize),
        )
    }
    .as_bool();
    if !ok {
        diag::last_error("EnumDisplayMonitors", "enumerate_monitors");
        return None;
    }
    if scan.failed {
        return None; // partial view: never tear panels down on an incomplete monitor list
    }
    Some(scan.found)
}

/// Rebuild the panel set to match the current monitors (hot-plug / disconnect / reconnect /
/// resolution, DPI or arrangement change). Re-entrant requests coalesce through `ReconcileGate`.
fn reconcile_panels() {
    {
        let mut app = lock_state();
        if app.shutting_down || !app.gate.request() {
            return;
        }
    }
    loop {
        let ok = match enumerate_monitors() {
            Some(found) => apply_monitor_set(&desired_placements(&found)),
            None => false,
        };
        let again = {
            let mut app = lock_state();
            app.reconcile_retry = !ok;
            app.gate.finish()
        };
        if !again {
            break;
        }
    }
}

/// The part of the monitor a notch may dock in: its work area (the monitor less the taskbar
/// and any app bar), so a notch on the bottom edge sits above the taskbar and one on a side
/// edge beside it. The monitor's own bounds when the work area cannot be read.
fn docking_bounds(bounds: Bounds) -> Bounds {
    let rect: RECT = bounds.into();
    // SAFETY: plain queries on a rectangle; `info` has its size set, as the call requires.
    let work = unsafe {
        let monitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST);
        let mut info = monitor_info();
        GetMonitorInfoW(monitor, &mut info.monitorInfo)
            .as_bool()
            .then_some(info.monitorInfo.rcWork)
    };
    match work {
        Some(work) if work.right > work.left && work.bottom > work.top => Bounds::from(work),
        _ => bounds,
    }
}

/// The taskbar moved, resized, hid or showed (the work area changed): every notch docks
/// against the new work area again.
fn redock_panels() {
    let panels: Vec<(isize, Bounds, Slot)> = lock_state()
        .panels
        .iter()
        .map(|p| (p.window.key(), p.bounds, p.slot))
        .collect();
    for (key, bounds, slot) in panels {
        let target = notch_bounds(docking_bounds(bounds), slot);
        // SAFETY: the panel window is owned by `app.panels`; a failure just keeps the old spot.
        let _ = unsafe {
            SetWindowPos(
                hwnd_from_key(key),
                None,
                target.left,
                target.top,
                target.width(),
                target.height(),
                SWP_NOACTIVATE | SWP_NOZORDER,
            )
        };
        // A resized layered window shows nothing until its bitmap is published again.
        if let Some(panel) = lock_state()
            .panels
            .iter_mut()
            .find(|p| p.window.key() == key)
        {
            panel.drawn = None;
        }
    }
    let interval = refresh_panels(None);
    if let Some(controller) = controller_hwnd() {
        arm_timer(controller, interval);
    }
}

/// Effective DPI of the monitor with these bounds (96 when it cannot be read).
fn monitor_dpi(bounds: Bounds) -> u32 {
    let rect: RECT = bounds.into();
    let monitor = unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST) };
    let (mut x, mut y) = (0u32, 0u32);
    match unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) } {
        Ok(()) if x > 0 => x,
        _ => 96,
    }
}

/// Enabled monitors with their remembered position and DPI (defaults for unknown monitors).
fn desired_placements(found: &[MonitorSpec]) -> Vec<Placed> {
    let (settings, open) = {
        let app = lock_state();
        let open: Vec<String> = app
            .panels
            .iter()
            .filter(|p| !p.slot.folded)
            .map(|p| p.id.clone())
            .collect();
        (app.settings.clone(), open)
    };
    let size = settings.notch_scale();
    found
        .iter()
        .filter(|spec| settings.monitor(&spec.id).enabled)
        .map(|spec| Placed {
            spec: spec.clone(),
            slot: Slot {
                edge: settings.edge(&spec.id),
                along: settings.position(&spec.id),
                // A notch the pointer has opened stays open until the fold timer says so (a
                // settings change that turns folding on starts it: `refold_after_settings`).
                folded: settings.folds && !open.contains(&spec.id),
                dpi: notch_dpi(monitor_dpi(spec.bounds), size),
            },
        })
        .collect()
}

/// The DPI a notch is laid out at: its monitor's, times the chosen size. Every metric (rings,
/// text, cards) follows the DPI, so one number scales the whole notch, per monitor.
fn notch_dpi(monitor: u32, size: f32) -> u32 {
    ((monitor as f32 * size).round() as u32).clamp(48, 480)
}

/// Device name of the primary monitor, the one the hub's Edge control reports.
fn primary_monitor_id() -> Option<String> {
    // SAFETY: a plain query; the origin resolves to the primary monitor by default.
    let monitor = unsafe {
        windows::Win32::Graphics::Gdi::MonitorFromPoint(
            POINT { x: 0, y: 0 },
            windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTOPRIMARY,
        )
    };
    let mut info = monitor_info();
    // SAFETY: `info` is a MONITORINFOEXW with its size set, as the call requires.
    unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }
        .as_bool()
        .then(|| monitor_id(&info))
}

fn apply_monitor_set(desired: &[Placed]) -> bool {
    let existing: Vec<Placed> = lock_state()
        .panels
        .iter()
        .map(|p| Placed {
            spec: MonitorSpec {
                id: p.id.clone(),
                bounds: p.bounds,
            },
            slot: p.slot,
        })
        .collect();
    let mut all_ok = true;
    for action in plan_placements(&existing, desired) {
        if lock_state().shutting_down {
            return true;
        }
        match action {
            Placement::Destroy(id) => {
                let removed = {
                    let mut app = lock_state();
                    app.panels
                        .iter()
                        .position(|p| p.id == id)
                        .map(|i| app.panels.remove(i))
                };
                if let Some(mut removed) = removed {
                    // Retain ownership on failure so reconciliation retries destruction.
                    if let Err(error) = removed.window.try_destroy() {
                        diag::win32_error("DestroyWindow", &error, "remove monitor");
                        lock_state().panels.push(removed);
                        all_ok = false;
                    }
                }
            }
            Placement::Move(placed) => {
                let key = lock_state()
                    .panels
                    .iter()
                    .find(|p| p.id == placed.spec.id)
                    .map(|p| p.window.key());
                if let Some(key) = key {
                    let target = notch_bounds(docking_bounds(placed.spec.bounds), placed.slot);
                    let moved = unsafe {
                        SetWindowPos(
                            hwnd_from_key(key),
                            None,
                            target.left,
                            target.top,
                            target.width(),
                            target.height(),
                            SWP_NOACTIVATE | SWP_NOZORDER,
                        )
                    };
                    match moved {
                        Ok(()) => {
                            if let Some(p) = lock_state()
                                .panels
                                .iter_mut()
                                .find(|p| p.id == placed.spec.id)
                            {
                                p.bounds = placed.spec.bounds;
                                p.slot = placed.slot;
                                p.drawn = None; // size or DPI may have changed
                            }
                        }
                        Err(error) => {
                            // Recorded placement stays old: the next reconcile plans the move again.
                            diag::win32_error(
                                "SetWindowPos",
                                &error,
                                &format!("move monitor={}", placed.spec.id),
                            );
                            all_ok = false;
                        }
                    }
                }
            }
            Placement::Create(placed) => match create_panel(&placed) {
                Ok(panel) => lock_state().panels.push(panel),
                Err(error) => {
                    diag::win32_error(
                        "CreatePanel",
                        &error,
                        &format!("monitor={}", placed.spec.id),
                    );
                    all_ok = false;
                }
            },
        }
    }
    all_ok
}

fn create_panel(placed: &Placed) -> Result<Panel, Error> {
    let spec = &placed.spec;
    let instance: HINSTANCE = unsafe { GetModuleHandleW(None) }?.into();
    let target = notch_bounds(docking_bounds(spec.bounds), placed.slot);
    // Created hidden and without contents: the first refresh draws, then decides whether to
    // show it, so it never flashes over a fullscreen app. On error `window` drops and
    // destroys the half-built panel.
    let window = OwnedWindow::create(
        WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
        PANEL_CLASS,
        &wide(&format!("Pulse Notch {}", spec.id)),
        WS_POPUP,
        (target.left, target.top, target.width(), target.height()),
        instance,
    )?;
    send::accept_drops(window.key());
    Ok(Panel {
        id: spec.id.clone(),
        bounds: spec.bounds,
        slot: placed.slot,
        visibility: Retry::new(true),
        drawn: None,
        published: None,
        healed: None,
        window,
    })
}

fn teardown_panels() {
    let (panels, card) = {
        let mut app = lock_state();
        app.shutting_down = true;
        (std::mem::take(&mut app.panels), app.card.take())
    };
    // Destroys every window with no lock held.
    drop(panels);
    drop(card);
}

fn monitor_id(info: &MONITORINFOEXW) -> String {
    let end = info
        .szDevice
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(info.szDevice.len());
    String::from_utf16_lossy(&info.szDevice[..end])
}

// ---------------------------------------------------------------- visibility

/// Re-evaluate occupancy for every panel, show/hide on transitions, redraw when what is
/// shown changed. Returns the cadence the timer should use.
/// The notch's two dots from live state: red while a newer release is known, amber while a
/// permission needs the user. True when either changed.
fn refresh_badges() -> bool {
    let next = Badges {
        update: update::available(),
        permissions: bridge::permissions_attention(),
    };
    let mut app = lock_state();
    let changed = app.badges != next;
    app.badges = next;
    changed
}

fn refresh_panels(new_machine: Option<Machine>) -> u32 {
    refresh_badges();
    let usage = usage::snapshot();
    let (targets, own, settings_visible, cadence) = {
        let mut app = lock_state();
        if let Some(machine) = new_machine {
            app.machine = Some(machine);
        }
        let targets: Vec<(isize, Bounds, Retry<bool>)> = app
            .panels
            .iter()
            .map(|p| (p.window.key(), p.bounds, p.visibility))
            .collect();
        let mut own: Vec<isize> = targets.iter().map(|t| t.0).collect();
        if let Some(card) = &app.card {
            own.push(card.key());
        }
        (
            targets,
            own,
            app.settings.visible,
            app.settings.cadence_seconds,
        )
    };

    let mut outcomes = Vec::with_capacity(targets.len());
    for (key, bounds, mut visibility) in targets {
        let hwnd = hwnd_from_key(key);
        // Occupancy is only scanned when the pill could be shown at all.
        let suppressed = settings_visible && monitor_has_fullscreen_occupancy(bounds, &own);
        let hidden = desired_hidden(settings_visible, suppressed);
        if hidden {
            dismiss_card_for(key);
        } else {
            if !visibility.applied() {
                heal_panel(key, &own);
            }
            redraw_panel(key, &usage);
        }
        if visibility.needs(hidden) {
            let result = unsafe { set_panel_hidden(hwnd, hidden) };
            let ok = result.is_ok();
            match visibility.record(hidden, ok) {
                RetryReport::Failed => {
                    if let Err(error) = &result {
                        diag::win32_error(
                            "SetWindowPos",
                            error,
                            if hidden { "hide" } else { "show" },
                        );
                    }
                }
                RetryReport::Recovered(count) => diag::info(
                    "panel_transition_recovered",
                    &[
                        ("op", if hidden { "hide" } else { "show" }),
                        ("failures", count.to_string().as_str()),
                    ],
                ),
                RetryReport::Applied | RetryReport::StillFailing(_) => {}
            }
            if ok && !hidden {
                // A window that was hidden may come back without its layered contents: publish
                // the bitmap again now that it is shown, whatever `drawn` says.
                if let Some(panel) = lock_state()
                    .panels
                    .iter_mut()
                    .find(|p| p.window.key() == key)
                {
                    panel.drawn = None;
                }
                redraw_panel(key, &usage);
            }
        }
        outcomes.push((key, visibility));
    }

    let (total, hidden_count) = {
        let mut app = lock_state();
        for (key, visibility) in outcomes {
            if let Some(panel) = app.panels.iter_mut().find(|p| p.window.key() == key) {
                panel.visibility = visibility;
            }
        }
        (
            app.panels.len(),
            app.panels.iter().filter(|p| p.visibility.applied()).count(),
        )
    };
    sync_card();
    interval_ms(cadence, total, hidden_count)
}

/// Seconds a shown notch may go without a successful publish before it is redrawn anyway.
const REPUBLISH_AFTER: Duration = Duration::from_secs(300);
/// Least time between two re-homes or raises of one notch.
const HEAL_EVERY: Duration = Duration::from_secs(120);

/// Cheap self-heal for a notch that is shown and not suppressed but may not be on screen:
/// a window cloaked off the current virtual desktop is moved back to it, a window that lost
/// the top of the topmost band (or is covered by another process's window) is raised again,
/// and a bitmap not published for `REPUBLISH_AFTER` is rendered and published again. Every
/// action leaves one `panel_self_heal` line in `notch.log`. Never activates anything.
fn heal_panel(key: isize, own: &[isize]) {
    let hwnd = hwnd_from_key(key);
    let now = Instant::now();
    let (monitor, stale, may_act) = {
        let app = lock_state();
        let Some(panel) = app.panels.iter().find(|p| p.window.key() == key) else {
            return;
        };
        (
            panel.id.clone(),
            panel
                .published
                .map(|at| now.duration_since(at))
                .filter(|age| *age >= REPUBLISH_AFTER),
            panel
                .healed
                .is_none_or(|at| now.duration_since(at) >= HEAL_EVERY),
        )
    };
    let mut acted = false;
    if may_act {
        let mut cloaked = 0u32;
        // SAFETY: `cloaked` outlives the call and has the size the attribute needs.
        let known = unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED,
                &mut cloaked as *mut _ as *mut _,
                size_of::<u32>() as u32,
            )
        }
        .is_ok();
        if known && cloaked != 0 {
            // DWM_CLOAKED_SHELL (2) is the shell hiding it: the window sits on another
            // virtual desktop. Follow the desktop of the foreground window.
            let moved = if cloaked & 2 != 0 {
                let card = lock_state().card.as_ref().map(OwnedWindow::key);
                let mut targets = vec![hwnd];
                targets.extend(card.map(hwnd_from_key));
                move_to_foreground_desktop(&targets)
            } else {
                Err(Error::from(E_FAIL))
            };
            diag::info(
                "panel_self_heal",
                &[
                    ("reason", "cloaked"),
                    ("monitor", monitor.as_str()),
                    ("cloaked", cloaked.to_string().as_str()),
                    ("moved", moved.is_ok().to_string().as_str()),
                ],
            );
            acted = true;
        } else if let Some(cover) = covering_window(hwnd, own) {
            // SAFETY: re-stacks our own window only; no move, size or activation.
            let raised = unsafe {
                SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                )
            };
            diag::info(
                "panel_self_heal",
                &[
                    ("reason", "covered"),
                    ("monitor", monitor.as_str()),
                    ("by", cover.as_str()),
                    ("raised", raised.is_ok().to_string().as_str()),
                ],
            );
            acted = true;
        }
    }
    let republish = acted || stale.is_some();
    if let Some(panel) = lock_state()
        .panels
        .iter_mut()
        .find(|p| p.window.key() == key)
        .filter(|_| republish)
    {
        panel.drawn = None;
        if acted {
            panel.healed = Some(now);
        }
    }
    if let Some(age) = stale {
        diag::info(
            "panel_self_heal",
            &[
                ("reason", "stale"),
                ("monitor", monitor.as_str()),
                ("age_s", age.as_secs().to_string().as_str()),
            ],
        );
    }
}

/// Moves our windows to the virtual desktop the foreground window is on (the one the user
/// is looking at). `IVirtualDesktopManager::MoveWindowToDesktop` only accepts windows of
/// this process, which these are.
fn move_to_foreground_desktop(targets: &[HWND]) -> Result<(), Error> {
    static COM_STARTED: AtomicBool = AtomicBool::new(false);
    // SAFETY: plain COM calls on the UI thread; the interface is released on drop and the
    // GUID outlives the calls.
    unsafe {
        if !COM_STARTED.swap(true, Ordering::Relaxed) {
            // S_FALSE or a changed apartment mode still leave COM usable on this thread.
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let manager: IVirtualDesktopManager =
            CoCreateInstance(&VirtualDesktopManager, None::<&IUnknown>, CLSCTX_ALL)?;
        let front = GetForegroundWindow();
        if front.0.is_null() {
            return Err(Error::from(E_FAIL));
        }
        let desktop = manager.GetWindowDesktopId(front)?;
        let mut result = Ok(());
        for target in targets {
            if let Err(error) = manager.MoveWindowToDesktop(*target, &desktop) {
                result = Err(error);
            }
        }
        result
    }
}

/// The class of a window of another process that sits above `hwnd` and overlaps it, or
/// `"not_topmost"` when `hwnd` lost its topmost flag; `None` when nothing covers it.
fn covering_window(hwnd: HWND, own: &[isize]) -> Option<String> {
    let style = query_window_style(hwnd, GWL_EXSTYLE)?;
    if style & WS_EX_TOPMOST.0 == 0 {
        return Some("not_topmost".to_string());
    }
    let mut mine = RECT::default();
    // SAFETY: `mine` outlives the call.
    unsafe { GetWindowRect(hwnd, &mut mine) }.ok()?;
    // SAFETY: no arguments.
    let pid = unsafe { GetCurrentProcessId() };
    let mut above = hwnd;
    // Only the windows stacked above ours: a short walk (the topmost band is small).
    for _ in 0..64 {
        // SAFETY: GW_HWNDPREV on a live handle; Err means nothing is above.
        above = unsafe { GetWindow(above, GW_HWNDPREV) }.ok()?;
        if above.0.is_null() {
            return None;
        }
        // SAFETY: `above` came from the window list just now.
        let visible = unsafe { IsWindowVisible(above) }.as_bool();
        if !visible || own.contains(&hwnd_key(above)) {
            continue;
        }
        let mut other = 0u32;
        // SAFETY: `other` outlives the call.
        unsafe { GetWindowThreadProcessId(above, Some(&mut other)) };
        if other == pid {
            continue;
        }
        let mut cloaked = 0u32;
        // SAFETY: `cloaked` outlives the call and has the size the attribute needs.
        let cloak_ok = unsafe {
            DwmGetWindowAttribute(
                above,
                DWMWA_CLOAKED,
                &mut cloaked as *mut _ as *mut _,
                size_of::<u32>() as u32,
            )
        }
        .is_ok();
        let mut rect = RECT::default();
        // SAFETY: `rect` outlives the call.
        if (cloak_ok && cloaked != 0) || unsafe { GetWindowRect(above, &mut rect) }.is_err() {
            continue;
        }
        let overlaps = rect.left < mine.right
            && rect.right > mine.left
            && rect.top < mine.bottom
            && rect.bottom > mine.top;
        if overlaps {
            let mut class = [0u16; 128];
            // SAFETY: the buffer outlives the call.
            let length = unsafe { GetClassNameW(above, &mut class) }.max(0) as usize;
            return Some(String::from_utf16_lossy(&class[..length]));
        }
    }
    None
}

/// Draws the panel's bitmap when what it shows (cells, edge, folded state, DPI, badges)
/// changed.
fn redraw_panel(key: isize, usage: &[Usage; 2]) {
    let ring = send::ring();
    let (views, slot, badges, handle, press) = {
        let app = lock_state();
        let Some(panel) = app.panels.iter().find(|p| p.window.key() == key) else {
            return;
        };
        // The folded pill shows no readings, so its identity is the empty list.
        let views = if panel.slot.folded {
            Vec::new()
        } else {
            layout::views(app.machine.as_ref(), usage, &ring)
        };
        // The settings handle part the pointer is on (the folded pill has none).
        let handle = app
            .ui
            .handle
            .filter(|h| h.0 == key && !panel.slot.folded)
            .map(|h| h.1);
        let press = app
            .ui
            .pressed
            .filter(|p| p.0 == key && !panel.slot.folded)
            .map(|p| p.1);
        let drawn = (
            &views,
            panel.slot.edge,
            panel.slot.folded,
            panel.slot.dpi,
            app.badges,
            handle,
            press,
        );
        if panel.drawn.as_ref().is_some_and(|shown| {
            (
                &shown.0, shown.1, shown.2, shown.3, shown.4, shown.5, shown.6,
            ) == drawn
        }) {
            return;
        }
        (views, panel.slot, app.badges, handle, press)
    };
    let Some(canvas) = with_text(|text| {
        render::render_notch(
            &views,
            slot.edge,
            slot.folded,
            badges,
            slot.dpi,
            text,
            (handle, press),
        )
    }) else {
        return;
    };
    // A bitmap with no visible pixel would blank the notch while `drawn` called it current:
    // never publish one; `drawn` stays stale so the next refresh renders again.
    if canvas.pixels.iter().all(|pixel| pixel >> 24 == 0) {
        diag::info(
            "panel_empty_bitmap",
            &[
                ("width", canvas.width.to_string().as_str()),
                ("height", canvas.height.to_string().as_str()),
                ("folded", slot.folded.to_string().as_str()),
            ],
        );
        return;
    }
    match present(hwnd_from_key(key), &canvas, None) {
        Ok(()) => {
            if let Some(panel) = lock_state()
                .panels
                .iter_mut()
                .find(|p| p.window.key() == key)
            {
                panel.published = Some(Instant::now());
                panel.drawn = Some((
                    views,
                    slot.edge,
                    slot.folded,
                    slot.dpi,
                    badges,
                    handle,
                    press,
                ));
            }
        }
        // `drawn` stays stale, so the next refresh retries.
        Err(error) => diag::win32_error("UpdateLayeredWindow", &error, "panel"),
    }
}

/// Show (topmost, no activation) or hide a window through SetWindowPos so failures are
/// observable (ShowWindow only reports prior visibility, never failure).
unsafe fn set_panel_hidden(hwnd: HWND, hidden: bool) -> Result<(), Error> {
    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
    unsafe {
        if hidden {
            SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                flags | SWP_NOZORDER | SWP_HIDEWINDOW,
            )
        } else {
            SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags | SWP_SHOWWINDOW)
        }
    }
}

struct ScanContext<'a> {
    monitor: RECT,
    own: &'a [isize],
    decided: bool,
    blocked: bool,
}

fn monitor_has_fullscreen_occupancy(monitor: Bounds, own: &[isize]) -> bool {
    if !COORDINATES_COMPARABLE.load(Ordering::Relaxed) {
        return false;
    }
    let mut context = ScanContext {
        monitor: monitor.into(),
        own,
        decided: false,
        blocked: false,
    };
    // The callback only reads window attributes; context outlives the synchronous call.
    let result = unsafe {
        EnumWindows(
            Some(enum_visible_window),
            LPARAM(&mut context as *mut _ as isize),
        )
    };
    if result.is_err() && !context.decided {
        // Stopping early (decided) makes EnumWindows return an error by design; anything else is real.
        diag::last_error("EnumWindows", "occupancy_scan");
    }
    context.blocked
}

unsafe extern "system" fn enum_visible_window(hwnd: HWND, data: LPARAM) -> BOOL {
    // EnumWindows supplies live HWNDs; data points to the caller's stack ScanContext.
    unsafe {
        let ctx = &mut *(data.0 as *mut ScanContext);
        if ctx.decided {
            return false.into();
        }
        if !IsWindowVisible(hwnd).as_bool() || ctx.own.contains(&hwnd_key(hwnd)) {
            return true.into();
        }
        // Suppression needs reliable evidence: any query that fails for a window that could
        // be the topmost cover decides "unknown" (pill stays visible), never "covered".
        let Some(is_shell) = is_shell_desktop_window(hwnd) else {
            ctx.decided = true;
            return false.into();
        };
        if is_shell {
            return true.into();
        }
        SetLastError(ERROR_SUCCESS);
        let owned = match GetWindow(hwnd, GW_OWNER) {
            Ok(owner) => !owner.0.is_null(),
            // GetWindow returns null both for "no owner" and for failure; only a non-zero
            // last error is a failure.
            Err(_) if GetLastError() == ERROR_SUCCESS => false,
            Err(_) => {
                ctx.decided = true;
                return false.into();
            }
        };
        let Some(ex_style) = query_window_style(hwnd, GWL_EXSTYLE) else {
            ctx.decided = true;
            return false.into();
        };
        if owned || is_tool_window_ex_style(ex_style) {
            return true.into();
        }
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut _,
            size_of::<u32>() as u32,
        )
        .is_err()
        {
            ctx.decided = true; // unknown attributes keep pill visible
            return false.into();
        }
        if cloaked != 0 {
            return true.into();
        }
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            ctx.decided = true; // no coordinate evidence: do not suppress
            return false.into();
        }
        let Some(style) = query_window_style(hwnd, GWL_STYLE) else {
            ctx.decided = true;
            return false.into();
        };
        match classify_window(rect, ctx.monitor, style) {
            Occupancy::Outside => true.into(),
            Occupancy::Covers => {
                ctx.decided = true;
                ctx.blocked = true;
                false.into()
            }
            Occupancy::Partial => {
                ctx.decided = true;
                false.into()
            }
        }
    }
}

fn query_window_style(hwnd: HWND, index: WINDOW_LONG_PTR_INDEX) -> Option<u32> {
    unsafe {
        SetLastError(ERROR_SUCCESS);
        let value = GetWindowLongPtrW(hwnd, index);
        if value == 0 && GetLastError() != ERROR_SUCCESS {
            return None;
        }
        Some(value as u32)
    }
}

/// None when the class name cannot be read (zero length is the API's failure result).
fn is_shell_desktop_window(hwnd: HWND) -> Option<bool> {
    let mut class = [0u16; 256];
    let length = unsafe { GetClassNameW(hwnd, &mut class) };
    if length <= 0 {
        return None;
    }
    let name = String::from_utf16_lossy(&class[..length as usize]);
    Some(is_shell_class_name(&name))
}

// ---------------------------------------------------------------- hover card, menu, drag

const MSG_MOUSELEAVE: u32 = 0x02A3;

fn lparam_point(lparam: LPARAM) -> (i32, i32) {
    (
        (lparam.0 & 0xFFFF) as i16 as i32,
        ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
    )
}

fn alt_down() -> bool {
    let state = unsafe { GetKeyState(VK_MENU.0 as i32) };
    state < 0
}

/// Cell under a point in a panel's own pixels; none on the folded pill.
fn cell_under(key: isize, x: i32, y: i32) -> Option<usize> {
    let app = lock_state();
    let panel = app.panels.iter().find(|p| p.window.key() == key)?;
    if panel.slot.folded {
        return None;
    }
    layout::cell_at_for(panel.slot.edge, x, y, panel.slot.dpi)
}

/// Part of the settings handle under a point in a panel's own pixels; none on the folded
/// pill. The grip counts only while it is out (the pointer is on the handle).
fn handle_under(key: isize, x: i32, y: i32) -> Option<Handle> {
    let app = lock_state();
    let panel = app.panels.iter().find(|p| p.window.key() == key)?;
    if panel.slot.folded {
        return None;
    }
    let grip_out = app.ui.handle.is_some_and(|h| h.0 == key);
    layout::handle_at(panel.slot.edge, x, y, panel.slot.dpi, grip_out)
}

/// Creates the shared card window on first use; returns its key.
fn ensure_card() -> Option<isize> {
    let existing = lock_state().card.as_ref().map(OwnedWindow::key);
    if existing.is_some() {
        return existing;
    }
    if lock_state().shutting_down {
        return None;
    }
    let instance: HINSTANCE = unsafe { GetModuleHandleW(None) }.ok()?.into();
    let window = match OwnedWindow::create(
        WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
        CARD_CLASS,
        &wide("Pulse Notch Card"),
        WS_POPUP,
        (0, 0, 1, 1),
        instance,
    ) {
        Ok(window) => window,
        Err(error) => {
            diag::win32_error("CreateCard", &error, "card");
            return None;
        }
    };
    let key = window.key();
    lock_state().card = Some(window);
    Some(key)
}

fn hide_card() {
    let (key, release) = {
        let mut app = lock_state();
        app.ui.card_shown = None;
        app.ui.card_hover = false;
        app.ui.card_hit = None;
        let release = std::mem::take(&mut app.ui.popup_hover_sent);
        (app.card.as_ref().map(OwnedWindow::key), release)
    };
    if let Some(key) = key {
        let _ = unsafe { set_panel_hidden(hwnd_from_key(key), true) };
    }
    if release {
        send::popup_hover(false);
    }
    sync_card_animation(false);
    sync_send_hover();
}

/// Clears hover/menu/card state that belongs to `key` (the panel was hidden or removed).
fn dismiss_card_for(key: isize) {
    let relevant = {
        let mut app = lock_state();
        let relevant = app.ui.hover.is_some_and(|h| h.0 == key)
            || app.ui.menu.is_some_and(|m| m.0 == key)
            || app.ui.card_shown.as_ref().is_some_and(|c| c.key == key);
        if relevant {
            app.ui.hover = None;
            app.ui.menu = None;
        }
        if app.ui.handle.is_some_and(|h| h.0 == key) {
            app.ui.handle = None;
        }
        relevant
    };
    if relevant {
        stop_menu_timer();
        hide_card();
    }
}

fn stop_menu_timer() {
    if let Some(controller) = controller_hwnd() {
        let _ = unsafe { KillTimer(Some(controller), MENU_TIMER_ID) };
    }
}

/// Ctrl+V belongs to the Send cell while the pointer is on it or on its card.
fn sync_send_hover() {
    let over = {
        let app = lock_state();
        app.ui.hover.is_some_and(|h| h.1 == SEND_CELL)
            || (app.ui.card_hover
                && app
                    .ui
                    .card_shown
                    .as_ref()
                    .is_some_and(|c| c.cell == SEND_CELL))
    };
    send::set_hover(over);
}

/// Decides which card should be up and shows, updates or hides it: the card of the hovered
/// cell, else (Send popup news, no hover needed) the popup under the first visible notch.
/// The Quit menu and a notch drag own the card window and are left alone.
fn sync_card() {
    let usage = usage::snapshot();
    let now = usage::now_secs();
    let popup = send::popup_panel();
    let notice = update::panel().or_else(|| alerts::panel(now));
    sync_notice_timer(notice.is_some());
    let notice_cell = Cell::ALL
        .iter()
        .position(|c| *c == Cell::Claude)
        .unwrap_or(0);
    let (target, current_empty, show_notice) = {
        let app = lock_state();
        if app.shutting_down || app.ui.menu.is_some() || app.ui.drag.is_some() {
            return;
        }
        // The pointer on the card itself keeps it; a popup needs no hover at all.
        let on_card = if app.ui.card_hover {
            app.ui
                .card_shown
                .as_ref()
                .filter(|s| !s.notice)
                .map(|s| (s.key, s.cell))
        } else {
            None
        };
        // A sharing popup always hangs from the Send ring, whichever ring was hovered last:
        // on the notch the pointer is on, else the card already up, else the first visible.
        let popup_target = if popup.is_some() {
            let key = app
                .ui
                .hover
                .map(|h| h.0)
                .or_else(|| {
                    app.ui
                        .card_shown
                        .as_ref()
                        .filter(|s| s.popup)
                        .map(|s| s.key)
                })
                .or_else(|| {
                    app.panels
                        .iter()
                        .find(|p| !p.visibility.applied())
                        .map(|p| p.window.key())
                });
            key.map(|key| (key, SEND_CELL))
        } else {
            None
        };
        // A usage alert or the update card waits while a ring is hovered.
        let show_notice = notice.is_some() && app.ui.hover.is_none();
        let notice_target = if show_notice {
            app.panels
                .iter()
                .find(|p| !p.visibility.applied())
                .map(|p| (p.window.key(), notice_cell))
        } else {
            None
        };
        (
            notice_target.or(popup_target).or(app.ui.hover).or(on_card),
            app.ui.card_shown.is_none(),
            show_notice,
        )
    };
    let Some((key, cell)) = target else {
        if !current_empty {
            hide_card();
        }
        return;
    };
    let (mut panel, is_popup) = match notice {
        Some(panel) if show_notice => (panel, false),
        _ => {
            let app = lock_state();
            card::panel_for(Cell::ALL[cell], app.machine.as_ref(), &usage, now, popup)
        }
    };
    // The shown card carries the tail `show_card` gave it; compare the content without it.
    panel.content.tail = lock_state()
        .ui
        .card_shown
        .as_ref()
        .and_then(|shown| shown.panel.content.tail);
    let unchanged = lock_state().ui.card_shown.as_ref().is_some_and(|shown| {
        shown.key == key
            && shown.cell == cell
            && shown.popup == is_popup
            && shown.notice == show_notice
            && shown.panel == panel
    });
    if !unchanged {
        show_card(key, cell, panel, is_popup, show_notice);
    }
    // A popup that replaced the one under the pointer has not heard about the hover yet.
    let announce = {
        let mut app = lock_state();
        let over_popup = app.ui.card_hover && app.ui.card_shown.as_ref().is_some_and(|c| c.popup);
        let announce = over_popup && !app.ui.popup_hover_sent;
        if announce {
            app.ui.popup_hover_sent = true;
        }
        announce
    };
    if announce {
        send::popup_hover(true);
    }
    sync_send_hover();
}

/// Shows (or updates) `panel` as the card of `cell` of the notch `key`, beside the notch on
/// the side away from its screen edge.
fn show_card(key: isize, cell: usize, mut panel: send::Panel, popup: bool, notice: bool) {
    let (dpi, monitor, edge, folded) = {
        let app = lock_state();
        if app.shutting_down {
            return;
        }
        let Some(notch) = app.panels.iter().find(|p| p.window.key() == key) else {
            return;
        };
        (
            notch.slot.dpi,
            docking_bounds(notch.bounds),
            notch.slot.edge,
            notch.slot.folded,
        )
    };
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd_from_key(key), &mut rect) }.is_err() {
        return;
    }
    let s = layout::scale(dpi);
    // The card's tail leaves the side facing the notch; its offset (set once the card is
    // placed) keeps the point on the hovered ring when the monitor pushes the card aside.
    panel.content.tail = Some(card::Tail { edge, offset: 0 });
    let Some(card_size) = with_text(|text| render::card_size(&panel.content, dpi, text)) else {
        return;
    };
    let (ring_x, ring_y) = layout::ring_center(edge, cell, dpi);
    // A popup pinned to the folded pill hangs from the pill's middle.
    let centre = match (edge.is_vertical(), folded) {
        (true, true) => (rect.top + rect.bottom) / 2,
        (true, false) => rect.top + ring_y as i32,
        (false, true) => (rect.left + rect.right) / 2,
        (false, false) => rect.left + ring_x as i32,
    };
    let (x, y) = layout::card_origin(
        edge,
        (rect.left, rect.top, rect.right, rect.bottom),
        centre,
        card_size,
        (layout::CARD_GAP * s).round() as i32,
        (monitor.left, monitor.top, monitor.right, monitor.bottom),
    );
    let offset = if edge.is_vertical() {
        centre - (y + card_size.1 / 2)
    } else {
        centre - (x + card_size.0 / 2)
    };
    panel.content.tail = Some(card::Tail { edge, offset });
    let Some(card_key) = ensure_card() else {
        return;
    };
    let live = panel.live();
    let phase = card_phase(&panel.content);
    let hover = lock_state().ui.card_hit;
    let Some(canvas) =
        with_text(|text| render::render_card_hover(&panel.content, &live, dpi, text, phase, hover))
    else {
        return;
    };
    let hwnd = hwnd_from_key(card_key);
    if let Err(error) = present(hwnd, &canvas, Some((x, y))) {
        diag::win32_error("UpdateLayeredWindow", &error, "card");
        return;
    }
    if let Err(error) = unsafe { set_panel_hidden(hwnd, false) } {
        diag::win32_error("SetWindowPos", &error, "card show");
        return;
    }
    let animated = render::animated(&panel.content);
    lock_state().ui.card_shown = Some(Shown {
        key,
        cell,
        panel,
        popup,
        notice,
    });
    sync_card_animation(animated);
}

/// Where a moving card is in its loop (0..1), or `None` for a card that holds still.
fn card_phase(content: &card::CardContent) -> Option<f32> {
    render::animated(content).then(|| {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        (ms % ANIM_PERIOD_MS) as f32 / ANIM_PERIOD_MS as f32
    })
}

/// Starts or stops the timer that redraws a moving card.
fn sync_card_animation(active: bool) {
    if ANIM_TIMER_ARMED.swap(active, Ordering::Relaxed) == active {
        return;
    }
    if let Some(controller) = controller_hwnd() {
        if active {
            let _ = unsafe { SetTimer(Some(controller), ANIM_TIMER_ID, ANIM_MS, None) };
        } else {
            let _ = unsafe { KillTimer(Some(controller), ANIM_TIMER_ID) };
        }
    }
}

/// The card on screen, its DPI and its window, when one is up.
fn shown_card() -> Option<(send::Panel, u32, isize)> {
    let app = lock_state();
    let shown = app.ui.card_shown.as_ref()?;
    let dpi = app
        .panels
        .iter()
        .find(|p| p.window.key() == shown.key)
        .map(|p| p.slot.dpi)?;
    let window = app.card.as_ref().map(OwnedWindow::key)?;
    Some((shown.panel.clone(), dpi, window))
}

/// One frame of a moving card, drawn in place.
fn animate_card() {
    let Some((panel, dpi, window)) = shown_card() else {
        sync_card_animation(false);
        return;
    };
    if !render::animated(&panel.content) {
        sync_card_animation(false);
        return;
    }
    redraw_card(&panel, dpi, window);
}

/// Draws `panel` again in its window, with the hover plate on the control under the pointer.
fn redraw_card(panel: &send::Panel, dpi: u32, window: isize) {
    let live = panel.live();
    let phase = card_phase(&panel.content);
    let hover = lock_state().ui.card_hit;
    let Some(canvas) =
        with_text(|text| render::render_card_hover(&panel.content, &live, dpi, text, phase, hover))
    else {
        return;
    };
    let _ = present(hwnd_from_key(window), &canvas, None);
}

/// The live control (one with an action) under `(x, y)` of the card window.
fn card_hit_at(x: i32, y: i32) -> Option<card::Hit> {
    let app = lock_state();
    let shown = app.ui.card_shown.as_ref()?;
    let dpi = app
        .panels
        .iter()
        .find(|p| p.window.key() == shown.key)
        .map(|p| p.slot.dpi)?;
    let hit =
        with_text(|text| render::hit_at(&shown.panel.content, dpi, text, (x, y))).flatten()?;
    shown.panel.action(hit).map(|_| hit)
}

/// The primary button went down (or came up) over the card: the plate under the pointer reads
/// pressed (lighter than hovered) until it is released.
fn set_card_pressed(down: bool) {
    if !render::set_pressed(down) {
        return;
    }
    if lock_state().ui.menu.is_some() {
        redraw_menu();
    } else if let Some((panel, dpi, window)) = shown_card() {
        redraw_card(&panel, dpi, window);
    }
}

/// The pointer entered or left the Quit menu: its plate lights or goes out.
fn set_menu_hot(hot: bool) {
    {
        let mut app = lock_state();
        if app.ui.menu_hot == hot {
            return;
        }
        app.ui.menu_hot = hot;
    }
    redraw_menu();
}

/// Draws the open Quit menu again in place (hover and pressed states).
fn redraw_menu() {
    let (dpi, window, hot) = {
        let app = lock_state();
        let Some((key, _)) = app.ui.menu else {
            return;
        };
        let Some(dpi) = app
            .panels
            .iter()
            .find(|p| p.window.key() == key)
            .map(|p| p.slot.dpi)
        else {
            return;
        };
        let Some(window) = app.card.as_ref().map(OwnedWindow::key) else {
            return;
        };
        (dpi, window, app.ui.menu_hot)
    };
    let Some(canvas) = with_text(|text| render::render_menu_hover(dpi, text, hot)) else {
        return;
    };
    let _ = present(hwnd_from_key(window), &canvas, None);
}

/// The pointer moved over the open Quit menu: lights its plate and asks for the leave message.
fn on_menu_mouse_move(hwnd: HWND) {
    set_menu_hot(true);
    let armed = std::mem::replace(&mut lock_state().ui.card_tracking, true);
    if !armed {
        let mut request = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        if unsafe { TrackMouseEvent(&mut request) }.is_err() {
            lock_state().ui.card_tracking = false;
        }
    }
}

/// Moves the hover plate to the control under the pointer (or off every control).
fn set_card_hit(hit: Option<card::Hit>) {
    let (changed, shown) = {
        let mut app = lock_state();
        let changed = app.ui.card_hit != hit;
        app.ui.card_hit = hit;
        (changed, app.ui.card_shown.is_some())
    };
    if !changed || !shown {
        return;
    }
    if let Some((panel, dpi, window)) = shown_card() {
        redraw_card(&panel, dpi, window);
    }
}

/// Starts or stops the one-second tick that expires alert cards.
fn sync_notice_timer(active: bool) {
    if NOTICE_TIMER_ARMED.swap(active, Ordering::Relaxed) == active {
        return;
    }
    if let Some(controller) = controller_hwnd() {
        if active {
            let _ = unsafe { SetTimer(Some(controller), NOTICE_TIMER_ID, 1000, None) };
        } else {
            let _ = unsafe { KillTimer(Some(controller), NOTICE_TIMER_ID) };
        }
    }
}

fn arm_leave(hwnd: HWND) {
    let key = hwnd_key(hwnd);
    {
        let mut app = lock_state();
        if app.ui.tracking == Some(key) {
            return;
        }
        app.ui.tracking = Some(key);
    }
    let mut request = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    if unsafe { TrackMouseEvent(&mut request) }.is_err() {
        lock_state().ui.tracking = None;
    }
}

fn clear_hover(key: isize) {
    let was_hovering = {
        let mut app = lock_state();
        if app.ui.menu.is_some() || app.ui.drag.is_some() {
            false
        } else if app.ui.hover.is_some_and(|h| h.0 == key) {
            app.ui.hover = None;
            true
        } else {
            false
        }
    };
    if was_hovering {
        sync_card();
    }
}

/// The pointer left a notch (or the Send card). Over the Send cell the card is interactive, so
/// give the pointer a moment to reach it before dismissing.
fn arm_hover_grace() {
    if let Some(controller) = controller_hwnd() {
        let _ = unsafe { SetTimer(Some(controller), HOVER_TIMER_ID, HOVER_GRACE_MS, None) };
    }
}

fn hover_grace_tick() {
    if let Some(controller) = controller_hwnd() {
        let _ = unsafe { KillTimer(Some(controller), HOVER_TIMER_ID) };
    }
    let key = {
        let app = lock_state();
        match app.ui.hover {
            Some((key, _)) if !app.ui.card_hover && app.ui.tracking != Some(key) => key,
            _ => return,
        }
    };
    clear_hover(key);
}

/// Opens or folds one notch in place: it keeps its edge and centre, only its size changes.
/// Opening starts the fold timer that closes it again once the pointer has left.
fn set_folded(key: isize, folded: bool) {
    let (target, slot) = {
        let app = lock_state();
        let Some(panel) = app.panels.iter().find(|p| p.window.key() == key) else {
            return;
        };
        if panel.slot.folded == folded {
            return;
        }
        let slot = Slot {
            folded,
            ..panel.slot
        };
        (notch_bounds(docking_bounds(panel.bounds), slot), slot)
    };
    let moved = unsafe {
        SetWindowPos(
            hwnd_from_key(key),
            None,
            target.left,
            target.top,
            target.width(),
            target.height(),
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
    if let Err(error) = moved {
        diag::win32_error("SetWindowPos", &error, "fold");
        return;
    }
    {
        let mut app = lock_state();
        if let Some(panel) = app.panels.iter_mut().find(|p| p.window.key() == key) {
            panel.slot = slot;
            panel.drawn = None;
        }
        app.ui.fold_outside = 0;
        if folded {
            app.ui.hover = None;
            app.ui.handle = None;
        }
    }
    redraw_panel(key, &usage::snapshot());
    if let Some(controller) = controller_hwnd() {
        if folded {
            sync_card();
        } else {
            let _ = unsafe { SetTimer(Some(controller), FOLD_TIMER_ID, FOLD_POLL_MS, None) };
        }
    }
}

/// A hub setting changed how the notches are placed. Reconciling keeps a notch the pointer
/// opened open, and only opening a notch starts the fold timer, so turning "On hover" on
/// while the notch was pinned open (Always show) would have left it open for good. Start the
/// timer and let the first tick fold it, as the Mac's "On hover" folds the notch at once,
/// unless the pointer is on it.
fn refold_after_settings() {
    let pending = {
        let mut app = lock_state();
        let pending = app.settings.folds && app.panels.iter().any(|p| !p.slot.folded);
        if pending {
            app.ui.fold_outside = FOLD_GRACE_TICKS - 1;
        }
        pending
    };
    if !pending {
        return;
    }
    if let Some(controller) = controller_hwnd() {
        let _ = unsafe { SetTimer(Some(controller), FOLD_TIMER_ID, FOLD_POLL_MS, None) };
    }
}

/// While any notch is open: folds it once the pointer has been away from it (and from its
/// card) for a few polls. A menu, a drag, a pointer on the card or a sharing popup keep it
/// open. Stops itself when every notch is folded.
fn fold_tick() {
    let mut cursor = POINT::default();
    if unsafe { GetCursorPos(&mut cursor) }.is_err() {
        return;
    }
    let (open, keep, card) = {
        let app = lock_state();
        let open: Vec<isize> = app
            .panels
            .iter()
            .filter(|p| !p.slot.folded)
            .map(|p| p.window.key())
            .collect();
        let keep = app.ui.menu.is_some()
            || app.ui.drag.is_some()
            || app.ui.card_hover
            || app
                .ui
                .card_shown
                .as_ref()
                .is_some_and(|c| c.popup || c.notice);
        (open, keep, app.card.as_ref().map(OwnedWindow::key))
    };
    if open.is_empty() || !lock_state().settings.folds {
        if let Some(controller) = controller_hwnd() {
            let _ = unsafe { KillTimer(Some(controller), FOLD_TIMER_ID) };
        }
        return;
    }
    let over = |key: isize| {
        let mut rect = RECT::default();
        unsafe { GetWindowRect(hwnd_from_key(key), &mut rect) }.is_ok()
            && cursor.x >= rect.left
            && cursor.x < rect.right
            && cursor.y >= rect.top
            && cursor.y < rect.bottom
    };
    let inside = keep
        || open.iter().any(|key| over(*key))
        || (lock_state().ui.card_shown.is_some() && card.is_some_and(over));
    let fold = {
        let mut app = lock_state();
        if inside {
            app.ui.fold_outside = 0;
            false
        } else {
            app.ui.fold_outside += 1;
            app.ui.fold_outside >= FOLD_GRACE_TICKS
        }
    };
    if fold {
        for key in open {
            set_folded(key, true);
        }
    }
}

fn on_mouse_move(hwnd: HWND, x: i32, y: i32) {
    let key = hwnd_key(hwnd);
    if lock_state()
        .ui
        .drag
        .as_ref()
        .is_some_and(|d| d.panel == key)
    {
        drag_to(hwnd);
        return;
    }
    arm_leave(hwnd);
    // The pointer reached the resting pill: open the notch under it.
    if lock_state()
        .panels
        .iter()
        .any(|p| p.window.key() == key && p.slot.folded)
    {
        set_folded(key, false);
        return;
    }
    // The settings handle (button and grip) takes the pointer ahead of the rings.
    if update_handle(key, x, y) {
        return;
    }
    match cell_under(key, x, y) {
        Some(cell) => {
            let changed = {
                let mut app = lock_state();
                if app.ui.menu.is_some() || app.ui.hover == Some((key, cell)) {
                    false
                } else {
                    app.ui.hover = Some((key, cell));
                    true
                }
            };
            if changed {
                sync_card();
            }
        }
        None => clear_hover(key),
    }
}

/// Moves the pointer onto or off the settings handle, redrawing the notch when the part under
/// it changed (the arc fills in as the button and the grip comes out, or both go back). True
/// while the pointer is on the handle: the rings see nothing then.
fn update_handle(key: isize, x: i32, y: i32) -> bool {
    if lock_state().ui.menu.is_some() {
        return false;
    }
    let now = handle_under(key, x, y);
    let changed = {
        let mut app = lock_state();
        let before = app.ui.handle.filter(|h| h.0 == key).map(|h| h.1);
        if before != now {
            app.ui.handle = now.map(|part| (key, part));
        }
        before != now
    };
    if changed {
        redraw_panel(key, &usage::snapshot());
        if now.is_some() {
            clear_hover(key);
        }
    }
    now.is_some()
}

fn on_mouse_leave(hwnd: HWND) {
    let key = hwnd_key(hwnd);
    if lock_state().ui.drag.is_none() {
        clear_pressed_part();
    }
    // The pointer left the notch: the grip goes back into the button's arc.
    let handle_was_out = {
        let mut app = lock_state();
        if app.ui.tracking == Some(key) {
            app.ui.tracking = None;
        }
        let out = app.ui.handle.is_some_and(|h| h.0 == key);
        if out {
            app.ui.handle = None;
        }
        out
    };
    if handle_was_out {
        redraw_panel(key, &usage::snapshot());
    }
    let over_send = lock_state().ui.hover.is_some_and(|(hovered, cell)| {
        hovered == key && (cell == SEND_CELL || is_claude_cell(cell))
    });
    if over_send {
        arm_hover_grace();
    } else {
        clear_hover(key);
    }
}

/// Pointer moved over the card window (only the Send card takes the pointer).
fn on_card_mouse_move(hwnd: HWND) {
    let first_popup_hover = {
        let mut app = lock_state();
        app.ui.card_hover = true;
        let popup = app.ui.card_shown.as_ref().is_some_and(|c| c.popup);
        let first = popup && !app.ui.popup_hover_sent;
        if first {
            app.ui.popup_hover_sent = true;
        }
        first
    };
    if first_popup_hover {
        send::popup_hover(true);
    }
    sync_send_hover();
    let armed = std::mem::replace(&mut lock_state().ui.card_tracking, true);
    if !armed {
        let mut request = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        if unsafe { TrackMouseEvent(&mut request) }.is_err() {
            lock_state().ui.card_tracking = false;
        }
    }
}

fn on_card_mouse_leave() {
    let release = {
        let mut app = lock_state();
        app.ui.card_tracking = false;
        app.ui.card_hover = false;
        std::mem::take(&mut app.ui.popup_hover_sent)
    };
    if release {
        send::popup_hover(false);
    }
    sync_send_hover();
    arm_hover_grace();
    sync_card();
}

/// The Claude cell's card has a button (restart and sync), so it takes the pointer like the
/// Send card does.
fn is_claude_cell(cell: usize) -> bool {
    Cell::ALL.get(cell) == Some(&Cell::Claude)
}

/// A click on the Send card or the Claude card: runs the action of the control under the
/// pointer.
fn on_card_click(x: i32, y: i32) {
    let notice = lock_state()
        .ui
        .card_shown
        .as_ref()
        .is_some_and(|shown| shown.notice);
    if notice {
        on_notice_click(x, y);
        return;
    }
    let (action, claude) = {
        let app = lock_state();
        let Some(shown) = app.ui.card_shown.as_ref() else {
            return;
        };
        let Some(dpi) = app
            .panels
            .iter()
            .find(|p| p.window.key() == shown.key)
            .map(|p| p.slot.dpi)
        else {
            return;
        };
        let action = with_text(|text| render::hit_at(&shown.panel.content, dpi, text, (x, y)))
            .flatten()
            .and_then(|hit| shown.panel.action(hit));
        (action, is_claude_cell(shown.cell))
    };
    if let Some(action) = action {
        if claude {
            claude_restart::start();
        } else {
            send::perform(action);
        }
        // The card is about to change or go; the pointer re-announces itself on the next one.
        let mut app = lock_state();
        app.ui.card_hover = false;
        app.ui.card_hit = None;
        app.ui.popup_hover_sent = false;
    }
}

/// A click on an alert card puts it away; on the update card it presses the row under it.
fn on_notice_click(x: i32, y: i32) {
    let hit = {
        let app = lock_state();
        let shown = app.ui.card_shown.as_ref();
        let dpi = shown.and_then(|s| {
            app.panels
                .iter()
                .find(|p| p.window.key() == s.key)
                .map(|p| p.slot.dpi)
        });
        shown.zip(dpi).and_then(|(shown, dpi)| {
            with_text(|text| render::hit_at(&shown.panel.content, dpi, text, (x, y))).flatten()
        })
    };
    if update::panel().is_some() {
        if let Some(hit) = hit {
            update::click(hit);
        }
    } else {
        alerts::dismiss();
    }
    {
        let mut app = lock_state();
        app.ui.card_hover = false;
        app.ui.card_hit = None;
    }
    sync_card();
}

fn on_lbutton_down(hwnd: HWND, x: i32, y: i32) {
    let key = hwnd_key(hwnd);
    // Alt-drag takes the notch from anywhere; the grip takes it without Alt.
    if alt_down() {
        begin_drag(hwnd, false);
        return;
    }
    match handle_under(key, x, y) {
        Some(Handle::Grip) => {
            set_pressed_part(key, Some(render::Press::Grip));
            begin_drag(hwnd, true);
        }
        Some(Handle::Orb) => {
            lock_state().ui.press_orb = Some(key);
            set_pressed_part(key, Some(render::Press::Orb));
        }
        None => {
            if let Some(cell) = cell_under(key, x, y) {
                lock_state().ui.press = Some((key, cell));
                set_pressed_part(key, Some(render::Press::Cell(cell)));
            }
        }
    }
}

/// Marks the part of notch `key` the button is held on (or none) and redraws the notch.
fn set_pressed_part(key: isize, part: Option<render::Press>) {
    {
        let mut app = lock_state();
        let now = part.map(|part| (key, part));
        if app.ui.pressed == now {
            return;
        }
        app.ui.pressed = now;
    }
    redraw_panel(key, &usage::snapshot());
}

/// Puts any pressed look away, redrawing the notch that wore it.
fn clear_pressed_part() {
    let key = lock_state().ui.pressed.map(|p| p.0);
    if let Some(key) = key {
        set_pressed_part(key, None);
    }
}

fn on_lbutton_up(hwnd: HWND, x: i32, y: i32) {
    let key = hwnd_key(hwnd);
    clear_pressed_part();
    let (drag, press, press_orb) = {
        let mut app = lock_state();
        (
            app.ui.drag.take(),
            app.ui.press.take(),
            app.ui.press_orb.take(),
        )
    };
    if let Some(drag) = drag {
        let _ = unsafe { ReleaseCapture() };
        finish_drag(drag);
        return;
    }
    // A press and release on the settings button opens the hub's settings, as on the Mac.
    if press_orb == Some(key) {
        if handle_under(key, x, y) == Some(Handle::Orb) && !hub::open(SETTINGS_SECTION) {
            diag::info("hub_unavailable", &[("section", SETTINGS_SECTION)]);
        }
        return;
    }
    let Some((press_key, press_cell)) = press else {
        return;
    };
    if press_key == key && cell_under(key, x, y) == Some(press_cell) {
        let section = Cell::ALL[press_cell].section();
        if !hub::open(section) {
            diag::info("hub_unavailable", &[("section", section)]);
        }
    }
}

fn on_capture_lost() {
    clear_pressed_part();
    let drag = lock_state().ui.drag.take();
    if let Some(drag) = drag {
        finish_drag(drag);
    }
}

/// Alt-drag: captures the pointer and carries the notch along its edge; moved near another
/// edge of the same monitor, it docks there (and turns its rings on their side).
fn begin_drag(hwnd: HWND, grip: bool) {
    let key = hwnd_key(hwnd);
    let mut cursor = POINT::default();
    let mut rect = RECT::default();
    if unsafe { GetCursorPos(&mut cursor) }.is_err()
        || unsafe { GetWindowRect(hwnd, &mut rect) }.is_err()
    {
        return;
    }
    let drag = {
        let app = lock_state();
        let Some(panel) = app.panels.iter().find(|p| p.window.key() == key) else {
            return;
        };
        let edge = panel.slot.edge;
        Drag {
            panel: key,
            id: panel.id.clone(),
            grab: if edge.is_vertical() {
                cursor.y - rect.top
            } else {
                cursor.x - rect.left
            },
            edge,
            folded: panel.slot.folded,
            dpi: panel.slot.dpi,
            monitor: docking_bounds(panel.bounds),
            left: rect.left,
            top: rect.top,
            moved: false,
        }
    };
    {
        let mut app = lock_state();
        app.ui.hover = None;
        app.ui.press = None;
        app.ui.press_orb = None;
        // The grip goes back into the button while the notch is carried, unless it was
        // grabbed: it stays out, pressed, until released.
        if !grip {
            app.ui.handle = None;
        }
    }
    redraw_panel(key, &usage::snapshot());
    hide_card();
    lock_state().ui.drag = Some(drag);
    let _ = unsafe { SetCapture(hwnd) };
}

fn drag_to(hwnd: HWND) {
    let mut cursor = POINT::default();
    if unsafe { GetCursorPos(&mut cursor) }.is_err() {
        return;
    }
    let (left, top, width, height, turned) = {
        let mut app = lock_state();
        let Some(drag) = app.ui.drag.as_mut() else {
            return;
        };
        let m = drag.monitor;
        let monitor = (m.left, m.top, m.right, m.bottom);
        let edge = layout::edge_for_cursor(drag.edge, monitor, (cursor.x, cursor.y), drag.dpi);
        let (width, height) = layout::panel_size(edge, drag.folded, drag.dpi);
        let (pointer, length, low, high) = if edge.is_vertical() {
            (cursor.y, height, m.top, m.bottom)
        } else {
            (cursor.x, width, m.left, m.right)
        };
        // Same edge: keep the grab point under the pointer. New edge: centre on the pointer.
        let wanted = if edge == drag.edge {
            pointer - drag.grab
        } else {
            pointer - length / 2
        };
        let along = wanted.clamp(low, (high - length).max(low));
        let (left, top) = match edge {
            Edge::Top => (along, m.top),
            Edge::Bottom => (along, (m.bottom - height).max(m.top)),
            Edge::Left => (m.left, along),
            Edge::Right => ((m.right - width).max(m.left), along),
        };
        let turned = edge != drag.edge;
        if !turned && left == drag.left && top == drag.top {
            return;
        }
        drag.edge = edge;
        drag.left = left;
        drag.top = top;
        drag.moved = true;
        (left, top, width, height, turned)
    };
    let moved = unsafe {
        SetWindowPos(
            hwnd,
            None,
            left,
            top,
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
    if let Err(error) = moved {
        diag::win32_error("SetWindowPos", &error, "drag");
        return;
    }
    if turned {
        // The rings turn with the edge: redraw at the new orientation.
        let key = hwnd_key(hwnd);
        let edge = lock_state().ui.drag.as_ref().map(|d| d.edge);
        if let (Some(edge), Some(panel)) = (
            edge,
            lock_state()
                .panels
                .iter_mut()
                .find(|p| p.window.key() == key),
        ) {
            panel.slot.edge = edge;
            panel.drawn = None;
        }
        redraw_panel(key, &usage::snapshot());
    }
}

/// Remembers the dropped edge and position for the monitor and writes them atomically.
fn finish_drag(drag: Drag) {
    if !drag.moved {
        return;
    }
    let m = drag.monitor;
    let along = layout::along_for_origin(
        drag.edge,
        (m.left, m.top, m.right, m.bottom),
        layout::panel_size(drag.edge, drag.folded, drag.dpi),
        (drag.left, drag.top),
    );
    {
        let mut app = lock_state();
        app.settings.set_position(&drag.id, along);
        app.settings.set_edge(&drag.id, drag.edge);
        if let Some(panel) = app.panels.iter_mut().find(|p| p.id == drag.id) {
            panel.slot.along = along;
            panel.slot.edge = drag.edge;
        }
    }
    persist_settings();
    // The hub's Edge control follows the move at once instead of at the next two-second poll.
    bridge::wake();
}

/// Right-click: a single "Quit" item, drawn as a non-activating card so focus never moves.
fn open_menu(hwnd: HWND) {
    let key = hwnd_key(hwnd);
    let (dpi, monitor, edge) = {
        let app = lock_state();
        let Some(panel) = app.panels.iter().find(|p| p.window.key() == key) else {
            return;
        };
        (
            panel.slot.dpi,
            docking_bounds(panel.bounds),
            panel.slot.edge,
        )
    };
    let mut cursor = POINT::default();
    let mut rect = RECT::default();
    if unsafe { GetCursorPos(&mut cursor) }.is_err()
        || unsafe { GetWindowRect(hwnd, &mut rect) }.is_err()
    {
        return;
    }
    let (menu_width, menu_height) = render::menu_size(dpi);
    let (x, y) = layout::card_origin(
        edge,
        (rect.left, rect.top, rect.right, rect.bottom),
        if edge.is_vertical() {
            cursor.y
        } else {
            cursor.x
        },
        (menu_width, menu_height),
        (layout::CARD_GAP * layout::scale(dpi)).round() as i32,
        (monitor.left, monitor.top, monitor.right, monitor.bottom),
    );
    let Some(card_key) = ensure_card() else {
        return;
    };
    let Some(canvas) = with_text(|text| render::render_menu(dpi, text)) else {
        return;
    };
    {
        let mut app = lock_state();
        app.ui.hover = None;
        app.ui.card_shown = None;
        app.ui.menu = Some((
            key,
            RECT {
                left: x,
                top: y,
                right: x + menu_width,
                bottom: y + menu_height,
            },
        ));
    }
    let card = hwnd_from_key(card_key);
    if let Err(error) = present(card, &canvas, Some((x, y))) {
        diag::win32_error("UpdateLayeredWindow", &error, "menu");
        lock_state().ui.menu = None;
        return;
    }
    let _ = unsafe { set_panel_hidden(card, false) };
    if let Some(controller) = controller_hwnd() {
        let _ = unsafe { SetTimer(Some(controller), MENU_TIMER_ID, MENU_POLL_MS, None) };
    }
}

fn close_menu() {
    {
        let mut app = lock_state();
        app.ui.menu = None;
        app.ui.menu_hot = false;
    }
    stop_menu_timer();
    hide_card();
    sync_card(); // a sharing popup that waited behind the menu comes back
}

/// Dismisses the menu when a mouse button goes down anywhere outside it.
fn menu_tick() {
    let menu = lock_state().ui.menu;
    let Some((_, rect)) = menu else {
        stop_menu_timer();
        return;
    };
    let mut cursor = POINT::default();
    if unsafe { GetCursorPos(&mut cursor) }.is_err() {
        return;
    }
    let pressed = [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON]
        .iter()
        .any(|button| unsafe { GetAsyncKeyState(button.0 as i32) } < 0);
    let inside = cursor.x >= rect.left
        && cursor.x < rect.right
        && cursor.y >= rect.top
        && cursor.y < rect.bottom;
    if pressed && !inside {
        close_menu();
    }
}

// ---------------------------------------------------------------- nearby sharing

/// `send` reported news: redraw the Send ring on every visible notch and the card.
fn on_send_changed() {
    let usage = usage::snapshot();
    let visible: Vec<isize> = lock_state()
        .panels
        .iter()
        .filter(|p| !p.visibility.applied())
        .map(|p| p.window.key())
        .collect();
    for key in visible {
        redraw_panel(key, &usage);
    }
    sync_card();
}

/// `WM_DROPFILES` on a notch: files dropped on the Send cell are sent; anywhere else they are
/// ignored. The drop is always released.
fn on_drop(hwnd: HWND, hdrop: WPARAM) {
    let dropped = send::take_drop(hdrop.0 as isize);
    if cell_under(hwnd_key(hwnd), dropped.x, dropped.y) == Some(SEND_CELL) {
        send::drop_files(dropped.paths);
    }
}

// ---------------------------------------------------------------- window procedures

extern "system" fn controller_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match message {
            WM_TIMER if wparam.0 == TIMER_ID => {
                on_timer(hwnd);
                return LRESULT(0);
            }
            WM_TIMER if wparam.0 == MENU_TIMER_ID => {
                menu_tick();
                return LRESULT(0);
            }
            WM_TIMER if wparam.0 == NOTICE_TIMER_ID => {
                sync_card();
                return LRESULT(0);
            }
            WM_TIMER if wparam.0 == ANIM_TIMER_ID => {
                animate_card();
                return LRESULT(0);
            }
            update::MSG_UPDATE => {
                // The red dot follows whether a newer release is known.
                let badges_changed = refresh_badges();
                sync_card();
                if badges_changed {
                    let interval = refresh_panels(None);
                    arm_timer(hwnd, interval);
                }
                return LRESULT(0);
            }
            WM_TIMER if wparam.0 == FOLD_TIMER_ID => {
                fold_tick();
                return LRESULT(0);
            }
            WM_TIMER if wparam.0 == HOVER_TIMER_ID => {
                hover_grace_tick();
                return LRESULT(0);
            }
            WM_SEND => {
                on_send_changed();
                return LRESULT(0);
            }
            WM_HOTKEY if wparam.0 == send::HOTKEY_ID as usize => {
                send::paste_clipboard();
                return LRESULT(0);
            }
            WM_DISPLAYCHANGE | WM_DPICHANGED => {
                on_display_change();
                return LRESULT(0);
            }
            WM_SETTINGCHANGE if wparam.0 == SPI_SETWORKAREA.0 as usize => {
                redock_panels();
                return LRESULT(0);
            }
            bridge::MSG_PLACEMENT_CHANGED => {
                // Accent or limits may have changed with nothing else: redraw every bitmap.
                for panel in lock_state().panels.iter_mut() {
                    panel.drawn = None;
                }
                on_display_change();
                refold_after_settings();
                return LRESULT(0);
            }
            usage::MSG_USAGE_UPDATED => {
                let prefs = alerts::Prefs::from_settings(&lock_state().settings);
                let raised = alerts::observe(&usage::snapshot(), usage::now_secs(), prefs);
                let interval = refresh_panels(None);
                arm_timer(hwnd, interval);
                if raised {
                    sync_card();
                }
                return LRESULT(0);
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                return LRESULT(0);
            }
            WM_ENDSESSION if wparam.0 != 0 => {
                let _ = DestroyWindow(hwnd);
                return LRESULT(0);
            }
            WM_DESTROY => {
                let _ = KillTimer(Some(hwnd), TIMER_ID);
                let _ = KillTimer(Some(hwnd), MENU_TIMER_ID);
                let _ = KillTimer(Some(hwnd), HOVER_TIMER_ID);
                let _ = KillTimer(Some(hwnd), FOLD_TIMER_ID);
                let _ = KillTimer(Some(hwnd), NOTICE_TIMER_ID);
                let _ = KillTimer(Some(hwnd), ANIM_TIMER_ID);
                teardown_panels();
                PostQuitMessage(0);
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

extern "system" fn panel_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match message {
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
            WM_NCHITTEST => return LRESULT(HTCLIENT as isize),
            WM_ERASEBKGND => return LRESULT(1),
            WM_PAINT => {
                // Contents come from UpdateLayeredWindow; just validate.
                let _ = ValidateRect(Some(hwnd), None);
                return LRESULT(0);
            }
            WM_MOUSEMOVE => {
                let (x, y) = lparam_point(lparam);
                on_mouse_move(hwnd, x, y);
                return LRESULT(0);
            }
            MSG_MOUSELEAVE => {
                on_mouse_leave(hwnd);
                return LRESULT(0);
            }
            WM_LBUTTONDOWN => {
                let (x, y) = lparam_point(lparam);
                on_lbutton_down(hwnd, x, y);
                return LRESULT(0);
            }
            WM_LBUTTONUP => {
                let (x, y) = lparam_point(lparam);
                on_lbutton_up(hwnd, x, y);
                return LRESULT(0);
            }
            WM_CAPTURECHANGED => {
                on_capture_lost();
                return LRESULT(0);
            }
            WM_SETCURSOR if (lparam.0 & 0xFFFF) as u32 == HTCLIENT => {
                // A pointing hand over the settings button, the four-way arrow over the grip.
                let part = lock_state()
                    .ui
                    .handle
                    .filter(|h| h.0 == hwnd_key(hwnd))
                    .map(|h| h.1);
                let shape = match part {
                    Some(Handle::Orb) => Some(IDC_HAND),
                    Some(Handle::Grip) => Some(IDC_SIZEALL),
                    None => None,
                };
                if let Some(shape) = shape {
                    if let Ok(cursor) = LoadCursorW(None, shape) {
                        SetCursor(Some(cursor));
                    }
                    return LRESULT(1);
                }
            }
            WM_RBUTTONUP => {
                if !alt_down() {
                    open_menu(hwnd);
                }
                return LRESULT(0);
            }
            WM_DPICHANGED => {
                on_display_change();
                return LRESULT(0);
            }
            WM_DROPFILES => {
                on_drop(hwnd, wparam);
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

extern "system" fn card_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match message {
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
            WM_NCHITTEST => {
                // Hover cards let the pointer through; the Quit menu and the Send card (its
                // rows are buttons) take it.
                let takes_pointer = {
                    let app = lock_state();
                    app.ui.menu.is_some()
                        || app.ui.card_shown.as_ref().is_some_and(|c| {
                            c.cell == SEND_CELL || c.notice || is_claude_cell(c.cell)
                        })
                };
                return if takes_pointer {
                    LRESULT(HTCLIENT as isize)
                } else {
                    LRESULT(HTTRANSPARENT as isize)
                };
            }
            WM_ERASEBKGND => return LRESULT(1),
            WM_PAINT => {
                let _ = ValidateRect(Some(hwnd), None);
                return LRESULT(0);
            }
            WM_MOUSEMOVE => {
                if lock_state().ui.menu.is_none() {
                    on_card_mouse_move(hwnd);
                    let (x, y) = lparam_point(lparam);
                    set_card_hit(card_hit_at(x, y));
                } else {
                    on_menu_mouse_move(hwnd);
                }
                return LRESULT(0);
            }
            WM_SETCURSOR if (lparam.0 & 0xFFFF) as u32 == HTCLIENT => {
                // A pointing hand over a control that acts, the arrow elsewhere.
                let mut cursor = POINT::default();
                let over = lock_state().ui.menu.is_none()
                    && GetCursorPos(&mut cursor).is_ok()
                    && ScreenToClient(hwnd, &mut cursor).as_bool()
                    && card_hit_at(cursor.x, cursor.y).is_some();
                let shape = if over { IDC_HAND } else { IDC_ARROW };
                if let Ok(handle) = LoadCursorW(None, shape) {
                    SetCursor(Some(handle));
                }
                return LRESULT(1);
            }
            MSG_MOUSELEAVE => {
                if lock_state().ui.menu.is_some() {
                    // The Quit menu: the plate goes out; the card's hover bookkeeping is not
                    // involved.
                    lock_state().ui.card_tracking = false;
                    set_card_pressed(false);
                    set_menu_hot(false);
                    return LRESULT(0);
                }
                set_card_pressed(false);
                set_card_hit(None);
                on_card_mouse_leave();
                return LRESULT(0);
            }
            WM_LBUTTONDOWN => {
                set_card_pressed(true);
                return LRESULT(0);
            }
            WM_MOUSEWHEEL => {
                // The wheel scrolls the Send-to card's device list, one device a notch.
                let notches = i32::from((wparam.0 >> 16) as u16 as i16) / 120;
                if notches != 0 && send::scroll_choose(-notches) {
                    sync_card();
                }
                return LRESULT(0);
            }
            WM_LBUTTONUP => {
                set_card_pressed(false);
                let menu_open = lock_state().ui.menu.is_some();
                if menu_open {
                    close_menu();
                    if let Some(controller) = controller_hwnd() {
                        let _ = PostMessageW(Some(controller), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                } else {
                    let (x, y) = lparam_point(lparam);
                    on_card_click(x, y);
                }
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::{HIDDEN_INTERVAL_MS, VISIBLE_INTERVAL_MS};
    use crate::visibility::{is_borderless_style, is_fullscreen_geometry};

    fn square() -> RECT {
        RECT {
            left: 0,
            top: 0,
            right: 100,
            bottom: 100,
        }
    }

    #[test]
    fn exact_monitor_bounds_are_fullscreen() {
        assert!(is_fullscreen_geometry(square(), square()));
    }
    #[test]
    fn inset_window_is_not_fullscreen() {
        let inset = RECT {
            left: 1,
            ..square()
        };
        assert!(!is_fullscreen_geometry(inset, square()));
    }
    #[test]
    fn captioned_window_is_not_borderless() {
        assert!(!is_borderless_style(WS_CAPTION.0));
    }
    #[test]
    fn production_sampling_cadence_matches_helper() {
        assert_eq!(interval_ms(2, 1, 1), HIDDEN_INTERVAL_MS);
        assert_eq!(interval_ms(2, 1, 0), VISIBLE_INTERVAL_MS);
    }
}

#[cfg(test)]
mod visibility_cases;
