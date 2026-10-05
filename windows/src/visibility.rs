//! Pure fullscreen-occupancy policy. Production (`enum_visible_window` in main.rs) and
//! tests both call these helpers; there is no test-local copy of any rule.
//!
//! Policy (conservative): the topmost visible, non-owned, non-tool, non-cloaked, non-shell
//! window that overlaps the pill's monitor decides. It hides the pill only if it covers the
//! whole monitor AND has no caption and no resize frame.

use windows::Win32::Foundation::RECT;
use windows::Win32::UI::WindowsAndMessaging::{WS_CAPTION, WS_EX_TOOLWINDOW, WS_THICKFRAME};

/// Rects must share one coordinate space: the process is made per-monitor DPI aware (V2)
/// at startup so window and monitor rects are both physical virtual-screen pixels.
pub fn windows_overlap(window: RECT, monitor: RECT) -> bool {
    window.right > monitor.left
        && window.left < monitor.right
        && window.bottom > monitor.top
        && window.top < monitor.bottom
}

pub fn is_fullscreen_geometry(window: RECT, monitor: RECT) -> bool {
    window.left <= monitor.left
        && window.top <= monitor.top
        && window.right >= monitor.right
        && window.bottom >= monitor.bottom
}

/// WS_CAPTION is WS_BORDER | WS_DLGFRAME. Only the full combination is a title bar; a lone
/// WS_BORDER or WS_DLGFRAME is not a caption. A resize frame (WS_THICKFRAME) always blocks.
pub fn is_borderless_style(style: u32) -> bool {
    let has_caption = style & WS_CAPTION.0 == WS_CAPTION.0;
    let has_resize_frame = style & WS_THICKFRAME.0 != 0;
    !has_caption && !has_resize_frame
}

pub fn is_tool_window_ex_style(ex_style: u32) -> bool {
    ex_style & WS_EX_TOOLWINDOW.0 != 0
}

pub fn is_shell_class_name(name: &str) -> bool {
    matches!(
        name,
        "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Occupancy {
    /// Does not overlap the monitor: keep scanning lower windows.
    Outside,
    /// Topmost overlapping window is a borderless full-monitor cover: hide the pill.
    Covers,
    /// Topmost overlapping window is not a cover: it decides, pill stays visible.
    Partial,
}

pub fn classify_window(window: RECT, monitor: RECT, style: u32) -> Occupancy {
    if !windows_overlap(window, monitor) {
        Occupancy::Outside
    } else if is_fullscreen_geometry(window, monitor) && is_borderless_style(style) {
        Occupancy::Covers
    } else {
        Occupancy::Partial
    }
}
