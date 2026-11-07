//! A ~100-line anti-aliased painter for the dark settings window.
//!
//! Why not just theme the stock controls: Windows has no supported dark theme
//! for classic BUTTON/LISTBOX controls. Themed checkboxes ignore
//! WM_CTLCOLORSTATIC's text colour and draw black text on our black window.
//! And plain GDI shapes (RoundRect, Ellipse) are aliased, so pill switches look
//! like stair steps. So each control paints itself into an off-screen 32-bpp DIB
//! with signed-distance-field coverage (same trick as the tray badge), then
//! does one BitBlt. Double-buffered for free, with no GDI+ or Direct2D load.

use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{COLORREF, RECT};
use windows_sys::Win32::Graphics::Gdi::*;

// Palette, as 0xRRGGBB (the DIB's native pixel layout).
pub const BG: u32 = 0x111111;
pub const SURFACE: u32 = 0x1C1C1C;
pub const SURFACE_HI: u32 = 0x2A2A2A;
pub const BORDER: u32 = 0x2C2C2C;
pub const TEXT: u32 = 0xEDEDED;
pub const MUTED: u32 = 0x8B8B8B;
pub const FAINT: u32 = 0x555555;
pub const TRACK_OFF: u32 = 0x3A3A3A;
pub const KNOB: u32 = 0xF5F5F5;
pub const ACCENT: u32 = 0x3ECF8E;
pub const RED: u32 = 0xE5484D; // same red as the tray's paused badge

/// GDI wants COLORREF (0x00BBGGRR), the reverse byte order of the DIB pixels.
pub const fn cref(c: u32) -> COLORREF {
    ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF)
}

pub struct Canvas {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    px: *mut u32,
    pub w: i32,
    pub h: i32,
}

impl Canvas {
    pub fn new(like: HDC, w: i32, h: i32, bg: u32) -> Option<Canvas> {
        let (w, h) = (w.max(1), h.max(1));
        unsafe {
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = w;
            bi.bmiHeader.biHeight = -h; // negative = top-down rows, so index = y*w + x
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            let mut bits = null_mut();
            let bmp = CreateDIBSection(like, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bmp.is_null() {
                return None;
            }
            let dc = CreateCompatibleDC(like);
            let old = SelectObject(dc, bmp);
            let mut c = Canvas { dc, bmp, old, px: bits.cast(), w, h };
            c.pixels().fill(bg);
            Some(c)
        }
    }

    fn pixels(&mut self) -> &mut [u32] {
        // The DIB section owns w*h u32s for as long as `bmp` lives (until Drop).
        unsafe { std::slice::from_raw_parts_mut(self.px, (self.w * self.h) as usize) }
    }

    /// Filled rounded rectangle. r = h/2 gives a pill, and w = h = 2r a circle.
    pub fn fill(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: u32) {
        self.shape(x, y, w, h, r, None, color);
    }

    /// Rounded-rectangle outline of width `sw`, drawn *inside* the edge.
    #[allow(clippy::too_many_arguments)] // x,y,w,h,r flat on purpose: call sites read like CSS
    pub fn stroke(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, sw: f32, color: u32) {
        self.shape(x, y, w, h, r, Some(sw), color);
    }

    #[allow(clippy::too_many_arguments)]
    fn shape(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, stroke: Option<f32>, color: u32) {
        // GDI batches calls: flush pending text so our writes don't race it.
        unsafe { GdiFlush() };
        let (cw, ch) = (self.w, self.h);
        let (x0, y0) = ((x.floor() as i32).max(0), (y.floor() as i32).max(0));
        let (x1, y1) = (((x + w).ceil() as i32).min(cw), ((y + h).ceil() as i32).min(ch));
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let (hx, hy) = (w / 2.0 - r, h / 2.0 - r);
        let px = self.pixels();
        for py in y0..y1 {
            for pxi in x0..x1 {
                // Rounded-box SDF (Inigo Quilez), sampled at the pixel centre.
                let qx = (pxi as f32 + 0.5 - cx).abs() - hx;
                let qy = (py as f32 + 0.5 - cy).abs() - hy;
                let d = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r;
                // About one pixel of falloff across the edge: that's the anti-aliasing.
                let cov = match stroke {
                    None => 0.5 - d,
                    Some(sw) => sw / 2.0 + 0.5 - (d + sw / 2.0).abs(),
                }
                .clamp(0.0, 1.0);
                if cov > 0.0 {
                    let p = &mut px[(py * cw + pxi) as usize];
                    *p = mix(*p, color, cov);
                }
            }
        }
    }

    /// GDI text (keeps ClearType and the user's UI font) on top of the shapes.
    pub fn text(&mut self, s: &str, r: RECT, color: u32, font: HFONT, flags: u32) -> RECT {
        let wide: Vec<u16> = s.encode_utf16().collect();
        let mut r = r;
        unsafe {
            SelectObject(self.dc, font);
            SetBkMode(self.dc, TRANSPARENT as i32);
            SetTextColor(self.dc, cref(color));
            DrawTextW(self.dc, wide.as_ptr(), wide.len() as i32, &mut r, flags | DT_NOPREFIX);
        }
        r
    }

    pub fn blit(&self, dst: HDC, x: i32, y: i32) {
        unsafe { BitBlt(dst, x, y, self.w, self.h, self.dc, 0, 0, SRCCOPY) };
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old); // deselect first: you can't delete a selected bitmap
            DeleteDC(self.dc);
            DeleteObject(self.bmp);
        }
    }
}

/// Linear blend of two 0xRRGGBB colours, a = 0 → `d`, a = 1 → `s`.
fn mix(d: u32, s: u32, a: f32) -> u32 {
    let ch = |sh: u32| {
        let (dc, sc) = (((d >> sh) & 0xFF) as f32, ((s >> sh) & 0xFF) as f32);
        ((dc + (sc - dc) * a + 0.5) as u32) << sh
    };
    ch(16) | ch(8) | ch(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn colour_helpers() {
        assert_eq!(super::cref(0x112233), 0x332211);
        assert_eq!(super::mix(0x000000, 0xFFFFFF, 0.5), 0x808080);
        assert_eq!(super::mix(0x123456, 0xABCDEF, 0.0), 0x123456);
    }
}
