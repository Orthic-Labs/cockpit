//! macOS-style screenshots.
//!
//! * Alt+Shift+4: drag a region (crosshair, dimmed outside, live size label); Space switches
//!   to window picking (hover highlights a window, click captures its visible rectangle);
//!   Esc or right click cancels.
//! * Alt+Shift+5: a small toolbar (Entire Screen, Window, Selection, Desktop / Clipboard
//!   destination, Cancel).
//!
//! Everything runs on one dedicated UI thread with its own message loop. `keys.rs` posts
//! thread messages here from the keyboard hook (hotkeys, Esc, Space). The overlay and
//! toolbar windows are layered, topmost and `WS_EX_NOACTIVATE`: they never take focus, so
//! the previous foreground window is untouched (and restored defensively afterwards); the
//! hook, not window focus, delivers Esc and Space.
//!
//! Capture: the overlays are destroyed, DWM is flushed, then the region is copied from the
//! screen DC with `BitBlt` (process is per-monitor DPI aware, so coordinates are physical
//! pixels on the virtual desktop). Output: PNG through WIC to the Desktop known folder
//! (`Screenshot YYYY-MM-DD at HH.MM.SS.png`) unless the destination is Clipboard only, plus
//! `CF_DIB` on the clipboard. No sound. A small non-activating thumbnail card sits bottom
//! right for five seconds; clicking it opens the file.
//!
//! Re-entrancy rule: window procedures only borrow the thread-local state through
//! `try_borrow_mut` for plain data access. Anything that destroys or creates windows runs
//! from the thread-message loop (`finish`) with no borrow held; procedures just queue a
//! `Finish` and post `WM_FINISH`.

use crate::canvas::{Canvas, Mask};
use crate::diag;
use crate::raii::{ClassGuard, OwnedWindow, hwnd_from_key, hwnd_key};
use crate::surface::{Surface, TextPainter, present};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::fs;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use windows::Win32::Foundation::{
    COLORREF, E_FAIL, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmFlush, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFOHEADER, BLENDFUNCTION, BitBlt, CAPTUREBLT, EnumDisplayMonitors, GdiFlush,
    GetDC, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    ROP_CODE, ReleaseDC, SRCCOPY, ValidateRect,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA,
    IWICBitmapFrameEncode, IWICImagingFactory, WICBitmapEncoderNoCache,
};
use windows::Win32::System::Com::StructuredStorage::IPropertyBag2;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize, IStream,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalFree, GlobalLock, GlobalUnlock,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId, Sleep};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, KNOWN_FOLDER_FLAG, SHGetKnownFolderPath, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, Error, IUnknown, Interface, PCWSTR, w};

const OVERLAY_CLASS: PCWSTR = w!("PulseShotOverlay");
const TOOLBAR_CLASS: PCWSTR = w!("PulseShotToolbar");
const THUMB_CLASS: PCWSTR = w!("PulseShotThumb");
const PLAIN_CLASS: PCWSTR = w!("PulseShotPlain");

const WM_REQUEST: u32 = WM_APP + 1;
const WM_KEY_ESCAPE: u32 = WM_APP + 2;
const WM_KEY_SPACE: u32 = WM_APP + 3;
const WM_FINISH: u32 = WM_APP + 4;
const THUMB_TIMER: usize = 1;
const THUMB_MS: u32 = 5000;
/// A drag smaller than this (either side) is treated as a stray click.
const MIN_SELECTION: i32 = 4;
const CF_DIB: u32 = 8;

// Premultiplied 0xAARRGGBB overlay pixels. Alpha 1 (not 0) keeps "empty" areas hit-testable.
const IDLE: u32 = 0x0100_0000;
const DIM: u32 = 0x7000_0000;
const HOLE: u32 = 0x0100_0000;
const EDGE: u32 = 0xFFFF_FFFF;

/// What the hotkey asks for.
#[derive(Clone, Copy, Debug)]
pub enum Request {
    Region,
    Toolbar,
}

static THREAD_ID: AtomicU32 = AtomicU32::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static SAVE_TO_DESKTOP: AtomicBool = AtomicBool::new(true);

pub fn set_save_to_desktop(value: bool) {
    SAVE_TO_DESKTOP.store(value, Ordering::Relaxed);
}

pub fn save_to_desktop() -> bool {
    SAVE_TO_DESKTOP.load(Ordering::Relaxed)
}

/// A selection session (overlay or toolbar) is on screen; the hook then owns Esc and Space.
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

fn post(message: u32, wparam: usize) {
    let id = THREAD_ID.load(Ordering::Relaxed);
    if id != 0 {
        // Failure means the thread is gone or has no queue yet; nothing to recover.
        let _ = unsafe { PostThreadMessageW(id, message, WPARAM(wparam), LPARAM(0)) };
    }
}

pub fn request(request: Request) {
    if active() {
        return;
    }
    post(
        WM_REQUEST,
        match request {
            Request::Region => 0,
            Request::Toolbar => 1,
        },
    );
}

pub fn key_escape() {
    post(WM_KEY_ESCAPE, 0);
}

pub fn key_space() {
    post(WM_KEY_SPACE, 0);
}

// ---------------------------------------------------------------- geometry

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RectI {
    l: i32,
    t: i32,
    r: i32,
    b: i32,
}

impl RectI {
    fn from_rect(rect: RECT) -> Self {
        Self {
            l: rect.left,
            t: rect.top,
            r: rect.right,
            b: rect.bottom,
        }
    }
    fn between(a: (i32, i32), b: (i32, i32)) -> Self {
        Self {
            l: a.0.min(b.0),
            t: a.1.min(b.1),
            r: a.0.max(b.0),
            b: a.1.max(b.1),
        }
    }
    fn w(self) -> i32 {
        self.r - self.l
    }
    fn h(self) -> i32 {
        self.b - self.t
    }
    fn shifted(self, dx: i32, dy: i32) -> Self {
        Self {
            l: self.l + dx,
            t: self.t + dy,
            r: self.r + dx,
            b: self.b + dy,
        }
    }
    fn inflate(self, n: i32) -> Self {
        Self {
            l: self.l - n,
            t: self.t - n,
            r: self.r + n,
            b: self.b + n,
        }
    }
    fn contains(self, x: i32, y: i32) -> bool {
        (self.l..self.r).contains(&x) && (self.t..self.b).contains(&y)
    }
    fn intersect(self, other: Self) -> Option<Self> {
        let out = Self {
            l: self.l.max(other.l),
            t: self.t.max(other.t),
            r: self.r.min(other.r),
            b: self.b.min(other.b),
        };
        (out.w() > 0 && out.h() > 0).then_some(out)
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn cursor() -> (i32, i32) {
    let mut point = POINT::default();
    if unsafe { GetCursorPos(&mut point) }.is_err() {
        return (0, 0);
    }
    (point.x, point.y)
}

fn virtual_screen() -> RectI {
    let (x, y, w, h) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    RectI {
        l: x,
        t: y,
        r: x + w,
        b: y + h,
    }
}

struct MonitorGeo {
    monitor: RectI,
    work: RectI,
    scale: f32,
}

fn monitor_geo(point: (i32, i32)) -> MonitorGeo {
    let fallback = virtual_screen();
    let monitor = unsafe {
        MonitorFromPoint(
            POINT {
                x: point.0,
                y: point.1,
            },
            MONITOR_DEFAULTTONEAREST,
        )
    };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let (rect, work) = if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        (
            RectI::from_rect(info.rcMonitor),
            RectI::from_rect(info.rcWork),
        )
    } else {
        (fallback, fallback)
    };
    let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
    let scale =
        match unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) } {
            Ok(()) if dpi_x > 0 => dpi_x as f32 / 96.0,
            _ => 1.0,
        };
    MonitorGeo {
        monitor: rect,
        work,
        scale,
    }
}

// ---------------------------------------------------------------- thread-local state

#[derive(Clone, Copy)]
enum Finish {
    Cancel,
    Capture(RectI),
    Pick { window_mode: bool },
}

struct Session {
    prev_fg: isize,
    toolbar: Option<Toolbar>,
    pick: Option<Pick>,
}

struct Thumb {
    window: OwnedWindow,
    path: Option<PathBuf>,
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    static PAINTER: RefCell<Option<TextPainter>> = const { RefCell::new(None) };
    static THUMB: RefCell<Option<Thumb>> = const { RefCell::new(None) };
    static PENDING: Cell<Option<Finish>> = const { Cell::new(None) };
    static OWNER: Cell<isize> = const { Cell::new(0) };
}

fn with_session<R>(f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    SESSION.with(|cell| {
        let mut guard = cell.try_borrow_mut().ok()?;
        guard.as_mut().map(f)
    })
}

fn take_session() -> Option<Session> {
    SESSION.with(|cell| {
        cell.try_borrow_mut()
            .ok()
            .and_then(|mut guard| guard.take())
    })
}

fn take_thumb() -> Option<Thumb> {
    THUMB.with(|cell| {
        cell.try_borrow_mut()
            .ok()
            .and_then(|mut guard| guard.take())
    })
}

fn text(label: &str, px: i32, bold: bool) -> Option<Mask> {
    PAINTER.with(|cell| {
        let mut guard = cell.try_borrow_mut().ok()?;
        guard
            .as_mut()
            .and_then(|painter| painter.render(label, px, bold))
    })
}

fn queue(finish: Finish) {
    PENDING.set(Some(finish));
    post(WM_FINISH, 0);
}

// ---------------------------------------------------------------- overlay

fn fill_rect(pixels: &mut [u32], width: usize, height: usize, rect: RectI, value: u32) {
    let clamp_x = |v: i32| v.clamp(0, width as i32) as usize;
    let clamp_y = |v: i32| v.clamp(0, height as i32) as usize;
    let (x0, x1) = (clamp_x(rect.l), clamp_x(rect.r));
    if x0 >= x1 {
        return;
    }
    for y in clamp_y(rect.t)..clamp_y(rect.b) {
        pixels[y * width + x0..y * width + x1].fill(value);
    }
}

/// Publishes an existing DIB as the window's per-pixel-alpha contents.
fn publish(hwnd: HWND, surface: &Surface) {
    let size = SIZE {
        cx: surface.width as i32,
        cy: surface.height as i32,
    };
    let source = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: 0, // AC_SRC_OVER
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: 1, // AC_SRC_ALPHA
    };
    // SAFETY: pointers refer to locals that outlive the synchronous call; the DC holds the DIB.
    let result = unsafe {
        UpdateLayeredWindow(
            hwnd,
            None,
            None,
            Some(&size),
            Some(surface.dc()),
            Some(&source),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )
    };
    if let Err(error) = result {
        diag::win32_error("UpdateLayeredWindow", &error, "shot_overlay");
    }
}

/// One full-monitor overlay. Pixels: nearly transparent until a selection exists, then
/// dimmed with an almost transparent hole and a white edge around the selection.
struct Overlay {
    window: OwnedWindow,
    surface: Surface,
    rect: RectI,
    dimmed: bool,
    hole: Option<RectI>,
}

impl Overlay {
    fn new(rect: RectI, instance: HINSTANCE) -> Option<Self> {
        let window = match OwnedWindow::create(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            OVERLAY_CLASS,
            &wide("Pulse Screenshot"),
            WS_POPUP,
            (rect.l, rect.t, rect.w(), rect.h()),
            instance,
        ) {
            Ok(window) => window,
            Err(error) => {
                diag::win32_error("CreateWindowExW", &error, "shot_overlay");
                return None;
            }
        };
        let surface = Surface::new(rect.w() as usize, rect.h() as usize)?;
        let mut overlay = Self {
            window,
            surface,
            rect,
            dimmed: true, // forces the first paint to fill everything
            hole: None,
        };
        overlay.paint(None, false);
        let _ = unsafe { ShowWindow(overlay.window.hwnd(), SW_SHOWNOACTIVATE) };
        Some(overlay)
    }

    /// `hole` is in virtual-screen coordinates. Repaints only what changed.
    fn paint(&mut self, hole: Option<RectI>, dim: bool) {
        let local = hole.map(|r| r.shifted(-self.rect.l, -self.rect.t));
        let (width, height) = (self.surface.width, self.surface.height);
        let everything = dim != self.dimmed;
        if !everything && local == self.hole {
            return;
        }
        let bounds = RectI {
            l: 0,
            t: 0,
            r: width as i32,
            b: height as i32,
        };
        let touches =
            |r: Option<RectI>| r.is_some_and(|r| r.inflate(2).intersect(bounds).is_some());
        if !everything && !touches(local) && !touches(self.hole) {
            self.hole = local;
            return;
        }
        let base = if dim { DIM } else { IDLE };
        let previous = self.hole;
        let pixels = self.surface.pixels_mut();
        if everything {
            pixels.fill(base);
        } else if let Some(old) = previous {
            fill_rect(pixels, width, height, old.inflate(2), base);
        }
        if let Some(rect) = local {
            fill_rect(pixels, width, height, rect.inflate(1), EDGE);
            fill_rect(pixels, width, height, rect, HOLE);
        }
        self.dimmed = dim;
        self.hole = local;
        publish(self.window.hwnd(), &self.surface);
    }
}

// ---------------------------------------------------------------- region / window picking

struct Pick {
    overlays: Vec<Overlay>,
    label: OwnedWindow,
    label_shown: bool,
    scale: f32,
    window_mode: bool,
    drag: Option<(i32, i32)>,
    cursor: (i32, i32),
    hover: Option<RectI>,
}

unsafe extern "system" fn enum_monitor_rect(
    _: HMONITOR,
    _: HDC,
    rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: invoked synchronously by EnumDisplayMonitors; `data` is the caller's Vec.
    unsafe {
        let out = &mut *(data.0 as *mut Vec<RectI>);
        if !rect.is_null() {
            out.push(RectI::from_rect(*rect));
        }
    }
    true.into()
}

struct PickContext {
    point: POINT,
    pid: u32,
    found: Option<RectI>,
}

unsafe extern "system" fn enum_pick(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: EnumWindows supplies live handles; `data` is the caller's PickContext. Read-only
    // window queries only.
    unsafe {
        let context = &mut *(data.0 as *mut PickContext);
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return true.into();
        }
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == context.pid {
            return true.into(); // our own overlays and the notch
        }
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex_style & (WS_EX_NOACTIVATE.0 | WS_EX_TRANSPARENT.0 | WS_EX_TOOLWINDOW.0) != 0 {
            return true.into();
        }
        let mut class = [0u16; 64];
        let length = GetClassNameW(hwnd, &mut class).max(0) as usize;
        let name = String::from_utf16_lossy(&class[..length]);
        if matches!(name.as_str(), "Progman" | "WorkerW") {
            return true.into(); // the desktop is not a window to pick
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            size_of::<u32>() as u32,
        );
        if cloaked != 0 {
            return true.into();
        }
        let mut rect = RECT::default();
        let framed = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut RECT as *mut c_void,
            size_of::<RECT>() as u32,
        )
        .is_ok();
        if !framed && GetWindowRect(hwnd, &mut rect).is_err() {
            return true.into();
        }
        let bounds = RectI::from_rect(rect);
        if bounds.w() > 0 && bounds.h() > 0 && bounds.contains(context.point.x, context.point.y) {
            context.found = Some(bounds);
            return false.into(); // topmost match: stop
        }
        true.into()
    }
}

/// Visible rectangle of the topmost real window under `point`.
fn window_at(point: (i32, i32)) -> Option<RectI> {
    let mut context = PickContext {
        point: POINT {
            x: point.0,
            y: point.1,
        },
        pid: unsafe { GetCurrentProcessId() },
        found: None,
    };
    // Stopping early makes EnumWindows report an error by design.
    let _ = unsafe { EnumWindows(Some(enum_pick), LPARAM(&mut context as *mut _ as isize)) };
    context.found
}

impl Pick {
    fn new(window_mode: bool, instance: HINSTANCE) -> Option<Self> {
        let mut monitors: Vec<RectI> = Vec::new();
        // SAFETY: the callback only pushes into `monitors`, which outlives the call.
        let ok = unsafe {
            EnumDisplayMonitors(
                None,
                None,
                Some(enum_monitor_rect),
                LPARAM(&mut monitors as *mut _ as isize),
            )
        }
        .as_bool();
        if !ok || monitors.is_empty() {
            diag::last_error("EnumDisplayMonitors", "shot_pick");
            return None;
        }
        let overlays: Vec<Overlay> = monitors
            .iter()
            .filter_map(|rect| Overlay::new(*rect, instance))
            .collect();
        if overlays.is_empty() {
            return None;
        }
        let label = match OwnedWindow::create(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
            PLAIN_CLASS,
            &wide("Pulse Screenshot Size"),
            WS_POPUP,
            (0, 0, 1, 1),
            instance,
        ) {
            Ok(window) => window,
            Err(error) => {
                diag::win32_error("CreateWindowExW", &error, "shot_label");
                return None;
            }
        };
        let here = cursor();
        let mut pick = Self {
            overlays,
            label,
            label_shown: false,
            scale: monitor_geo(here).scale,
            window_mode,
            drag: None,
            cursor: here,
            hover: None,
        };
        if window_mode {
            pick.hover = window_at(here);
        }
        pick.refresh();
        Some(pick)
    }

    fn selection(&self) -> Option<RectI> {
        if self.window_mode {
            self.hover
        } else {
            self.drag.map(|start| RectI::between(start, self.cursor))
        }
    }

    fn refresh(&mut self) {
        let hole = self.selection();
        let dim = hole.is_some();
        for overlay in &mut self.overlays {
            overlay.paint(hole, dim);
        }
        match hole {
            Some(rect) => self.show_label(rect),
            None => {
                if self.label_shown {
                    let _ = unsafe { ShowWindow(self.label.hwnd(), SW_HIDE) };
                    self.label_shown = false;
                }
            }
        }
    }

    fn show_label(&mut self, rect: RectI) {
        let s = self.scale;
        let caption = format!("{} \u{00D7} {}", rect.w(), rect.h());
        let Some(mask) = text(&caption, (13.0 * s).round() as i32, false) else {
            return;
        };
        let (pad_x, pad_y) = ((8.0 * s) as i32, (4.0 * s) as i32);
        let (width, height) = (
            mask.width as i32 + 2 * pad_x,
            mask.height as i32 + 2 * pad_y,
        );
        let mut canvas = Canvas::new(width as usize, height as usize);
        canvas.fill_round_rect(
            0.0,
            0.0,
            width as f32,
            height as f32,
            [6.0 * s; 4],
            0x1C1C1E,
            0.92,
        );
        canvas.draw_mask(&mask, pad_x, pad_y, 0xFFFFFF, 1.0);
        let screen = monitor_geo(self.cursor).monitor;
        let mut x = self.cursor.0 + (16.0 * s) as i32;
        let mut y = self.cursor.1 + (20.0 * s) as i32;
        if x + width > screen.r {
            x = self.cursor.0 - width - (8.0 * s) as i32;
        }
        if y + height > screen.b {
            y = self.cursor.1 - height - (8.0 * s) as i32;
        }
        if let Err(error) = present(self.label.hwnd(), &canvas, Some((x, y))) {
            diag::win32_error("UpdateLayeredWindow", &error, "shot_label");
            return;
        }
        if !self.label_shown {
            let _ = unsafe { ShowWindow(self.label.hwnd(), SW_SHOWNOACTIVATE) };
            self.label_shown = true;
        }
    }
}

fn pick_down() {
    with_session(|session| {
        if let Some(pick) = session.pick.as_mut()
            && !pick.window_mode
        {
            pick.cursor = cursor();
            pick.drag = Some(pick.cursor);
            pick.refresh();
        }
    });
}

fn pick_move() {
    with_session(|session| {
        let Some(pick) = session.pick.as_mut() else {
            return;
        };
        let here = cursor();
        if here == pick.cursor {
            return;
        }
        pick.cursor = here;
        if pick.window_mode {
            pick.hover = window_at(here);
            pick.refresh();
        } else if pick.drag.is_some() {
            pick.refresh();
        }
    });
}

fn pick_up() {
    let outcome = with_session(|session| {
        let pick = session.pick.as_mut()?;
        let here = cursor();
        pick.cursor = here;
        if pick.window_mode {
            pick.hover = window_at(here);
            return pick.hover.map(Finish::Capture);
        }
        let start = pick.drag.take()?;
        let rect = RectI::between(start, here);
        if rect.w() >= MIN_SELECTION && rect.h() >= MIN_SELECTION {
            Some(Finish::Capture(rect))
        } else {
            pick.refresh();
            None
        }
    })
    .flatten();
    let _ = unsafe { ReleaseCapture() };
    if let Some(finish) = outcome {
        queue(finish);
    }
}

fn pick_capture_lost() {
    with_session(|session| {
        if let Some(pick) = session.pick.as_mut()
            && pick.drag.take().is_some()
        {
            pick.refresh();
        }
    });
}

fn toggle_mode() {
    let _ = unsafe { ReleaseCapture() };
    with_session(|session| {
        if let Some(pick) = session.pick.as_mut() {
            pick.window_mode = !pick.window_mode;
            pick.drag = None;
            pick.cursor = cursor();
            pick.hover = if pick.window_mode {
                window_at(pick.cursor)
            } else {
                None
            };
            pick.refresh();
        }
    });
}

fn set_pick_cursor() {
    let window_mode = with_session(|session| session.pick.as_ref().map(|p| p.window_mode))
        .flatten()
        .unwrap_or(false);
    let id = if window_mode { IDC_HAND } else { IDC_CROSS };
    if let Ok(cursor) = unsafe { LoadCursorW(None, id) } {
        let _ = unsafe { SetCursor(Some(cursor)) };
    }
}

// ---------------------------------------------------------------- toolbar (Alt+Shift+5)

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Entire,
    Window,
    Region,
    ToDesktop,
    ToClipboard,
    Cancel,
    Caption,
}

struct Button {
    action: Action,
    mask: Mask,
    x: i32,
    w: i32,
}

struct Toolbar {
    window: OwnedWindow,
    buttons: Vec<Button>,
    hover: Option<usize>,
    width: i32,
    height: i32,
    scale: f32,
    monitor: RectI,
}

impl Toolbar {
    fn new(instance: HINSTANCE) -> Option<Self> {
        let here = cursor();
        let geo = monitor_geo(here);
        let s = geo.scale;
        let px = (13.0 * s).round() as i32;
        let pad = (14.0 * s) as i32;
        let gap = (6.0 * s) as i32;
        let inner = (12.0 * s) as i32;
        let group = (14.0 * s) as i32;
        let labels = [
            ("Entire Screen", Action::Entire),
            ("Window", Action::Window),
            ("Selection", Action::Region),
            ("Save to", Action::Caption),
            ("Desktop", Action::ToDesktop),
            ("Clipboard", Action::ToClipboard),
            ("Cancel", Action::Cancel),
        ];
        let mut buttons = Vec::with_capacity(labels.len());
        let mut x = pad;
        for (label, action) in labels {
            let mask = text(label, px, false)?;
            if matches!(action, Action::Caption | Action::Cancel) {
                x += group - gap;
            }
            let w = if action == Action::Caption {
                mask.width as i32 + (4.0 * s) as i32
            } else {
                mask.width as i32 + 2 * inner
            };
            buttons.push(Button { action, mask, x, w });
            x += w + gap;
        }
        let width = x - gap + pad;
        let height = (44.0 * s) as i32;
        let left = geo.work.l + (geo.work.w() - width) / 2;
        let top = geo.work.b - height - (28.0 * s) as i32;
        let window = match OwnedWindow::create(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            TOOLBAR_CLASS,
            &wide("Pulse Screenshot Toolbar"),
            WS_POPUP,
            (left, top, width, height),
            instance,
        ) {
            Ok(window) => window,
            Err(error) => {
                diag::win32_error("CreateWindowExW", &error, "shot_toolbar");
                return None;
            }
        };
        let toolbar = Self {
            window,
            buttons,
            hover: None,
            width,
            height,
            scale: s,
            monitor: geo.monitor,
        };
        toolbar.redraw();
        let _ = unsafe { ShowWindow(toolbar.window.hwnd(), SW_SHOWNOACTIVATE) };
        Some(toolbar)
    }

    fn button_band(&self) -> (f32, f32) {
        let top = 6.0 * self.scale;
        (top, self.height as f32 - 2.0 * top)
    }

    fn hit(&self, x: i32, y: i32) -> Option<usize> {
        let (top, height) = self.button_band();
        if !(top..=top + height).contains(&(y as f32)) {
            return None;
        }
        self.buttons
            .iter()
            .position(|b| b.action != Action::Caption && (b.x..b.x + b.w).contains(&x))
    }

    fn redraw(&self) {
        let s = self.scale;
        let mut canvas = Canvas::new(self.width as usize, self.height as usize);
        canvas.fill_round_rect(
            0.0,
            0.0,
            self.width as f32,
            self.height as f32,
            [12.0 * s; 4],
            0x2A2A2E,
            0.96,
        );
        let (top, height) = self.button_band();
        let to_desktop = save_to_desktop();
        for (index, button) in self.buttons.iter().enumerate() {
            let caption = button.action == Action::Caption;
            if !caption {
                let selected = match button.action {
                    Action::ToDesktop => to_desktop,
                    Action::ToClipboard => !to_desktop,
                    _ => false,
                };
                let fill = if selected {
                    0x0A84FF
                } else if self.hover == Some(index) {
                    0x4A4A52
                } else {
                    0x3A3A40
                };
                canvas.fill_round_rect(
                    button.x as f32,
                    top,
                    button.w as f32,
                    height,
                    [8.0 * s; 4],
                    fill,
                    1.0,
                );
            }
            let color = if caption { 0xA0A0A8 } else { 0xFFFFFF };
            let text_x = button.x + (button.w - button.mask.width as i32) / 2;
            let text_y = (self.height - button.mask.height as i32) / 2;
            canvas.draw_mask(&button.mask, text_x, text_y, color, 1.0);
        }
        if let Err(error) = present(self.window.hwnd(), &canvas, None) {
            diag::win32_error("UpdateLayeredWindow", &error, "shot_toolbar");
        }
    }
}

fn toolbar_hover(x: i32, y: i32) {
    with_session(|session| {
        if let Some(toolbar) = session.toolbar.as_mut() {
            let hit = toolbar.hit(x, y);
            if hit != toolbar.hover {
                toolbar.hover = hit;
                toolbar.redraw();
            }
        }
    });
}

fn toolbar_click(x: i32, y: i32) {
    let picked = with_session(|session| {
        let toolbar = session.toolbar.as_ref()?;
        let index = toolbar.hit(x, y)?;
        Some((toolbar.buttons[index].action, toolbar.monitor))
    })
    .flatten();
    let Some((action, monitor)) = picked else {
        return;
    };
    match action {
        Action::Entire => queue(Finish::Capture(monitor)),
        Action::Window => queue(Finish::Pick { window_mode: true }),
        Action::Region => queue(Finish::Pick { window_mode: false }),
        Action::Cancel => queue(Finish::Cancel),
        Action::ToDesktop | Action::ToClipboard => {
            set_save_to_desktop(action == Action::ToDesktop);
            with_session(|session| {
                if let Some(toolbar) = session.toolbar.as_ref() {
                    toolbar.redraw();
                }
            });
        }
        Action::Caption => {}
    }
}

// ---------------------------------------------------------------- session control

fn module_instance() -> Option<HINSTANCE> {
    unsafe { GetModuleHandleW(None) }.ok().map(Into::into)
}

fn install(session: Session) {
    SESSION.with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut() {
            *guard = Some(session);
        }
    });
    ACTIVE.store(true, Ordering::Relaxed);
}

fn begin(request: Request) {
    let busy = SESSION.with(|cell| {
        cell.try_borrow()
            .map(|guard| guard.is_some())
            .unwrap_or(true)
    });
    if busy {
        return;
    }
    let Some(instance) = module_instance() else {
        return;
    };
    let prev_fg = hwnd_key(unsafe { GetForegroundWindow() });
    let (toolbar, pick) = match request {
        Request::Region => (None, Pick::new(false, instance)),
        Request::Toolbar => (Toolbar::new(instance), None),
    };
    if toolbar.is_none() && pick.is_none() {
        diag::info("screenshot_session_failed", &[]);
        return;
    }
    install(Session {
        prev_fg,
        toolbar,
        pick,
    });
}

fn restore_foreground(previous: isize) {
    if previous == 0 {
        return;
    }
    let target = hwnd_from_key(previous);
    let current = hwnd_key(unsafe { GetForegroundWindow() });
    if current != previous && unsafe { IsWindow(Some(target)) }.as_bool() {
        let _ = unsafe { SetForegroundWindow(target) };
    }
}

fn finish(outcome: Finish) {
    let Some(session) = take_session() else {
        return;
    };
    let previous = session.prev_fg;
    drop(session); // destroys overlay / toolbar windows with no borrow held
    match outcome {
        Finish::Cancel => {
            ACTIVE.store(false, Ordering::Relaxed);
            restore_foreground(previous);
        }
        Finish::Pick { window_mode } => {
            let pick = module_instance().and_then(|instance| Pick::new(window_mode, instance));
            match pick {
                Some(pick) => install(Session {
                    prev_fg: previous,
                    toolbar: None,
                    pick: Some(pick),
                }),
                None => ACTIVE.store(false, Ordering::Relaxed),
            }
        }
        Finish::Capture(rect) => {
            ACTIVE.store(false, Ordering::Relaxed);
            capture_and_deliver(rect);
            restore_foreground(previous);
        }
    }
}

fn on_thread_message(message: &MSG) {
    match message.message {
        WM_REQUEST => begin(if message.wParam.0 == 1 {
            Request::Toolbar
        } else {
            Request::Region
        }),
        WM_KEY_ESCAPE => finish(Finish::Cancel),
        WM_KEY_SPACE => toggle_mode(),
        WM_FINISH => {
            if let Some(outcome) = PENDING.take() {
                finish(outcome);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- capture and delivery

fn capture(rect: RectI) -> Option<Surface> {
    let surface = Surface::new(rect.w() as usize, rect.h() as usize)?;
    // SAFETY: plain GDI; the screen DC is released on every path.
    unsafe {
        let screen = GetDC(None);
        if screen.0.is_null() {
            diag::last_error("GetDC", "shot_capture");
            return None;
        }
        let result = BitBlt(
            surface.dc(),
            0,
            0,
            rect.w(),
            rect.h(),
            Some(screen),
            rect.l,
            rect.t,
            ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0),
        );
        let _ = ReleaseDC(None, screen);
        let _ = GdiFlush();
        if let Err(error) = result {
            diag::win32_error("BitBlt", &error, "shot_capture");
            return None;
        }
    }
    Some(surface)
}

fn capture_and_deliver(requested: RectI) {
    let Some(rect) = requested.intersect(virtual_screen()) else {
        return;
    };
    // The overlays are gone; let DWM compose the desktop without them before reading it.
    unsafe {
        let _ = DwmFlush();
        let _ = DwmFlush();
        Sleep(30);
    }
    let Some(mut shot) = capture(rect) else {
        diag::info("screenshot_failed", &[("stage", "capture")]);
        return;
    };
    // BitBlt leaves alpha at zero; PNG and DIB consumers need it opaque.
    for pixel in shot.pixels_mut() {
        *pixel |= 0xFF00_0000;
    }
    let path = if save_to_desktop() {
        save_png(&mut shot)
    } else {
        None
    };
    let copied = copy_to_clipboard(&mut shot);
    diag::info(
        "screenshot",
        &[
            ("saved", if path.is_some() { "yes" } else { "no" }),
            ("clipboard", if copied { "yes" } else { "no" }),
        ],
    );
    show_thumb(&mut shot, path, copied, rect);
}

fn desktop_dir() -> Option<PathBuf> {
    // SAFETY: the returned buffer is copied and freed with CoTaskMemFree.
    unsafe {
        if let Ok(path) = SHGetKnownFolderPath(&FOLDERID_Desktop, KNOWN_FOLDER_FLAG(0), None) {
            let text = path.to_string();
            CoTaskMemFree(Some(path.0 as *const c_void));
            if let Ok(text) = text {
                return Some(PathBuf::from(text));
            }
        }
    }
    std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join("Desktop"))
}

fn unique_path(dir: &Path) -> PathBuf {
    let now = unsafe { GetLocalTime() };
    let stamp = format!(
        "Screenshot {:04}-{:02}-{:02} at {:02}.{:02}.{:02}",
        now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond
    );
    let mut path = dir.join(format!("{stamp}.png"));
    let mut n = 2;
    while path.exists() && n < 1000 {
        path = dir.join(format!("{stamp} ({n}).png"));
        n += 1;
    }
    path
}

fn save_png(shot: &mut Surface) -> Option<PathBuf> {
    let Some(dir) = desktop_dir() else {
        diag::info("screenshot_failed", &[("stage", "desktop_folder")]);
        return None;
    };
    let path = unique_path(&dir);
    match encode_png(shot, &path) {
        Ok(()) => Some(path),
        Err(error) => {
            diag::win32_error("WicEncodePng", &error, "shot_save");
            let _ = fs::remove_file(&path);
            None
        }
    }
}

fn encode_png(shot: &mut Surface, path: &Path) -> Result<(), Error> {
    let (width, height) = (shot.width as u32, shot.height as u32);
    let pixels = shot.pixels_mut();
    // SAFETY: a u32 slice viewed as its own bytes (BGRA in memory order on little endian).
    let bytes =
        unsafe { std::slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), pixels.len() * 4) };
    let name: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: COM is initialised on this thread; every interface is released on drop and
    // `name` / `bytes` outlive the calls.
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(
            &CLSID_WICImagingFactory,
            None::<&IUnknown>,
            CLSCTX_INPROC_SERVER,
        )?;
        let stream = factory.CreateStream()?;
        stream.InitializeFromFilename(PCWSTR(name.as_ptr()), 0x4000_0000)?; // GENERIC_WRITE
        let istream: IStream = stream.cast()?;
        let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        encoder.Initialize(&istream, WICBitmapEncoderNoCache)?;
        let mut frame: Option<IWICBitmapFrameEncode> = None;
        encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.ok_or_else(|| Error::from(E_FAIL))?;
        frame.Initialize(None::<&IPropertyBag2>)?;
        frame.SetSize(width, height)?;
        let mut format = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut format)?;
        frame.WritePixels(height, width * 4, bytes)?;
        frame.Commit()?;
        encoder.Commit()?;
    }
    Ok(())
}

fn open_clipboard(owner: HWND) -> bool {
    for _ in 0..10 {
        if unsafe { OpenClipboard(Some(owner)) }.is_ok() {
            return true;
        }
        unsafe { Sleep(20) };
    }
    false
}

fn copy_to_clipboard(shot: &mut Surface) -> bool {
    let (width, height) = (shot.width, shot.height);
    let pixels: &[u32] = shot.pixels_mut();
    let owner = hwnd_from_key(OWNER.get());
    if !open_clipboard(owner) {
        diag::info("clipboard_failed", &[("stage", "open")]);
        return false;
    }
    let header_len = size_of::<BITMAPINFOHEADER>();
    // SAFETY: the clipboard is open; the HGLOBAL is filled while locked and either handed to
    // the system by SetClipboardData (which then owns it) or freed here.
    let copied = unsafe {
        'fill: {
            if let Err(error) = EmptyClipboard() {
                diag::win32_error("EmptyClipboard", &error, "shot_clipboard");
                break 'fill false;
            }
            let Ok(memory) = GlobalAlloc(GMEM_MOVEABLE, header_len + width * height * 4) else {
                diag::last_error("GlobalAlloc", "shot_clipboard");
                break 'fill false;
            };
            let destination = GlobalLock(memory).cast::<u8>();
            if destination.is_null() {
                let _ = GlobalFree(Some(memory));
                break 'fill false;
            }
            let header = BITMAPINFOHEADER {
                biSize: header_len as u32,
                biWidth: width as i32,
                biHeight: height as i32, // positive: bottom-up rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: (width * height * 4) as u32,
                ..Default::default()
            };
            std::ptr::copy_nonoverlapping(
                (&header as *const BITMAPINFOHEADER).cast::<u8>(),
                destination,
                header_len,
            );
            let body = destination.add(header_len).cast::<u32>();
            for row in 0..height {
                let source = &pixels[(height - 1 - row) * width..(height - row) * width];
                std::ptr::copy_nonoverlapping(source.as_ptr(), body.add(row * width), width);
            }
            let _ = GlobalUnlock(memory);
            match SetClipboardData(CF_DIB, Some(HANDLE(memory.0))) {
                Ok(_) => true,
                Err(error) => {
                    diag::win32_error("SetClipboardData", &error, "shot_clipboard");
                    let _ = GlobalFree(Some(memory));
                    false
                }
            }
        }
    };
    let _ = unsafe { CloseClipboard() };
    copied
}

// ---------------------------------------------------------------- thumbnail card

/// Box-filters `source` (width, height, pixels) into the `target` rectangle (x, y, w, h) of
/// the canvas.
fn scale_into(
    canvas: &mut Canvas,
    source: (&[u32], usize, usize),
    target: (usize, usize, usize, usize),
) {
    let (pixels, source_w, source_h) = source;
    let (x0, y0, target_w, target_h) = target;
    const SAMPLES: usize = 3;
    for y in 0..target_h {
        for x in 0..target_w {
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let fx = (x as f32 + (sx as f32 + 0.5) / SAMPLES as f32) * source_w as f32
                        / target_w as f32;
                    let fy = (y as f32 + (sy as f32 + 0.5) / SAMPLES as f32) * source_h as f32
                        / target_h as f32;
                    let px = pixels[(fy as usize).min(source_h - 1) * source_w
                        + (fx as usize).min(source_w - 1)];
                    r += (px >> 16) & 0xFF;
                    g += (px >> 8) & 0xFF;
                    b += px & 0xFF;
                }
            }
            let n = (SAMPLES * SAMPLES) as u32;
            canvas.pixels[(y0 + y) * canvas.width + x0 + x] =
                0xFF00_0000 | ((r / n) << 16) | ((g / n) << 8) | (b / n);
        }
    }
}

fn show_thumb(shot: &mut Surface, path: Option<PathBuf>, copied: bool, rect: RectI) {
    drop(take_thumb());
    let caption = match (path.is_some(), copied) {
        (true, true) => "Saved to Desktop and copied",
        (true, false) => "Saved to Desktop",
        (false, true) => "Copied to clipboard",
        (false, false) => return,
    };
    let Some(instance) = module_instance() else {
        return;
    };
    let geo = monitor_geo(((rect.l + rect.r) / 2, (rect.t + rect.b) / 2));
    let s = geo.scale;
    let (source_w, source_h) = (shot.width, shot.height);
    let ratio = ((220.0 * s) / source_w as f32)
        .min((140.0 * s) / source_h as f32)
        .min(1.0);
    let image_w = ((source_w as f32 * ratio).round() as usize).max(1);
    let image_h = ((source_h as f32 * ratio).round() as usize).max(1);
    let pad = (10.0 * s) as usize;
    let caption_h = (22.0 * s) as usize;
    let (card_w, card_h) = (image_w + 2 * pad, image_h + 2 * pad + caption_h);
    let mut canvas = Canvas::new(card_w, card_h);
    canvas.fill_round_rect(
        0.0,
        0.0,
        card_w as f32,
        card_h as f32,
        [10.0 * s; 4],
        0x202024,
        0.96,
    );
    let source: &[u32] = shot.pixels_mut();
    scale_into(
        &mut canvas,
        (source, source_w, source_h),
        (pad, pad, image_w, image_h),
    );
    if let Some(mask) = text(caption, (12.0 * s).round() as i32, false) {
        let y = (pad + image_h) as i32 + (caption_h as i32 - mask.height as i32) / 2;
        canvas.draw_mask(&mask, pad as i32, y, 0xFFFFFF, 1.0);
    }
    let margin = (20.0 * s) as i32;
    let x = geo.work.r - card_w as i32 - margin;
    let y = geo.work.b - card_h as i32 - margin;
    let window = match OwnedWindow::create(
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        THUMB_CLASS,
        &wide("Pulse Screenshot Thumbnail"),
        WS_POPUP,
        (x, y, card_w as i32, card_h as i32),
        instance,
    ) {
        Ok(window) => window,
        Err(error) => {
            diag::win32_error("CreateWindowExW", &error, "shot_thumb");
            return;
        }
    };
    let hwnd = window.hwnd();
    if let Err(error) = present(hwnd, &canvas, Some((x, y))) {
        diag::win32_error("UpdateLayeredWindow", &error, "shot_thumb");
        return;
    }
    let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
    let _ = unsafe { SetTimer(Some(hwnd), THUMB_TIMER, THUMB_MS, None) };
    THUMB.with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut() {
            *guard = Some(Thumb { window, path });
        }
    });
}

fn open_thumb() {
    let thumb = take_thumb();
    if let Some(path) = thumb.as_ref().and_then(|t| t.path.as_ref()) {
        let name: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `name` is NUL-terminated and outlives the call.
        let _ = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                PCWSTR(name.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
    }
    drop(thumb); // destroys the card
}

// ---------------------------------------------------------------- window procedures

extern "system" fn overlay_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match message {
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
            WM_ERASEBKGND => return LRESULT(1),
            WM_PAINT => {
                let _ = ValidateRect(Some(hwnd), None);
                return LRESULT(0);
            }
            WM_SETCURSOR => {
                set_pick_cursor();
                return LRESULT(1);
            }
            WM_LBUTTONDOWN => {
                let _ = SetCapture(hwnd);
                pick_down();
                return LRESULT(0);
            }
            WM_MOUSEMOVE => {
                pick_move();
                return LRESULT(0);
            }
            WM_LBUTTONUP => {
                pick_up();
                return LRESULT(0);
            }
            WM_CAPTURECHANGED => {
                pick_capture_lost();
                return LRESULT(0);
            }
            WM_RBUTTONUP => {
                queue(Finish::Cancel);
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

fn lparam_point(lparam: LPARAM) -> (i32, i32) {
    (
        (lparam.0 & 0xFFFF) as i16 as i32,
        ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
    )
}

extern "system" fn toolbar_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match message {
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
            WM_ERASEBKGND => return LRESULT(1),
            WM_PAINT => {
                let _ = ValidateRect(Some(hwnd), None);
                return LRESULT(0);
            }
            WM_MOUSEMOVE => {
                let (x, y) = lparam_point(lparam);
                toolbar_hover(x, y);
                return LRESULT(0);
            }
            WM_LBUTTONUP => {
                let (x, y) = lparam_point(lparam);
                toolbar_click(x, y);
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

extern "system" fn thumb_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match message {
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
            WM_ERASEBKGND => return LRESULT(1),
            WM_PAINT => {
                let _ = ValidateRect(Some(hwnd), None);
                return LRESULT(0);
            }
            WM_TIMER if wparam.0 == THUMB_TIMER => {
                drop(take_thumb());
                return LRESULT(0);
            }
            WM_LBUTTONUP => {
                open_thumb();
                return LRESULT(0);
            }
            WM_RBUTTONUP => {
                drop(take_thumb());
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

extern "system" fn plain_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        // SAFETY: default processing of the unchanged message.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

// ---------------------------------------------------------------- thread

pub struct ShotThread {
    thread_id: u32,
    join: Option<JoinHandle<()>>,
}

impl Drop for ShotThread {
    fn drop(&mut self) {
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Starts the screenshot thread. None when it could not be set up (reported).
pub fn start() -> Option<ShotThread> {
    let (tx, rx) = mpsc::channel::<Option<u32>>();
    let join = thread::Builder::new()
        .name("pulse-shot".to_string())
        .spawn(move || thread_main(&tx))
        .ok()?;
    match rx.recv() {
        Ok(Some(thread_id)) => Some(ShotThread {
            thread_id,
            join: Some(join),
        }),
        _ => {
            let _ = join.join();
            None
        }
    }
}

fn register_classes(instance: HINSTANCE) -> Result<[ClassGuard; 4], Error> {
    Ok([
        ClassGuard::register(OVERLAY_CLASS, Some(overlay_proc), instance)?,
        ClassGuard::register(TOOLBAR_CLASS, Some(toolbar_proc), instance)?,
        ClassGuard::register(THUMB_CLASS, Some(thumb_proc), instance)?,
        ClassGuard::register(PLAIN_CLASS, Some(plain_proc), instance)?,
    ])
}

fn thread_main(ready: &mpsc::Sender<Option<u32>>) {
    // Declared first, dropped last: COM stays up until every window and class is gone.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    run_loop(ready);
    if com {
        unsafe { CoUninitialize() };
    }
}

fn run_loop(ready: &mpsc::Sender<Option<u32>>) {
    let Some(instance) = module_instance() else {
        let _ = ready.send(None);
        return;
    };
    let _classes = match register_classes(instance) {
        Ok(classes) => classes,
        Err(error) => {
            diag::win32_error("RegisterClassW", &error, "shot_classes");
            let _ = ready.send(None);
            return;
        }
    };
    let Some(painter) = TextPainter::new() else {
        let _ = ready.send(None);
        return;
    };
    PAINTER.with(|cell| *cell.borrow_mut() = Some(painter));
    // Hidden owner for the clipboard (OpenClipboard needs a real window handle).
    let owner = match OwnedWindow::create(
        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        PLAIN_CLASS,
        &wide("Pulse Screenshot Owner"),
        WS_POPUP,
        (0, 0, 0, 0),
        instance,
    ) {
        Ok(owner) => owner,
        Err(error) => {
            diag::win32_error("CreateWindowExW", &error, "shot_owner");
            let _ = ready.send(None);
            return;
        }
    };
    OWNER.set(owner.key());

    let mut message = MSG::default();
    // Creates this thread's message queue before the id is published.
    let _ = unsafe { PeekMessageW(&mut message, None, WM_USER, WM_USER, PM_NOREMOVE) };
    THREAD_ID.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
    let _ = ready.send(Some(unsafe { GetCurrentThreadId() }));

    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        if message.hwnd.0.is_null() {
            on_thread_message(&message);
        } else {
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    THREAD_ID.store(0, Ordering::Relaxed);
    ACTIVE.store(false, Ordering::Relaxed);
    drop(take_session());
    drop(take_thumb());
    PAINTER.with(|cell| drop(cell.borrow_mut().take()));
    OWNER.set(0);
    drop(owner);
}
