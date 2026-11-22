//! A ~100-line anti-aliased painter for the dark settings window.
//!
//! Why not just theme the stock controls: Windows has no supported dark theme
//! for classic BUTTON/LISTBOX controls. Themed checkboxes ignore
//! WM_CTLCOLORSTATIC's text colour and draw black text on our black window.
//! And plain GDI shapes (RoundRect, Ellipse) are aliased, so pill switches look
//! like stair steps. So each control paints itself into an off-screen 32-bpp DIB
//! with signed-distance-field coverage (same trick as the tray badge), then
//! does one BitBlt. Double-buffered for free, with no GDI+ or Direct2D load.

use std::cell::RefCell;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{COLORREF, RECT};
use windows_sys::Win32::Graphics::Gdi::*;

// Palette, as 0xRRGGBB (the DIB's native pixel layout).
pub const BG: u32 = 0x10141A;
pub const SURFACE: u32 = 0x1A2029;
pub const SURFACE_HI: u32 = 0x2B3B44;
pub const BORDER: u32 = 0x333F4C;
pub const TEXT: u32 = 0xEEF2F6;
pub const MUTED: u32 = 0xA9B5C3;
pub const FAINT: u32 = 0x555555;
pub const TRACK_OFF: u32 = 0x465467;
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
    buf_w: i32,
    buf_h: i32,
    pub w: i32,
    pub h: i32,
}

thread_local! {
    /// Reusable off-screen DIB for the UI thread so paints and slider drags
    /// don't allocate/destroy GDI objects on every message.
    static SCRATCH: RefCell<Option<Canvas>> = const { RefCell::new(None) };
}

/// Free the cached scratch DIB (called when the settings window closes).
pub fn release_scratch() {
    SCRATCH.with_borrow_mut(|slot| *slot = None);
}

impl Canvas {
    pub fn new(like: HDC, w: i32, h: i32, bg: u32) -> Option<Canvas> {
        let (w, h) = (w.max(1), h.max(1));
        (w as usize).checked_mul(h as usize)?;
        unsafe {
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = w;
            bi.bmiHeader.biHeight = -h; // negative = top-down rows, so index = y*buf_w + x
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            let mut bits = null_mut();
            let bmp = CreateDIBSection(like, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bmp.is_null() || bits.is_null() {
                if !bmp.is_null() {
                    DeleteObject(bmp);
                }
                return None;
            }
            let dc = CreateCompatibleDC(like);
            if dc.is_null() {
                DeleteObject(bmp);
                return None;
            }
            let old = SelectObject(dc, bmp);
            let mut c = Canvas { dc, bmp, old, px: bits.cast(), buf_w: w, buf_h: h, w, h };
            c.clear(bg);
            Some(c)
        }
    }

    /// Borrow the reusable scratch canvas, growing it only when needed.
    pub fn with_scratch(like: HDC, w: i32, h: i32, bg: u32, f: impl FnOnce(&mut Canvas)) {
        let (w, h) = (w.max(1), h.max(1));
        SCRATCH.with_borrow_mut(|slot| {
            let needs_new = slot.as_ref().is_none_or(|c| c.buf_w < w || c.buf_h < h);
            if needs_new {
                let (nw, nh) = match slot.as_ref() {
                    Some(c) => (c.buf_w.max(w), c.buf_h.max(h)),
                    None => (w, h),
                };
                *slot = Canvas::new(like, nw, nh, bg);
            }
            if let Some(c) = slot.as_mut() {
                c.w = w;
                c.h = h;
                if !needs_new {
                    unsafe { GdiFlush() };
                    c.clear(bg);
                }
                f(c);
            }
        });
    }

    fn clear(&mut self, bg: u32) {
        let stride = self.buf_w as usize;
        let w = self.w as usize;
        let h = self.h as usize;
        let px = self.pixels();
        if w == stride {
            px[..w * h].fill(bg);
        } else {
            for y in 0..h {
                let row = y * stride;
                px[row..row + w].fill(bg);
            }
        }
    }

    fn pixels(&mut self) -> &mut [u32] {
        // The DIB section owns buf_w*buf_h u32s for as long as `bmp` lives (until Drop).
        unsafe { std::slice::from_raw_parts_mut(self.px, (self.buf_w as usize) * (self.buf_h as usize)) }
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
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        // GDI batches calls: flush pending text so our writes don't race it.
        unsafe { GdiFlush() };
        let (cw, ch, stride) = (self.w, self.h, self.buf_w);
        let r = r.clamp(0.0, (w.min(h)) / 2.0);
        let (x0, y0) = ((x.floor() as i32).max(0), (y.floor() as i32).max(0));
        let (x1, y1) = (((x + w).ceil() as i32).min(cw), ((y + h).ceil() as i32).min(ch));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let (hx, hy) = (w / 2.0 - r, h / 2.0 - r);

        // Interior skip / fast-fill bounds: pixels strictly inside this box have
        // d <= -1 (for fill: cov == 1.0; for stroke: cov == 0.0).
        let pad = stroke.unwrap_or(0.0) + 1.5;
        let ix0 = ((x + r.max(pad)).ceil() as i32).clamp(x0, x1);
        let ix1 = ((x + w - r.max(pad)).floor() as i32).clamp(ix0, x1);
        let iy0 = ((y + r.max(pad)).ceil() as i32).clamp(y0, y1);
        let iy1 = ((y + h - r.max(pad)).floor() as i32).clamp(iy0, y1);

        let px = self.pixels();
        for py in y0..y1 {
            let row = (py * stride) as usize;
            let in_mid_y = py >= iy0 && py < iy1 && ix0 < ix1;
            let mut pxi = x0;
            while pxi < x1 {
                if in_mid_y && pxi == ix0 {
                    if stroke.is_none() {
                        px[row + ix0 as usize..row + ix1 as usize].fill(color);
                    }
                    pxi = ix1;
                    continue;
                }
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
                    let p = &mut px[row + pxi as usize];
                    *p = if cov >= 1.0 { color } else { mix(*p, color, cov) };
                }
                pxi += 1;
            }
        }
    }

    /// GDI text (keeps ClearType and the user's UI font) on top of the shapes.
    pub fn text(&mut self, s: &str, r: RECT, color: u32, font: HFONT, flags: u32) -> RECT {
        if font.is_null() {
            return r;
        }
        let wide: Vec<u16> = s.encode_utf16().collect();
        let mut r = r;
        unsafe {
            let prev = SelectObject(self.dc, font);
            SetBkMode(self.dc, TRANSPARENT as i32);
            SetTextColor(self.dc, cref(color));
            DrawTextW(self.dc, wide.as_ptr(), wide.len() as i32, &mut r, flags | DT_NOPREFIX);
            SelectObject(self.dc, prev);
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
pub fn mix(d: u32, s: u32, a: f32) -> u32 {
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
