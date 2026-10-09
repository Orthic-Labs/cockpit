//! The shell's icon for a file (an installer, the app itself) as a premultiplied image the
//! card renderer can draw. Live cards only: the view renders never ask for one. Icons are
//! cached by path so a card that redraws does not ask the shell again.

use crate::surface::Surface;
use std::collections::HashMap;
use std::mem::size_of;
use std::sync::{Arc, Mutex, PoisonError};
use windows::Win32::Graphics::Gdi::GdiFlush;
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGetFileInfoW};
use windows::Win32::UI::WindowsAndMessaging::{DI_NORMAL, DestroyIcon, DrawIconEx};
use windows::core::PCWSTR;

/// Side of the large shell icon, in pixels.
const SIDE: usize = 32;
/// Icons kept; the cache is dropped when it grows past this.
const KEPT: usize = 24;

pub struct Icon {
    pub width: usize,
    pub height: usize,
    /// Premultiplied `0xAARRGGBB`, row-major.
    pub pixels: Vec<u32>,
}

type Cache = HashMap<String, Option<Arc<Icon>>>;

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

/// The icon of the file at `path`, or `None` when the shell has none.
pub fn of(path: &str) -> Option<Arc<Icon>> {
    if path.is_empty() {
        return None;
    }
    let mut guard = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some(found) = cache.get(path) {
        return found.clone();
    }
    if cache.len() >= KEPT {
        cache.clear();
    }
    let icon = load(path).map(Arc::new);
    cache.insert(path.to_string(), icon.clone());
    icon
}

fn load(path: &str) -> Option<Icon> {
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut info = SHFILEINFOW::default();
    // SAFETY: `wide` is NUL-terminated and `info` is a zeroed SHFILEINFOW; both outlive the
    // call, and the icon handle it returns is destroyed below.
    let found = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON,
        )
    };
    if found == 0 || info.hIcon.is_invalid() {
        return None;
    }
    let mut surface = Surface::new(SIDE, SIDE)?;
    // SAFETY: the surface's DC is live for the call; the icon handle is valid until the
    // DestroyIcon that follows.
    let drawn = unsafe {
        let drawn = DrawIconEx(
            surface.dc(),
            0,
            0,
            info.hIcon,
            SIDE as i32,
            SIDE as i32,
            0,
            None,
            DI_NORMAL,
        );
        let _ = GdiFlush();
        let _ = DestroyIcon(info.hIcon);
        drawn
    };
    drawn.ok()?;
    let mut pixels = surface.pixels_mut().to_vec();
    // An icon without an alpha channel leaves every alpha at zero: what it drew is opaque.
    if pixels.iter().all(|p| p >> 24 == 0) {
        for pixel in &mut pixels {
            if *pixel & 0x00FF_FFFF != 0 {
                *pixel |= 0xFF00_0000;
            }
        }
    }
    Some(Icon {
        width: SIDE,
        height: SIDE,
        pixels,
    })
}
