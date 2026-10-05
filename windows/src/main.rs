#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Native Windows pill (M0 prototype).
//!
//! Ownership: a hidden controller window owns the single sampling timer and receives
//! WM_DISPLAYCHANGE; per-monitor panels are RAII `OwnedWindow`s stored in `STATE`.
//! Locking rule: `STATE`/`SAMPLER` guards are only held for plain data access. Any call that
//! can re-enter a wndproc (SetWindowPos, ShowWindow, InvalidateRect, DestroyWindow,
//! CreateWindowExW, EnumWindows, EnumDisplayMonitors) runs with no guard held, and panels
//! are removed from `STATE` before they are dropped (destroyed).

mod diag;
mod lifecycle;
mod raii;
mod runtime;
mod settings;
mod visibility;

use diag::{FailureLatch, Transition};
use lifecycle::{Bounds, HIDDEN_INTERVAL_MS, MonitorSpec, ReconcileGate, cpu_fraction};
use raii::{
    ClassGuard, GdiObject, OwnedWindow, PaintScope, SelectScope, TimerGuard, hwnd_from_key,
    hwnd_key,
};
use runtime::{
    InstanceError, InstanceLock, Placed, Placement, Retry, RetryReport, anchor_bounds,
    desired_hidden, interval_ms, plan_placements,
};
use settings::{Anchor, PillSettings};
use std::mem::size_of;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use visibility::{Occupancy, classify_window, is_shell_class_name, is_tool_window_ex_style};
use windows::Win32::Foundation::{
    COLORREF, ERROR_SUCCESS, FILETIME, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, RECT,
    SetLastError, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    Arc, CreatePen, CreateSolidBrush, Ellipse, EnumDisplayMonitors, FillRect, GetMonitorInfoW,
    HBRUSH, HDC, HMONITOR, InvalidateRect, MONITORINFO, MONITORINFOEXW, PS_SOLID, SetBkMode,
    SetTextColor, TRANSPARENT, TextOutW,
};
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, Error, PCWSTR, s, w};

const PANEL_CLASS: PCWSTR = w!("CockpitM0NativePill");
const CONTROLLER_CLASS: PCWSTR = w!("CockpitM0Controller");
const TIMER_ID: usize = 7;
static COORDINATES_COMPARABLE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq)]
struct Reading {
    cpu: Option<f32>,
    memory: Option<f32>,
    disk: Option<f32>,
}

struct Panel {
    id: String,
    bounds: Bounds, // monitor bounds this panel was placed for (updated only after a move succeeds)
    anchor: Anchor, // anchor this panel was placed with (updated only after a move succeeds)
    // Last successfully applied hidden state. Created hidden; a failed show/hide leaves it
    // unchanged so the next refresh retries while the panel keeps owning its HWND.
    visibility: Retry<bool>,
    window: OwnedWindow,
}

struct AppState {
    panels: Vec<Panel>,
    reading: Option<Reading>,
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
            reading: None,
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

/// Sole sampling owner: only `sample_once`, called only from the controller's WM_TIMER
/// path (plus the first sample at startup), touches this. Held across plain syscalls only.
struct Sampler {
    previous_times: Option<(u64, u64, u64)>,
    cpu: FailureLatch,
    memory: FailureLatch,
    disk: FailureLatch,
}

static SAMPLER: Mutex<Sampler> = Mutex::new(Sampler {
    previous_times: None,
    cpu: FailureLatch::new(),
    memory: FailureLatch::new(),
    disk: FailureLatch::new(),
});

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
                &[("mutex", "Local\\Cockpit.Pill.v1.<user-sid>")],
            );
            return Ok(());
        }
        Err(InstanceError::Failed(error)) => {
            diag::win32_error("CreateMutexW", &error, "single_instance");
            return Err(error);
        }
    };
    load_settings();
    let result = run_pill();
    // run_pill has returned: timer killed, panels destroyed, classes unregistered.
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
        diag::info("settings_persist_skipped", &[("reason", "unusable_or_unavailable")]);
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

    // Drop order is reverse of declaration: timer, controller window (tears down panels),
    // panel class, controller class.
    let _controller_class =
        ClassGuard::register(CONTROLLER_CLASS, Some(controller_proc), instance)?;
    let _panel_class = ClassGuard::register(PANEL_CLASS, Some(panel_proc), instance)?;
    let controller = OwnedWindow::create(
        WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
        CONTROLLER_CLASS,
        &wide("Cockpit M0 Controller"),
        WS_POPUP, // never shown; still a top-level window, so it receives WM_DISPLAYCHANGE
        (0, 0, 0, 0),
        instance,
    )?;
    let _timer = TimerGuard::new(controller.hwnd(), TIMER_ID);
    if !arm_timer(controller.hwnd(), HIDDEN_INTERVAL_MS) {
        return Err(Error::from_win32());
    }

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

fn sample_once() -> Reading {
    let mut sampler = SAMPLER.lock().unwrap_or_else(PoisonError::into_inner);
    let sampler = &mut *sampler;
    let cpu = match read_cpu_times() {
        Ok(current) => {
            note(&mut sampler.cpu, "GetSystemTimes", false, "cpu");
            let value = cpu_fraction(sampler.previous_times, current);
            sampler.previous_times = Some(current);
            value
        }
        Err(error) => {
            if sampler.cpu.observe(true) == Transition::Failed {
                diag::win32_error("GetSystemTimes", &error, "cpu");
            }
            sampler.previous_times = None;
            None
        }
    };
    let memory = settle(
        &mut sampler.memory,
        "GlobalMemoryStatusEx",
        "memory",
        read_memory(),
    );
    let disk = settle(
        &mut sampler.disk,
        "GetDiskFreeSpaceExW",
        "disk",
        read_disk(),
    );
    Reading {
        cpu: cpu.map(|v| v.clamp(0.0, 1.0)),
        memory,
        disk,
    }
}

fn note(latch: &mut FailureLatch, op: &str, failed: bool, ctx: &str) {
    if latch.observe(failed) == Transition::Recovered {
        diag::info("sampler_recovered", &[("op", op), ("ctx", ctx)]);
    }
}

fn settle(
    latch: &mut FailureLatch,
    op: &str,
    ctx: &str,
    result: Result<Option<f32>, Error>,
) -> Option<f32> {
    match result {
        Ok(value) => {
            note(latch, op, false, ctx);
            value
        }
        Err(error) => {
            if latch.observe(true) == Transition::Failed {
                diag::win32_error(op, &error, ctx);
            }
            None
        }
    }
}

fn read_cpu_times() -> Result<(u64, u64, u64), Error> {
    let mut idle = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }?;
    Ok((filetime(idle), filetime(kernel), filetime(user)))
}

fn read_memory() -> Result<Option<f32>, Error> {
    let mut memory = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe { GlobalMemoryStatusEx(&mut memory) }?;
    Ok(Some(
        (1.0 - (memory.ullAvailPhys as f32 / memory.ullTotalPhys.max(1) as f32)).clamp(0.0, 1.0),
    ))
}

fn read_disk() -> Result<Option<f32>, Error> {
    let mut free = 0u64;
    let mut total = 0u64;
    unsafe { GetDiskFreeSpaceExW(PCWSTR::null(), Some(&mut free), Some(&mut total), None) }?;
    if total == 0 {
        return Ok(None); // unknown, shown as "--"
    }
    Ok(Some((1.0 - free as f32 / total as f32).clamp(0.0, 1.0)))
}

fn filetime(value: FILETIME) -> u64 {
    ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64
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
/// resolution or arrangement change). Re-entrant requests coalesce through `ReconcileGate`.
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

/// Enabled monitors with their stored anchors (defaults for monitors not in settings).
fn desired_placements(found: &[MonitorSpec]) -> Vec<Placed> {
    let settings = lock_state().settings.clone();
    found
        .iter()
        .filter_map(|spec| {
            let setting = settings.monitor(&spec.id);
            setting.enabled.then(|| Placed {
                spec: spec.clone(),
                anchor: setting.anchor,
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
            anchor: p.anchor,
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
                    let target = anchor_bounds(placed.spec.bounds, placed.anchor);
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
                                p.anchor = placed.anchor;
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
    let target = anchor_bounds(spec.bounds, placed.anchor);
    let window = OwnedWindow::create(
        WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
        PANEL_CLASS,
        &wide(&format!("Cockpit M0 {}", spec.id)),
        WS_POPUP,
        (target.left, target.top, target.width(), target.height()),
        instance,
    )?;
    // On error `window` drops and destroys the half-built panel. Created hidden: the first
    // refresh decides whether to show it, so it never flashes over a fullscreen app.
    unsafe { SetLayeredWindowAttributes(window.hwnd(), COLORREF(0), 238, LWA_ALPHA) }?;
    Ok(Panel {
        id: spec.id.clone(),
        bounds: spec.bounds,
        anchor: placed.anchor,
        visibility: Retry::new(true),
        window,
    })
}

fn teardown_panels() {
    let panels = {
        let mut app = lock_state();
        app.shutting_down = true;
        std::mem::take(&mut app.panels)
    };
    drop(panels); // destroys every panel window with no lock held
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

/// Re-evaluate occupancy for every panel, show/hide on transitions, invalidate when the
/// reading or visibility changed. Returns the cadence the timer should use.
fn refresh_panels(new_reading: Option<Reading>) -> u32 {
    let (targets, own, reading_changed, settings_visible, cadence) = {
        let mut app = lock_state();
        let changed = match new_reading {
            Some(reading) => {
                let changed = app.reading != Some(reading);
                app.reading = Some(reading);
                changed
            }
            None => false,
        };
        let targets: Vec<(isize, Bounds, Retry<bool>)> = app
            .panels
            .iter()
            .map(|p| (p.window.key(), p.bounds, p.visibility))
            .collect();
        let own: Vec<isize> = targets.iter().map(|t| t.0).collect();
        (
            targets,
            own,
            changed,
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
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
        } else if reading_changed && !visibility.applied() {
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
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
            app.panels
                .iter()
                .filter(|p| p.visibility.applied())
                .count(),
        )
    };
    interval_ms(cadence, total, hidden_count)
}

/// Show (topmost, no activation) or hide a panel through SetWindowPos so failures are
/// observable (ShowWindow only reports prior visibility, never failure).
unsafe fn set_panel_hidden(hwnd: HWND, hidden: bool) -> Result<(), Error> {
    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
    unsafe {
        if hidden {
            SetWindowPos(hwnd, None, 0, 0, 0, 0, flags | SWP_NOZORDER | SWP_HIDEWINDOW)
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
            WM_DISPLAYCHANGE | WM_DPICHANGED => {
                reconcile_panels();
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
            WM_NCHITTEST => return LRESULT(HTTRANSPARENT as isize),
            WM_ERASEBKGND => return LRESULT(1),
            WM_PAINT => {
                let reading = lock_state().reading; // guard dropped before any GDI call
                if let Some(scope) = PaintScope::begin(hwnd) {
                    paint_panel(scope.hdc(), hwnd, reading);
                }
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

// ---------------------------------------------------------------- rendering

fn percent_text(value: Option<f32>) -> String {
    value
        .map(|v| format!("{:>3}%", (v * 100.0) as u32))
        .unwrap_or_else(|| " --%".into())
}

fn paint_panel(hdc: HDC, hwnd: HWND, reading: Option<Reading>) {
    // Every GDI object below is an RAII guard; selection scopes drop before their objects.
    unsafe {
        let mut rect = RECT::default();
        if GetClientRect(hwnd, &mut rect).is_err() {
            diag::last_error("GetClientRect", "paint");
        }
        if let Some(background) =
            GdiObject::<HBRUSH>::new(CreateSolidBrush(COLORREF(0x00151515)), "CreateSolidBrush")
        {
            FillRect(hdc, &rect, background.get());
        }
        let ring = RECT {
            left: 12,
            top: 12,
            right: 62,
            bottom: 62,
        };
        if let Some(track) =
            GdiObject::new(CreatePen(PS_SOLID, 4, COLORREF(0x00555555)), "CreatePen")
            && let Some(_selected) = SelectScope::select(hdc, track.get().into(), "SelectObject")
        {
            let _ = Ellipse(hdc, ring.left, ring.top, ring.right, ring.bottom);
        }
        if let Some(value) = reading.and_then(|r| r.cpu)
            && let Some(arc) =
                GdiObject::new(CreatePen(PS_SOLID, 4, COLORREF(0x0000cc66)), "CreatePen")
            && let Some(_selected) = SelectScope::select(hdc, arc.get().into(), "SelectObject")
        {
            let angle = value.clamp(0.0, 1.0) * std::f32::consts::TAU;
            let center_x = (ring.left + ring.right) as f32 / 2.0;
            let center_y = (ring.top + ring.bottom) as f32 / 2.0;
            let radius_x = (ring.right - ring.left) as f32 / 2.0;
            let radius_y = (ring.bottom - ring.top) as f32 / 2.0;
            let end_x = (center_x + radius_x * angle.cos()) as i32;
            let end_y = (center_y - radius_y * angle.sin()) as i32;
            let _ = Arc(
                hdc,
                ring.left,
                ring.top,
                ring.right,
                ring.bottom,
                ring.right,
                ring.top + 25,
                end_x,
                end_y,
            );
        }
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(0x00ffffff));
        let cpu: Vec<u16> = format!("CPU {}", percent_text(reading.and_then(|r| r.cpu)))
            .encode_utf16()
            .collect();
        let _ = TextOutW(hdc, 70, 19, &cpu);
        let details: Vec<u16> = format!(
            "M {}  D {}",
            percent_text(reading.and_then(|r| r.memory)),
            percent_text(reading.and_then(|r| r.disk))
        )
        .encode_utf16()
        .collect();
        let _ = TextOutW(hdc, 8, 62, &details);
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
    fn percent_text_formats_known_and_unknown() {
        assert_eq!(percent_text(Some(0.5)), " 50%");
        assert_eq!(percent_text(None), " --%");
    }
    #[test]
    fn production_sampling_cadence_matches_helper() {
        assert_eq!(interval_ms(2, 1, 1), HIDDEN_INTERVAL_MS);
        assert_eq!(interval_ms(2, 1, 0), VISIBLE_INTERVAL_MS);
    }
}

#[cfg(test)]
mod visibility_cases;
