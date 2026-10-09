//! GDI plumbing for the software-rendered notch: a top-down 32-bit DIB section that is
//! handed to `UpdateLayeredWindow` (per-pixel alpha, no activation), and a GDI text
//! rasteriser that turns a string into a grey coverage mask for `Canvas::draw_mask`.
//! Every GDI object is released on drop on every path.

use crate::canvas::{Canvas, Mask};
use crate::diag;
use crate::raii::{GdiObject, SelectScope};
use std::ffi::c_void;
use std::mem::size_of;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC,
    CreateDIBSection, CreateFontIndirectW, DEFAULT_CHARSET, DIB_RGB_COLORS, DeleteDC, DeleteObject,
    GdiFlush, GetDC, GetTextExtentPoint32W, HBITMAP, HDC, HFONT, HGDIOBJ, LOGFONTW, ReleaseDC,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT, TextOutW,
};
use windows::Win32::UI::WindowsAndMessaging::{ULW_ALPHA, UpdateLayeredWindow};
use windows::core::Error;

const MAX_SURFACE_SIDE: usize = 8192;
const FONT_FACE: &str = "Segoe UI";

/// A 32-bit top-down DIB section selected into a memory DC.
pub struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut c_void,
    pub width: usize,
    pub height: usize,
}

impl Surface {
    pub fn new(width: usize, height: usize) -> Option<Self> {
        if width == 0 || height == 0 || width > MAX_SURFACE_SIDE || height > MAX_SURFACE_SIDE {
            return None;
        }
        // SAFETY: plain GDI object creation; every created object is either stored in the
        // returned `Surface` (released by Drop) or released on the failing path below.
        unsafe {
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            let _ = ReleaseDC(None, screen);
            if dc.0.is_null() {
                diag::last_error("CreateCompatibleDC", "surface");
                return None;
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32), // negative: top-down rows
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            {
                Ok(bitmap) if !bits.is_null() => bitmap,
                Ok(bitmap) => {
                    let _ = DeleteObject(bitmap.into());
                    let _ = DeleteDC(dc);
                    return None;
                }
                Err(error) => {
                    diag::win32_error("CreateDIBSection", &error, "surface");
                    let _ = DeleteDC(dc);
                    return None;
                }
            };
            let previous = SelectObject(dc, bitmap.into());
            Some(Self {
                dc,
                bitmap,
                previous,
                bits,
                width,
                height,
            })
        }
    }

    pub fn dc(&self) -> HDC {
        self.dc
    }

    /// Pixel memory (`0xAARRGGBB`, row-major). GDI drawing must be flushed first.
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        // SAFETY: `bits` points at `width * height` 32-bit pixels owned by the DIB section,
        // which lives as long as `self`; the `&mut self` borrow keeps access exclusive.
        unsafe { std::slice::from_raw_parts_mut(self.bits.cast::<u32>(), self.width * self.height) }
    }

    pub fn copy_from(&mut self, canvas: &Canvas) {
        if canvas.width == self.width && canvas.height == self.height {
            self.pixels_mut().copy_from_slice(&canvas.pixels);
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: restores the DC's original bitmap before deleting what we created.
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Publishes `canvas` as the window's contents with per-pixel alpha. `position` moves the
/// window to a new top-left in the same call; the window size becomes the canvas size.
pub fn present(hwnd: HWND, canvas: &Canvas, position: Option<(i32, i32)>) -> Result<(), Error> {
    let mut surface = Surface::new(canvas.width, canvas.height)
        .ok_or_else(|| Error::from(windows::Win32::Foundation::E_FAIL))?;
    surface.copy_from(canvas);
    let size = SIZE {
        cx: canvas.width as i32,
        cy: canvas.height as i32,
    };
    let source = POINT { x: 0, y: 0 };
    let destination = position.map(|(x, y)| POINT { x, y });
    let destination_ptr: Option<*const POINT> = destination.as_ref().map(|p| p as *const POINT);
    let blend = BLENDFUNCTION {
        BlendOp: 0, // AC_SRC_OVER
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: 1, // AC_SRC_ALPHA: the bitmap carries premultiplied alpha
    };
    // SAFETY: all pointers refer to locals that outlive the synchronous call; the source DC
    // holds the DIB just filled.
    unsafe {
        UpdateLayeredWindow(
            hwnd,
            None,
            destination_ptr,
            Some(&size),
            Some(surface.dc()),
            Some(&source),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )
    }
}

/// Renders strings with GDI (grey anti-aliasing, no ClearType) into coverage masks.
pub struct TextPainter {
    scratch: Surface,
    fonts: Vec<(i32, bool, GdiObject<HFONT>)>,
}

impl TextPainter {
    pub fn new() -> Option<Self> {
        Some(Self {
            scratch: Surface::new(1024, 96)?,
            fonts: Vec::new(),
        })
    }

    fn font(&mut self, size_px: i32, bold: bool) -> Option<HFONT> {
        if let Some((_, _, font)) = self
            .fonts
            .iter()
            .find(|(size, weight, _)| *size == size_px && *weight == bold)
        {
            return Some(font.get());
        }
        let mut logical = LOGFONTW {
            lfHeight: -size_px.max(1),
            lfWeight: if bold { 700 } else { 400 },
            lfCharSet: DEFAULT_CHARSET,
            lfQuality: ANTIALIASED_QUALITY,
            ..Default::default()
        };
        for (slot, unit) in logical.lfFaceName.iter_mut().zip(FONT_FACE.encode_utf16()) {
            *slot = unit;
        }
        // SAFETY: `logical` is a fully initialised LOGFONTW (face name NUL-terminated by the
        // zeroed default) that outlives the call.
        let font = GdiObject::new(
            unsafe { CreateFontIndirectW(&logical) },
            "CreateFontIndirectW",
        )?;
        let handle = font.get();
        self.fonts.push((size_px, bold, font));
        Some(handle)
    }

    /// Coverage mask for `text` at `size_px` (pixel height of the font cell).
    pub fn render(&mut self, text: &str, size_px: i32, bold: bool) -> Option<Mask> {
        if text.is_empty() {
            return Some(Mask {
                width: 0,
                height: 0,
                coverage: Vec::new(),
            });
        }
        let font = self.font(size_px, bold)?;
        let wide: Vec<u16> = text.encode_utf16().collect();
        let dc = self.scratch.dc();
        let _selected = SelectScope::select(dc, font.into(), "SelectObject")?;
        let mut extent = SIZE::default();
        // SAFETY: `dc` is the scratch memory DC with the font selected; `wide` and `extent`
        // outlive each call.
        unsafe {
            if !GetTextExtentPoint32W(dc, &wide, &mut extent).as_bool() {
                diag::last_error("GetTextExtentPoint32W", "text");
                return None;
            }
        }
        let width = (extent.cx.max(0) as usize + 1).min(self.scratch.width);
        let height = (extent.cy.max(0) as usize).min(self.scratch.height);
        if width == 0 || height == 0 {
            return None;
        }
        self.scratch.pixels_mut().fill(0);
        // SAFETY: as above; the white-on-black text is read back after GdiFlush.
        unsafe {
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x00FF_FFFF));
            if !TextOutW(dc, 0, 0, &wide).as_bool() {
                diag::last_error("TextOutW", "text");
                return None;
            }
            let _ = GdiFlush();
        }
        let stride = self.scratch.width;
        let pixels = self.scratch.pixels_mut();
        let mut coverage = Vec::with_capacity(width * height);
        for row in 0..height {
            for column in 0..width {
                coverage.push((pixels[row * stride + column] & 0xFF) as u8);
            }
        }
        Some(Mask {
            width,
            height,
            coverage,
        })
    }
}
