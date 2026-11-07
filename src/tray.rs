//! Tray icon (with a red "paused" badge variant) and its context menu.

use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicPtr, Ordering::Relaxed};
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use windows_sys::core::w;

use crate::settings::F_ENABLED;
use crate::{clipboard, ui};

pub const WM_TRAY: u32 = WM_APP + 1;

/// The hidden main window: clipboard listener, tray owner, clipboard owner.
/// Settings/undo code reaches it through here instead of threading an HWND around.
pub static MAIN: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
pub fn main_hwnd() -> HWND {
    MAIN.load(Relaxed)
}

const ID_TOGGLE: usize = 1;
const ID_UNDO: usize = 2;
const ID_SETTINGS: usize = 3;
const ID_EXIT: usize = 4;

pub fn add(hwnd: HWND) {
    notify(hwnd, NIM_ADD);
}
pub fn remove(hwnd: HWND) {
    notify(hwnd, NIM_DELETE);
}
/// Re-sync icon + tooltip with the Enabled flag.
pub fn refresh() {
    notify(main_hwnd(), NIM_MODIFY);
}

/// Single entry point for pausing, so tray, menu and settings window never disagree.
pub fn set_enabled(on: bool) {
    F_ENABLED.set(on);
    refresh();
    ui::refresh();
}

fn notify(hwnd: HWND, op: u32) {
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        let on = F_ENABLED.get();
        nid.hIcon = icon(!on);
        let tip = if on { "Inscriptio: trimming links" } else { "Inscriptio (paused)" };
        for (dst, src) in nid.szTip.iter_mut().zip(tip.encode_utf16()) {
            *dst = src; // zeroed() already provides the NUL terminator
        }
        Shell_NotifyIconW(op, &nid);
    }
}

pub fn on_tray(hwnd: HWND, lp: LPARAM) {
    // Legacy (pre-NOTIFYICON_VERSION_4) callback: LOWORD(lParam) is the mouse message.
    match (lp as u32) & 0xFFFF {
        WM_LBUTTONUP => ui::show(),
        WM_RBUTTONUP | WM_CONTEXTMENU => menu(hwnd),
        _ => {}
    }
}

fn menu(hwnd: HWND) {
    let on = F_ENABLED.get();
    unsafe {
        let m = CreatePopupMenu();
        // A verb, not a checkmark: "Trim links ✓" didn't read as the pause switch.
        AppendMenuW(m, MF_STRING, ID_TOGGLE, if on { w!("Pause") } else { w!("Resume") });
        AppendMenuW(m, MF_STRING | if clipboard::can_undo() { 0 } else { MF_GRAYED }, ID_UNDO, w!("Undo last trim"));
        AppendMenuW(m, MF_STRING, ID_SETTINGS, w!("Settings…"));
        AppendMenuW(m, MF_SEPARATOR, 0, null());
        AppendMenuW(m, MF_STRING, ID_EXIT, w!("Exit"));
        // Bold = what a left-click does, the standard shell hint.
        SetMenuDefaultItem(m, ID_SETTINGS as u32, 0);

        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        // Without the foreground dance, the menu won't close when you click
        // elsewhere. Classic shell quirk (KB135788).
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(m, TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY, pt.x, pt.y, 0, hwnd, null());
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(m);

        match cmd as usize {
            ID_TOGGLE => set_enabled(!on),
            ID_UNDO => clipboard::undo(),
            ID_SETTINGS => ui::show(),
            ID_EXIT => {
                DestroyWindow(hwnd);
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- icons

/// [normal, paused], built once. Icons live for the whole process, so they are never destroyed.
static ICONS: [AtomicPtr<c_void>; 2] = [AtomicPtr::new(null_mut()), AtomicPtr::new(null_mut())];

pub fn icon(paused: bool) -> HICON {
    let slot = &ICONS[paused as usize];
    let cached = slot.load(Relaxed);
    if !cached.is_null() {
        return cached;
    }
    let base = base_icon();
    let h = if paused { badge(base).unwrap_or(base) } else { base };
    slot.store(h, Relaxed);
    h
}

/// Embedded icon (resource ID 1, from app.rc) at the tray's small-icon size.
/// LoadImageW with SM_CXSMICON makes Windows pick the frame for the current
/// DPI (16 px at 100%, 24 at 150%, 32 at 200%) instead of downscaling
/// LoadIconW's 32 px default. That's what keeps the tray icon crisp.
fn base_icon() -> HICON {
    unsafe {
        // MAKEINTRESOURCE(1): an integer ID smuggled through a PCWSTR.
        // `without_provenance` says "this is a number, never dereferenced".
        // Don't take clippy's `dangling()` hint: for u16 that's address 2, i.e. the wrong ID.
        let h = LoadImageW(
            GetModuleHandleW(null()),
            std::ptr::without_provenance::<u16>(1),
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_DEFAULTCOLOR,
        );
        // Fallback keeps the app usable if the resource is somehow missing.
        if h.is_null() { LoadIconW(null_mut(), IDI_APPLICATION) } else { h }
    }
}

/// Copy of `base` with a red dot in the bottom-left. A transparent ring is cut
/// around the dot first, so it reads as a badge on both light and dark taskbars.
/// Pure pixel math on a BGRA buffer: no GDI+, no Direct2D, no extra deps.
fn badge(base: HICON) -> Option<HICON> {
    unsafe {
        let mut ii: ICONINFO = std::mem::zeroed();
        if GetIconInfo(base, &mut ii) == 0 {
            return None;
        }
        // GetIconInfo hands us *copies* of the bitmaps. We own them, so we must delete them.
        let cleanup = |ii: &ICONINFO| {
            DeleteObject(ii.hbmColor);
            DeleteObject(ii.hbmMask);
        };
        if ii.hbmColor.is_null() {
            cleanup(&ii);
            return None; // monochrome icon: no alpha channel to work with
        }

        let mut bm: BITMAP = std::mem::zeroed();
        GetObjectW(ii.hbmColor, size_of::<BITMAP>() as i32, (&mut bm as *mut BITMAP).cast());
        let (w, h) = (bm.bmWidth, bm.bmHeight);

        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = -h; // negative = top-down rows, so y grows downward like screen coords
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;

        let mut px = vec![0u32; (w * h) as usize];
        let dc = GetDC(null_mut());
        let got = GetDIBits(dc, ii.hbmColor, 0, h as u32, px.as_mut_ptr().cast(), &mut bmi, DIB_RGB_COLORS);
        if got == 0 {
            ReleaseDC(null_mut(), dc);
            cleanup(&ii);
            return None;
        }

        paint_dot(&mut px, w as usize, h as usize);

        let mut bits: *mut c_void = null_mut();
        let color = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        ReleaseDC(null_mut(), dc);
        if color.is_null() || bits.is_null() {
            cleanup(&ii);
            return None;
        }
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits.cast::<u32>(), px.len());

        // With a 32-bpp color bitmap Windows uses its alpha channel, and an
        // all-zero AND mask just means "use the alpha". Rows are WORD-aligned.
        let mask_bytes = vec![0u8; (w as usize).div_ceil(16) * 2 * h as usize];
        let mask = CreateBitmap(w, h, 1, 1, mask_bytes.as_ptr().cast());

        let out = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&out); // copies the bitmaps, so ours can go
        DeleteObject(color);
        DeleteObject(mask);
        cleanup(&ii);
        (!icon.is_null()).then_some(icon)
    }
}

/// Anti-aliased badge on a straight-alpha BGRA buffer (icons are not premultiplied).
fn paint_dot(px: &mut [u32], w: usize, h: usize) {
    let s = w.min(h) as f32;
    let r = s * 0.21; // 16 px icon → ~6.7 px dot: visible, not shouty
    let gap = (s / 16.0).max(1.0);
    let (cx, cy) = (r + 0.5, h as f32 - r - 0.5);
    // Coverage of a pixel by a circle of radius `rad`: a 1 px linear ramp at
    // the edge. That's cheap analytic AA, good enough at 16–32 px.
    let cover = |d: f32, rad: f32| (rad + 0.5 - d).clamp(0.0, 1.0);
    const RED: (f32, f32, f32) = (229.0, 72.0, 77.0); // #E5484D: Radix red 9, calm but clear

    for y in 0..h {
        for x in 0..w {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            let cut = cover(d, r + gap);
            if cut == 0.0 {
                continue;
            }
            let p = &mut px[y * w + x];
            let [b, g, rr, a] = p.to_le_bytes().map(f32::from);
            // 1) carve the ring: fade the existing art out
            let da = a / 255.0 * (1.0 - cut);
            // 2) "over" composite the red dot (straight-alpha formula)
            let sa = cover(d, r);
            let oa = sa + da * (1.0 - sa);
            if oa <= 0.0 {
                *p = 0;
                continue;
            }
            let mix = |sc: f32, dc: f32| ((sc * sa + dc * da * (1.0 - sa)) / oa).round() as u8;
            *p = u32::from_le_bytes([mix(RED.2, b), mix(RED.1, g), mix(RED.0, rr), (oa * 255.0).round() as u8]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::paint_dot;

    #[test]
    fn badge_lands_bottom_left_with_cutout() {
        // 16×16 opaque white "icon", straight-alpha BGRA as u32 (0xAARRGGBB).
        let (w, h) = (16, 16);
        let mut px = vec![0xFFFF_FFFFu32; w * h];
        paint_dot(&mut px, w, h);
        let at = |x: usize, y: usize| px[y * w + x].to_le_bytes(); // [b, g, r, a]

        let [b, g, r, a] = at(3, 12); // dot centre ≈ (3.86, 12.14)
        assert_eq!(a, 255);
        assert!(r > 200 && g < 100 && b < 100, "centre should be red: {r},{g},{b}");

        // (7,12) is ≈3.66 px from centre: past the dot (r≈3.36), inside the cut ring (r+gap≈4.36).
        let [.., a_ring] = at(7, 12);
        assert!(a_ring < 128, "ring should be carved out, alpha {a_ring}");

        assert_eq!(at(15, 0), [255, 255, 255, 255], "top-right must be untouched");
    }
}
