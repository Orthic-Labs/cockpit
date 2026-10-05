#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::mem::size_of;
use std::sync::{Mutex, OnceLock};
use std::ffi::c_void;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, COLORREF, FILETIME, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::{
    Arc, BeginPaint, CreatePen, CreateSolidBrush, DeleteObject, Ellipse, EndPaint, EnumDisplayMonitors, FillRect, GetMonitorInfoW,
    InvalidateRect, MonitorFromWindow, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TextOutW, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW, PS_SOLID, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS_NAME: PCWSTR = w!("CockpitM0NativePill");
const TIMER_ID: usize = 7;

#[derive(Clone, Copy, PartialEq)]
struct Reading { cpu: Option<f32>, memory: Option<f32>, disk: Option<f32> }

#[derive(Default)]
struct AppState {
    windows: Vec<isize>,
    timer_owner: Option<isize>,
    previous_times: Option<(u64, u64, u64)>,
    reading: Option<Reading>,
    hidden: bool,
}

static STATE: OnceLock<Mutex<AppState>> = OnceLock::new();

fn state() -> &'static Mutex<AppState> { STATE.get_or_init(|| Mutex::new(AppState::default())) }

fn monitor_info() -> MONITORINFOEXW {
    MONITORINFOEXW {
        monitorInfo: MONITORINFO { cbSize: size_of::<MONITORINFOEXW>() as u32, ..Default::default() },
        ..Default::default()
    }
}

fn hwnd_key(hwnd: HWND) -> isize { hwnd.0 as isize }
fn hwnd_from_key(key: isize) -> HWND { HWND(key as *mut c_void) }

fn main() -> windows::core::Result<()> {
    let instance = unsafe { GetModuleHandleW(None)? };
    let class = WNDCLASSW {
        hInstance: instance.into(),
        lpszClassName: CLASS_NAME,
        lpfnWndProc: Some(window_proc),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        ..Default::default()
    };
    unsafe { RegisterClassW(&class); }
    reconcile_monitors();
    update_reading_and_visibility();
    unsafe {
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).into() {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

unsafe extern "system" fn enum_monitor(monitor: HMONITOR, _: HDC, _: *mut RECT, _: LPARAM) -> BOOL {
    // Win32 invokes callback with valid monitor handle during synchronous enumeration.
    unsafe {
    let mut info = monitor_info();
    if GetMonitorInfoW(monitor, &mut info.monitorInfo).as_bool() {
        let id = monitor_id(&info);
        let title: Vec<u16> = format!("Cockpit M0 {id}").encode_utf16().chain(std::iter::once(0)).collect();
        let rect = info.monitorInfo.rcMonitor;
        let width = 132;
        let height = 76;
        let x = rect.right - width - 12;
        let y = rect.top + (rect.bottom - rect.top - height) / 2;
        let ex = WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
        let hwnd = CreateWindowExW(ex, CLASS_NAME, PCWSTR(title.as_ptr()), WS_POPUP,
            x, y, width, height, None, None, Some(GetModuleHandleW(None).unwrap().into()), None);
        if let Ok(hwnd) = hwnd {
            SetLayeredWindowAttributes(hwnd, COLORREF(0), 238, LWA_ALPHA).ok();
            let key = hwnd_key(hwnd);
            let start_timer = {
                let mut app = state().lock().unwrap();
                app.windows.push(key);
                if app.timer_owner.is_none() {
                    app.timer_owner = Some(key);
                    true
                } else { false }
            };
            if start_timer { SetTimer(Some(hwnd), TIMER_ID, 2_000, None); }
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }
    true.into()
    }
}

fn reconcile_monitors() {
    let old = {
        let mut app = state().lock().unwrap();
        app.timer_owner = None;
        app.hidden = false;
        std::mem::take(&mut app.windows)
    };
    for key in old { unsafe { DestroyWindow(hwnd_from_key(key)); } }
    unsafe { EnumDisplayMonitors(None, None, Some(enum_monitor), LPARAM(0)); }
}

fn monitor_id(info: &MONITORINFOEXW) -> String {
    let end = info.szDevice.iter().position(|c| *c == 0).unwrap_or(info.szDevice.len());
    String::from_utf16_lossy(&info.szDevice[..end])
}

extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match message {
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
            WM_TIMER if wparam.0 == TIMER_ID => {
                update_reading_and_visibility();
                return LRESULT(0);
            }
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut paint);
                paint_ring(hdc, hwnd);
                EndPaint(hwnd, &paint);
                return LRESULT(0);
            }
            WM_DISPLAYCHANGE => { reconcile_monitors(); update_reading_and_visibility(); return LRESULT(0); }
            WM_NCHITTEST => return LRESULT(HTTRANSPARENT as isize),
            _ => {}
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

fn update_reading_and_visibility() {
    let reading = read_system();
    let mut app = state().lock().unwrap();
    let old = app.reading;
    app.reading = Some(reading);

    let own = app.windows.clone();
    let mut any_hidden = false;
    for key in &own {
        let hwnd = hwnd_from_key(*key);
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        let hidden = unsafe { monitor_has_fullscreen_occupancy(monitor, &own) };
        any_hidden |= hidden;
        unsafe {
            if hidden { ShowWindow(hwnd, SW_HIDE); }
            else { ShowWindow(hwnd, SW_SHOWNOACTIVATE); }
            if old != Some(reading) && !hidden { InvalidateRect(Some(hwnd), None, false); }
        }
    }
    let visibility_changed = app.hidden != any_hidden;
    app.hidden = any_hidden;
    if visibility_changed {
        for key in own.iter().copied() { unsafe { InvalidateRect(Some(hwnd_from_key(key)), None, false); } }
    }
    // One owner timer per pane; hidden panes slow to 10 seconds. This is a sampling gate,
    // not a blocking sleep, so all monitor occupancy checks continue independently.
    if let Some(owner) = app.timer_owner {
        unsafe { SetTimer(Some(hwnd_from_key(owner)), TIMER_ID, if any_hidden { 10_000 } else { 2_000 }, None); }
    }
}

fn read_system() -> Reading {
    let mut idle = FILETIME::default(); let mut kernel = FILETIME::default(); let mut user = FILETIME::default();
    let cpu = {
        let mut app = state().lock().unwrap();
        let ok = unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).is_ok() };
        if !ok { return Reading { cpu: None, memory: read_memory(), disk: read_disk() }; }
        let current = (filetime(idle), filetime(kernel), filetime(user));
        let cpu = app.previous_times.and_then(|(pi, pk, pu)| {
            let total = (current.1 - pk).saturating_add(current.2 - pu);
            let busy = total.saturating_sub(current.0 - pi);
            if total == 0 { None } else { Some(busy as f32 / total as f32) }
        });
        app.previous_times = Some(current);
        cpu
    };
    let memory = read_memory();
    let disk = read_disk();
    Reading { cpu: cpu.map(|value| value.clamp(0.0, 1.0)), memory, disk }
}

fn read_memory() -> Option<f32> {
    let mut memory = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    if !unsafe { GlobalMemoryStatusEx(&mut memory).is_ok() } { return None; }
    Some((1.0 - (memory.ullAvailPhys as f32 / memory.ullTotalPhys.max(1) as f32)).clamp(0.0, 1.0))
}

fn read_disk() -> Option<f32> {
    let mut free = 0u64; let mut total = 0u64;
    if !unsafe { GetDiskFreeSpaceExW(PCWSTR::null(), Some(&mut free), Some(&mut total), None).is_ok() } || total == 0 { return None; }
    Some((1.0 - free as f32 / total as f32).clamp(0.0, 1.0))
}

fn filetime(value: FILETIME) -> u64 { ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64 }

unsafe fn monitor_has_fullscreen_occupancy(monitor: HMONITOR, own: &[isize]) -> bool {
    // Monitor handle and callback context remain valid for this synchronous call.
    unsafe {
    let mut info = monitor_info();
    if !GetMonitorInfoW(monitor, &mut info.monitorInfo).as_bool() { return false; }
    let wanted = info.monitorInfo.rcMonitor;
    let mut context = (false, wanted, own.as_ptr(), own.len());
    EnumWindows(Some(enum_visible_window), LPARAM(&mut context as *mut _ as isize));
    context.0
    }
}

unsafe extern "system" fn enum_visible_window(hwnd: HWND, data: LPARAM) -> BOOL {
    // EnumWindows supplies live HWNDs; data points to synchronous stack context.
    unsafe {
    let state = &mut *(data.0 as *mut (bool, RECT, *const isize, usize));
    if state.0 || !IsWindowVisible(hwnd).as_bool() || state.2.is_null() { return if state.0 { false.into() } else { true.into() }; }
    if is_shell_desktop_window(hwnd) { return true.into(); }
    let owned = GetWindow(hwnd, GW_OWNER).map(|owner| !owner.0.is_null()).unwrap_or(false);
    if (0..state.3).any(|i| *state.2.add(i) == hwnd_key(hwnd)) || owned { return true.into(); }
    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
    if ex & WS_EX_TOOLWINDOW.0 != 0 { return true.into(); }
    let mut cloaked = 0u32;
    if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, size_of::<u32>() as u32).is_ok() && cloaked != 0 { return true.into(); }
    let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
    let mut rect = RECT::default();
    if GetWindowRect(hwnd, &mut rect).is_err() { return true.into(); }
    if rect.right <= state.1.left || rect.left >= state.1.right || rect.bottom <= state.1.top || rect.top >= state.1.bottom { return true.into(); }
    state.0 = is_fullscreen_geometry(rect, state.1) && is_borderless_style(style);
    false.into()
    }
}

fn is_shell_desktop_window(hwnd: HWND) -> bool {
    let mut class = [0u16; 256];
    let length = unsafe { GetClassNameW(hwnd, &mut class) };
    let name = String::from_utf16_lossy(&class[..length.max(0) as usize]);
    matches!(name.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Windows.UI.Core.CoreWindow")
}

fn is_fullscreen_geometry(window: RECT, monitor: RECT) -> bool {
    window.left <= monitor.left && window.top <= monitor.top && window.right >= monitor.right && window.bottom >= monitor.bottom
}

fn is_borderless_style(style: u32) -> bool { style & (WS_CAPTION.0 | WS_THICKFRAME.0) == 0 }

unsafe fn paint_ring(hdc: HDC, hwnd: HWND) {
    // WM_PAINT supplies a valid HDC; GDI handles stay alive for this paint operation.
    unsafe {
    let mut rect = RECT::default(); let _ = GetClientRect(hwnd, &mut rect);
    let bg = CreateSolidBrush(COLORREF(0x00151515)); FillRect(hdc, &rect, bg); DeleteObject(bg.into());
    let pen = CreatePen(PS_SOLID, 4, COLORREF(0x00555555)); let old = SelectObject(hdc, pen.into());
    let ring = RECT { left: 12, top: 12, right: 62, bottom: 62 };
    Ellipse(hdc, ring.left, ring.top, ring.right, ring.bottom);
    SelectObject(hdc, old); DeleteObject(pen.into());
    let reading = state().lock().unwrap().reading;
    if let Some(value) = reading.and_then(|r| r.cpu) {
        let green = CreatePen(PS_SOLID, 4, COLORREF(0x0000cc66)); let old = SelectObject(hdc, green.into());
        let angle = value.clamp(0.0, 1.0) * std::f32::consts::TAU;
        let center_x = (ring.left + ring.right) as f32 / 2.0;
        let center_y = (ring.top + ring.bottom) as f32 / 2.0;
        let radius_x = (ring.right - ring.left) as f32 / 2.0;
        let radius_y = (ring.bottom - ring.top) as f32 / 2.0;
        let end_x = (center_x + radius_x * angle.cos()) as i32;
        let end_y = (center_y - radius_y * angle.sin()) as i32;
        Arc(hdc, ring.left, ring.top, ring.right, ring.bottom, ring.right, ring.top + 25, end_x, end_y);
        SelectObject(hdc, old); DeleteObject(green.into());
    }
    SetBkMode(hdc, TRANSPARENT); SetTextColor(hdc, COLORREF(0x00ffffff));
    let cpu_text = reading.and_then(|r| r.cpu).map(|v| format!("{:>3}%", (v * 100.0) as u32)).unwrap_or_else(|| " --%".into());
    let text: Vec<u16> = format!("CPU {cpu_text}").encode_utf16().collect();
    TextOutW(hdc, 70, 19, &text);
    let memory_text = reading.and_then(|r| r.memory).map(|v| format!("{:>3}%", (v * 100.0) as u32)).unwrap_or_else(|| " --%".into());
    let disk_text = reading.and_then(|r| r.disk).map(|v| format!("{:>3}%", (v * 100.0) as u32)).unwrap_or_else(|| " --%".into());
    let details: Vec<u16> = format!("M {memory_text}  D {disk_text}").encode_utf16().collect();
    TextOutW(hdc, 8, 62, &details);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn exact_monitor_bounds_are_fullscreen() { assert!(is_fullscreen_geometry(RECT{left:0,top:0,right:100,bottom:100}, RECT{left:0,top:0,right:100,bottom:100})); }
    #[test] fn inset_window_is_not_fullscreen() { assert!(!is_fullscreen_geometry(RECT{left:1,top:0,right:100,bottom:100}, RECT{left:0,top:0,right:100,bottom:100})); }
    #[test] fn captioned_window_is_not_borderless() { assert!(!is_borderless_style(WS_CAPTION.0)); }
}
