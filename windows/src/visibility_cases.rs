//! Geometry/style cases for pill auto-hide. Included from main.rs as
//! `#[cfg(all(test, windows))] mod visibility_cases;` (parent has `use windows::...::*`).
//! Pure functions only: `is_fullscreen_geometry`, `is_borderless_style`, `monitor_id`.
//! The overlap pre-filter inside `enum_visible_window` is inline and is mirrored by
//! `overlaps` below so its documented intent is exercised.

use super::*;

fn r(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
    RECT {
        left,
        top,
        right,
        bottom,
    }
}

const PRIMARY: RECT = RECT {
    left: 0,
    top: 0,
    right: 1920,
    bottom: 1080,
};
const RIGHT_MON: RECT = RECT {
    left: 1920,
    top: 0,
    right: 3840,
    bottom: 1080,
};
const LEFT_MON: RECT = RECT {
    left: -1920,
    top: 0,
    right: 0,
    bottom: 1080,
};
const ABOVE_MON: RECT = RECT {
    left: 0,
    top: -1080,
    right: 1920,
    bottom: 0,
};

fn fullscreen_borderless(w: RECT, m: RECT, style: u32) -> bool {
    is_fullscreen_geometry(w, m) && is_borderless_style(style)
}

// ---- secondary monitors ----
#[test]
fn secondary_right_monitor_exact_bounds() {
    assert!(is_fullscreen_geometry(RIGHT_MON, RIGHT_MON));
}
#[test]
fn primary_sized_window_on_secondary_is_not_fullscreen() {
    assert!(!is_fullscreen_geometry(
        r(1920, 0, 3840 - 1, 1080),
        RIGHT_MON
    ));
    assert!(!is_fullscreen_geometry(PRIMARY, RIGHT_MON));
}
#[test]
fn fullscreen_on_primary_does_not_cover_secondary() {
    assert!(!is_fullscreen_geometry(PRIMARY, RIGHT_MON));
    assert!(
        !windows_overlap(PRIMARY, RIGHT_MON),
        "edge-adjacent monitors must not count as overlap"
    );
}

// ---- negative coordinates ----
#[test]
fn negative_origin_monitor_exact_bounds() {
    assert!(is_fullscreen_geometry(LEFT_MON, LEFT_MON));
}
#[test]
fn negative_monitor_inset_by_one_is_not_fullscreen() {
    assert!(!is_fullscreen_geometry(r(-1919, 0, 0, 1080), LEFT_MON));
    assert!(!is_fullscreen_geometry(r(-1920, 1, 0, 1080), LEFT_MON));
    assert!(!is_fullscreen_geometry(r(-1920, 0, -1, 1080), LEFT_MON));
    assert!(!is_fullscreen_geometry(r(-1920, 0, 0, 1079), LEFT_MON));
}
#[test]
fn monitor_above_primary_negative_top() {
    assert!(is_fullscreen_geometry(ABOVE_MON, ABOVE_MON));
    assert!(!is_fullscreen_geometry(PRIMARY, ABOVE_MON));
    assert!(!windows_overlap(PRIMARY, ABOVE_MON));
}
#[test]
fn oversized_window_with_negative_overhang_is_fullscreen() {
    assert!(is_fullscreen_geometry(r(-1925, -5, 5, 1085), LEFT_MON));
}

// ---- partial coverage ----
#[test]
fn half_screen_snap_is_not_fullscreen() {
    assert!(!is_fullscreen_geometry(r(0, 0, 960, 1080), PRIMARY));
    assert!(!is_fullscreen_geometry(r(960, 0, 1920, 1080), PRIMARY));
}
#[test]
fn taskbar_height_shortfall_is_not_fullscreen() {
    assert!(!is_fullscreen_geometry(r(0, 0, 1920, 1040), PRIMARY));
}
#[test]
fn partial_window_still_overlaps_for_enumeration() {
    assert!(windows_overlap(r(1900, 1000, 2100, 1200), PRIMARY));
    assert!(windows_overlap(r(1900, 1000, 2100, 1200), RIGHT_MON));
}
#[test]
fn degenerate_zero_size_rect_is_not_fullscreen() {
    assert!(!is_fullscreen_geometry(r(0, 0, 0, 0), PRIMARY));
}

// ---- windows spanning monitors ----
#[test]
fn spanning_borderless_window_covers_both_monitors() {
    let span = r(0, 0, 3840, 1080);
    let style = WS_POPUP.0 | WS_VISIBLE.0;
    assert!(fullscreen_borderless(span, PRIMARY, style));
    assert!(fullscreen_borderless(span, RIGHT_MON, style));
}
#[test]
fn spanning_window_covers_only_monitors_fully_inside() {
    let span = r(960, 0, 2880, 1080);
    assert!(windows_overlap(span, PRIMARY) && windows_overlap(span, RIGHT_MON));
    assert!(!is_fullscreen_geometry(span, PRIMARY));
    assert!(!is_fullscreen_geometry(span, RIGHT_MON));
}
#[test]
fn spanning_left_and_primary_negative_coordinates() {
    let span = r(-1920, 0, 1920, 1080);
    assert!(is_fullscreen_geometry(span, LEFT_MON));
    assert!(is_fullscreen_geometry(span, PRIMARY));
    assert!(!is_fullscreen_geometry(span, RIGHT_MON));
    assert!(!windows_overlap(span, RIGHT_MON));
}
#[test]
fn spanning_window_taller_monitor_not_covered() {
    let tall = r(1920, 0, 3840, 1440);
    let mon = r(1920, 0, 3840, 1600);
    assert!(!is_fullscreen_geometry(tall, mon));
}

// ---- captioned maximized windows ----
#[test]
fn captioned_maximized_window_is_geometry_fullscreen_but_not_borderless() {
    // Maximized captioned windows overhang the monitor by the frame (~8px).
    let maxed = r(-8, -8, 1928, 1088);
    let style = WS_OVERLAPPEDWINDOW.0 | WS_MAXIMIZE.0 | WS_VISIBLE.0;
    assert!(is_fullscreen_geometry(maxed, PRIMARY));
    assert!(!is_borderless_style(style));
    assert!(!fullscreen_borderless(maxed, PRIMARY, style));
}
#[test]
fn captioned_maximized_on_negative_secondary_is_not_hiding() {
    let maxed = r(-1928, -8, 8, 1088);
    assert!(!fullscreen_borderless(
        maxed,
        LEFT_MON,
        WS_OVERLAPPEDWINDOW.0 | WS_MAXIMIZE.0
    ));
}
#[test]
fn caption_alone_or_thickframe_alone_blocks_hiding() {
    assert!(!is_borderless_style(WS_CAPTION.0));
    assert!(!is_borderless_style(WS_THICKFRAME.0));
    assert!(!is_borderless_style(
        WS_CAPTION.0 | WS_THICKFRAME.0 | WS_SYSMENU.0
    ));
}
#[test]
fn plain_border_bit_counts_as_captioned() {
    // WS_CAPTION = WS_BORDER | WS_DLGFRAME, so a WS_BORDER-only window is treated as captioned.
    assert!(!is_borderless_style(WS_BORDER.0));
    assert!(!is_borderless_style(WS_DLGFRAME.0));
}

// ---- borderless fullscreen ----
#[test]
fn popup_borderless_exact_monitor_hides() {
    assert!(fullscreen_borderless(
        PRIMARY,
        PRIMARY,
        WS_POPUP.0 | WS_VISIBLE.0
    ));
}
#[test]
fn borderless_fullscreen_on_secondary_and_negative_monitors() {
    let s = WS_POPUP.0 | WS_VISIBLE.0;
    assert!(fullscreen_borderless(RIGHT_MON, RIGHT_MON, s));
    assert!(fullscreen_borderless(LEFT_MON, LEFT_MON, s));
    assert!(fullscreen_borderless(ABOVE_MON, ABOVE_MON, s));
}
#[test]
fn borderless_but_smaller_than_monitor_does_not_hide() {
    assert!(!fullscreen_borderless(
        r(100, 100, 1000, 800),
        PRIMARY,
        WS_POPUP.0
    ));
}
#[test]
fn zero_style_is_borderless() {
    assert!(is_borderless_style(0));
}
#[test]
fn unrelated_style_bits_do_not_affect_borderless() {
    assert!(is_borderless_style(
        WS_POPUP.0 | WS_VISIBLE.0 | WS_CLIPCHILDREN.0 | WS_MINIMIZEBOX.0
    ));
}

// ---- monitor_id ----
#[test]
fn monitor_id_stops_at_nul() {
    let mut info = MONITORINFOEXW::default();
    for (i, c) in "\\\\.\\DISPLAY2".encode_utf16().enumerate() {
        info.szDevice[i] = c;
    }
    assert_eq!(monitor_id(&info), "\\\\.\\DISPLAY2");
}
#[test]
fn monitor_id_empty_when_unset() {
    assert_eq!(monitor_id(&MONITORINFOEXW::default()), "");
}
#[test]
fn monitor_id_full_length_without_nul() {
    let mut info = MONITORINFOEXW::default();
    info.szDevice.fill(b'A' as u16);
    assert_eq!(monitor_id(&info).len(), info.szDevice.len());
}
