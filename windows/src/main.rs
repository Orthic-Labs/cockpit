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

mod autostart;
mod canvas;
mod card;
mod diag;
mod http;
mod hub;
mod json;
mod keys;
mod layout;
mod lifecycle;
mod raii;
mod render;
mod runtime;
mod send;
mod sensors;
mod settings;
mod shot;
mod surface;
mod usage;
mod viewshots;
mod visibility;

use layout::{Cell, CellView, SEND_CELL};
use lifecycle::{Bounds, HIDDEN_INTERVAL_MS, MonitorSpec, ReconcileGate};
use raii::{ClassGuard, OwnedWindow, TimerGuard, hwnd_from_key, hwnd_key};
use runtime::{
    InstanceError, InstanceLock, Placed, Placement, Retry, RetryReport, Slot, desired_hidden,
    interval_ms, notch_bounds, plan_placements,
};
use sensors::{Machine, Sampler};
use settings::PillSettings;
use std::cell::RefCell;
use std::mem::size_of;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use surface::{TextPainter, present};
use usage::Usage;
use visibility::{Occupancy, classify_window, is_shell_class_name, is_tool_window_ex_style};
use windows::Win32::Foundation::{
    ERROR_SUCCESS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SetLastError,
    WPARAM,
};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MONITORINFOEXW, MonitorFromRect, ValidateRect,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, VK_LBUTTON, VK_MBUTTON, VK_MENU, VK_RBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, Error, PCWSTR, s, w};

const PANEL_CLASS: PCWSTR = w!("PulseM1Notch");
const CONTROLLER_CLASS: PCWSTR = w!("PulseM1Controller");
const CARD_CLASS: PCWSTR = w!("PulseM1Card");
const TIMER_ID: usize = 7;
const MENU_TIMER_ID: usize = 8;
const MENU_POLL_MS: u32 = 80;
/// Grace period for the pointer to cross the gap from the Send cell to its card.
const HOVER_TIMER_ID: usize = 9;
const HOVER_GRACE_MS: u32 = 220;
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
    drawn: Option<(Vec<CellView>, u32)>,
    window: OwnedWindow,
}

struct Drag {
    panel: isize,
    id: String,
    start_cursor_x: i32,
    start_left: i32,
    left: i32,
    top: i32,
    width: i32,
    monitor_left: i32,
    monitor_right: i32,
    moved: bool,
}

struct Interaction {
    /// Panel and cell currently under the pointer.
    hover: Option<(isize, usize)>,
    /// Panel with an armed WM_MOUSELEAVE request.
    tracking: Option<isize>,
    /// Panel and cell where a plain left press started (click on release in the same cell).
    press: Option<(isize, usize)>,
    drag: Option<Drag>,
    /// Quit menu: owning panel and its screen rectangle.
    menu: Option<(isize, RECT)>,
    /// What the card window shows.
    card_shown: Option<Shown>,
    /// Pointer is over the card window (only the Send card takes the pointer).
    card_hover: bool,
    /// Leave notification armed on the card window.
    card_tracking: bool,
    /// `send::popup_hover(true)` was sent for the popup under the pointer.
    popup_hover_sent: bool,
}

/// The card on screen: owning panel, cell, content and whether it is a sharing popup.
struct Shown {
    key: isize,
    cell: usize,
    panel: send::Panel,
    popup: bool,
}

impl Interaction {
    const fn new() -> Self {
        Self {
            hover: None,
            tracking: None,
            press: None,
            drag: None,
            menu: None,
            card_shown: None,
            card_hover: false,
            card_tracking: false,
            popup_hover_sent: false,
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
    // Declared first, so it is released last (after windows, timer, classes and the final
    // settings write). Held for the lifetime of the process.
    let _instance = match InstanceLock::acquire() {
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
    send::stop();
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
        &wide("Pulse Notch Controller"),
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
    // timer so the hook thread stops first; `shots` is declared before `_keys` so the hook
    // stops posting before the screenshot thread exits.
    let (mac_shortcuts, screenshot_shortcuts) = {
        let app = lock_state();
        (
            app.settings.mac_shortcuts,
            app.settings.screenshot_shortcuts,
        )
    };
    shot::set_save_to_desktop(lock_state().settings.screenshot_to_desktop);
    let shots = if screenshot_shortcuts {
        shot::start()
    } else {
        None
    };
    let _keys = keys::start(mac_shortcuts, shots.is_some());

    usage::start(controller.key());
    let nearby = lock_state().settings.nearby_enabled;
    send::set_enabled(nearby);
    send::start(controller.key(), WM_SEND);
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
    let settings = lock_state().settings.clone();
    found
        .iter()
        .filter_map(|spec| {
            settings.monitor(&spec.id).enabled.then(|| Placed {
                spec: spec.clone(),
                slot: Slot {
                    along: settings.position(&spec.id),
                    dpi: monitor_dpi(spec.bounds),
                },
            })
        })
        .collect()
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
                    let target = notch_bounds(placed.spec.bounds, placed.slot);
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
    let target = notch_bounds(spec.bounds, placed.slot);
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
fn refresh_panels(new_machine: Option<Machine>) -> u32 {
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

/// Draws the panel's bitmap when the cells it shows (or its DPI) changed.
fn redraw_panel(key: isize, usage: &[Usage; 2]) {
    let ring = send::ring();
    let (views, dpi) = {
        let app = lock_state();
        let Some(panel) = app.panels.iter().find(|p| p.window.key() == key) else {
            return;
        };
        let views = layout::views(app.machine.as_ref(), usage, &ring);
        let dpi = panel.slot.dpi;
        if panel
            .drawn
            .as_ref()
            .is_some_and(|(shown, shown_dpi)| *shown == views && *shown_dpi == dpi)
        {
            return;
        }
        (views, dpi)
    };
    let Some(canvas) = with_text(|text| render::render_panel(&views, dpi, text)) else {
        return;
    };
    match present(hwnd_from_key(key), &canvas, None) {
        Ok(()) => {
            if let Some(panel) = lock_state()
                .panels
                .iter_mut()
                .find(|p| p.window.key() == key)
            {
                panel.drawn = Some((views, dpi));
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

fn panel_dpi(key: isize) -> Option<u32> {
    lock_state()
        .panels
        .iter()
        .find(|p| p.window.key() == key)
        .map(|p| p.slot.dpi)
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
        let release = std::mem::take(&mut app.ui.popup_hover_sent);
        (app.card.as_ref().map(OwnedWindow::key), release)
    };
    if let Some(key) = key {
        let _ = unsafe { set_panel_hidden(hwnd_from_key(key), true) };
    }
    if release {
        send::popup_hover(false);
    }
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

fn clamp_x(x: i32, width: i32, monitor: Bounds) -> i32 {
    x.clamp(monitor.left, (monitor.right - width).max(monitor.left))
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
    let (target, current_empty) = {
        let app = lock_state();
        if app.shutting_down || app.ui.menu.is_some() || app.ui.drag.is_some() {
            return;
        }
        // The pointer on the card itself keeps it; a popup needs no hover at all.
        let on_card = if app.ui.card_hover {
            app.ui.card_shown.as_ref().map(|s| (s.key, s.cell))
        } else {
            None
        };
        let popup_target = if popup.is_some() {
            app.panels
                .iter()
                .find(|p| !p.visibility.applied())
                .map(|p| (p.window.key(), SEND_CELL))
        } else {
            None
        };
        (
            app.ui.hover.or(on_card).or(popup_target),
            app.ui.card_shown.is_none(),
        )
    };
    let Some((key, cell)) = target else {
        if !current_empty {
            hide_card();
        }
        return;
    };
    let (panel, is_popup) = {
        let app = lock_state();
        card::panel_for(Cell::ALL[cell], app.machine.as_ref(), &usage, now, popup)
    };
    let unchanged = lock_state().ui.card_shown.as_ref().is_some_and(|shown| {
        shown.key == key && shown.cell == cell && shown.popup == is_popup && shown.panel == panel
    });
    if !unchanged {
        show_card(key, cell, panel, is_popup);
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

/// Shows (or updates) `panel` as the card of `cell` of the notch `key`, below the notch.
fn show_card(key: isize, cell: usize, panel: send::Panel, popup: bool) {
    let (dpi, monitor) = {
        let app = lock_state();
        if app.shutting_down {
            return;
        }
        let Some(notch) = app.panels.iter().find(|p| p.window.key() == key) else {
            return;
        };
        (notch.slot.dpi, notch.bounds)
    };
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd_from_key(key), &mut rect) }.is_err() {
        return;
    }
    let s = layout::scale(dpi);
    let (card_width, _) = render::card_size(&panel.content, dpi);
    let centre = rect.left + (layout::cell_left(cell, dpi) + layout::RING * s / 2.0) as i32;
    let x = clamp_x(centre - card_width / 2, card_width, monitor);
    let y = rect.bottom + (layout::CARD_GAP * s).round() as i32;
    let Some(card_key) = ensure_card() else {
        return;
    };
    let clickable: Vec<bool> = panel.actions.iter().map(Option::is_some).collect();
    let Some(canvas) = with_text(|text| render::render_card(&panel.content, &clickable, dpi, text))
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
    lock_state().ui.card_shown = Some(Shown {
        key,
        cell,
        panel,
        popup,
    });
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
    let Some(dpi) = panel_dpi(key) else {
        return;
    };
    match layout::cell_at(x, y, dpi) {
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

fn on_mouse_leave(hwnd: HWND) {
    let key = hwnd_key(hwnd);
    {
        let mut app = lock_state();
        if app.ui.tracking == Some(key) {
            app.ui.tracking = None;
        }
    }
    let over_send = lock_state().ui.hover == Some((key, SEND_CELL));
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

/// A click on the Send card: runs the action of the row under the pointer.
fn on_card_click(y: i32) {
    let action = {
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
        render::row_at(&shown.panel.content, dpi, y)
            .and_then(|row| shown.panel.actions.get(row).cloned().flatten())
    };
    if let Some(action) = action {
        send::perform(action);
        // The card is about to change or go; the pointer re-announces itself on the next one.
        let mut app = lock_state();
        app.ui.card_hover = false;
        app.ui.popup_hover_sent = false;
    }
}

fn on_lbutton_down(hwnd: HWND, x: i32, y: i32) {
    let key = hwnd_key(hwnd);
    if alt_down() {
        begin_drag(hwnd);
        return;
    }
    let Some(dpi) = panel_dpi(key) else {
        return;
    };
    if let Some(cell) = layout::cell_at(x, y, dpi) {
        lock_state().ui.press = Some((key, cell));
    }
}

fn on_lbutton_up(hwnd: HWND, x: i32, y: i32) {
    let key = hwnd_key(hwnd);
    let (drag, press) = {
        let mut app = lock_state();
        (app.ui.drag.take(), app.ui.press.take())
    };
    if let Some(drag) = drag {
        let _ = unsafe { ReleaseCapture() };
        finish_drag(drag);
        return;
    }
    let Some((press_key, press_cell)) = press else {
        return;
    };
    let Some(dpi) = panel_dpi(key) else {
        return;
    };
    if press_key == key && layout::cell_at(x, y, dpi) == Some(press_cell) {
        let section = Cell::ALL[press_cell].section();
        if !hub::open(section) {
            diag::info("hub_unavailable", &[("section", section)]);
        }
    }
}

fn on_capture_lost() {
    let drag = lock_state().ui.drag.take();
    if let Some(drag) = drag {
        finish_drag(drag);
    }
}

/// Alt-drag along the top edge: captures the pointer and moves the notch horizontally,
/// clamped to its own monitor.
fn begin_drag(hwnd: HWND) {
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
        Drag {
            panel: key,
            id: panel.id.clone(),
            start_cursor_x: cursor.x,
            start_left: rect.left,
            left: rect.left,
            top: rect.top,
            width: rect.right - rect.left,
            monitor_left: panel.bounds.left,
            monitor_right: panel.bounds.right,
            moved: false,
        }
    };
    {
        let mut app = lock_state();
        app.ui.hover = None;
        app.ui.press = None;
    }
    hide_card();
    lock_state().ui.drag = Some(drag);
    let _ = unsafe { SetCapture(hwnd) };
}

fn drag_to(hwnd: HWND) {
    let mut cursor = POINT::default();
    if unsafe { GetCursorPos(&mut cursor) }.is_err() {
        return;
    }
    let target = {
        let mut app = lock_state();
        let Some(drag) = app.ui.drag.as_mut() else {
            return;
        };
        let wanted = drag.start_left + (cursor.x - drag.start_cursor_x);
        let max_left = (drag.monitor_right - drag.width).max(drag.monitor_left);
        let left = wanted.clamp(drag.monitor_left, max_left);
        if left == drag.left {
            return;
        }
        drag.left = left;
        drag.moved = true;
        (left, drag.top)
    };
    let moved = unsafe {
        SetWindowPos(
            hwnd,
            None,
            target.0,
            target.1,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
    if let Err(error) = moved {
        diag::win32_error("SetWindowPos", &error, "drag");
    }
}

/// Remembers the dropped position for the monitor and writes it atomically.
fn finish_drag(drag: Drag) {
    if !drag.moved {
        return;
    }
    let along = layout::along_for_left(
        drag.monitor_left,
        drag.monitor_right - drag.monitor_left,
        drag.width,
        drag.left,
    );
    {
        let mut app = lock_state();
        app.settings.set_position(&drag.id, along);
        if let Some(panel) = app.panels.iter_mut().find(|p| p.id == drag.id) {
            panel.slot.along = along;
        }
    }
    persist_settings();
}

/// Right-click: a single "Quit" item, drawn as a non-activating card so focus never moves.
fn open_menu(hwnd: HWND) {
    let key = hwnd_key(hwnd);
    let (dpi, monitor) = {
        let app = lock_state();
        let Some(panel) = app.panels.iter().find(|p| p.window.key() == key) else {
            return;
        };
        (panel.slot.dpi, panel.bounds)
    };
    let mut cursor = POINT::default();
    let mut rect = RECT::default();
    if unsafe { GetCursorPos(&mut cursor) }.is_err()
        || unsafe { GetWindowRect(hwnd, &mut rect) }.is_err()
    {
        return;
    }
    let (menu_width, menu_height) = render::menu_size(dpi);
    let x = clamp_x(cursor.x - menu_width / 2, menu_width, monitor);
    let y = rect.bottom + (layout::CARD_GAP * layout::scale(dpi)).round() as i32;
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
    lock_state().ui.menu = None;
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
    let Some(dpi) = panel_dpi(hwnd_key(hwnd)) else {
        return;
    };
    if layout::cell_at(dropped.x, dropped.y, dpi) == Some(SEND_CELL) {
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
            usage::MSG_USAGE_UPDATED => {
                let interval = refresh_panels(None);
                arm_timer(hwnd, interval);
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
                        || app
                            .ui
                            .card_shown
                            .as_ref()
                            .is_some_and(|c| c.cell == SEND_CELL)
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
                }
                return LRESULT(0);
            }
            MSG_MOUSELEAVE => {
                on_card_mouse_leave();
                return LRESULT(0);
            }
            WM_LBUTTONUP => {
                let menu_open = lock_state().ui.menu.is_some();
                if menu_open {
                    close_menu();
                    if let Some(controller) = controller_hwnd() {
                        let _ = PostMessageW(Some(controller), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                } else {
                    let (_, y) = lparam_point(lparam);
                    on_card_click(y);
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
