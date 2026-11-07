//! Settings window: dark, per-monitor-DPI aware, created on demand and
//! destroyed on close, so it costs nothing while you're not looking at it.
//!
//!   ┌ Inscriptio ───────────────────────┐
//!   │ ● Trimming links          [Pause] │
//!   │ ────────────────────────────────── │
//!   │ Keep YouTube timestamps      (●━) │
//!   │ Play a sound on trim         (●━) │
//!   │ Volume      ━━━━━━━●────────────  │
//!   │ Keep history                 (●━) │
//!   │ Start with Windows           (━○) │
//!   │ ────────────────────────────────── │
//!   │ Last trim                  [Undo] │
//!   │ youtube.com/watch?v=…&si=…  (grey)│
//!   │ → youtu.be/jdkJ3kjp6Ig            │
//!   │ ────────────────────────────────── │
//!   │ History  double-click…    [Clear] │
//!   │ ╭────────────────────────────────╮│
//!   │ │ 11-07 04:31  youtu.be/…        ││
//!   │ ╰────────────────────────────────╯│
//!   │ 142 links trimmed · 9.3 KB removed│
//!   └───────────────────────────────────┘
//!
//! The controls are still real Win32 controls (focus, Tab, Space, Esc, screen
//! readers and DPI come for free). Only their pixels are ours: buttons,
//! switches and list rows are owner-drawn, and the volume trackbar is
//! custom-drawn. See paint.rs for why stock theming can't do dark.

use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering::Relaxed};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CAPTION_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::UI::Controls::{
    CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_DODEFAULT, CDRF_NOTIFYITEMDRAW, CDRF_SKIPDEFAULT, DRAWITEMSTRUCT, ICC_BAR_CLASSES,
    INITCOMMONCONTROLSEX, InitCommonControlsEx, MEASUREITEMSTRUCT, NM_CUSTOMDRAW, NMCUSTOMDRAW, NMHDR, ODS_DISABLED, ODS_FOCUS,
    ODS_NOFOCUSRECT, ODS_SELECTED, ODT_LISTBOX, ODT_STATIC, SetWindowTheme, TB_ENDTRACK, TBCD_CHANNEL, TBM_GETTHUMBRECT, TBM_SETLINESIZE,
    TBM_SETPAGESIZE, TBM_SETPOS, TBM_SETRANGE, TBS_BOTH, TBS_NOTICKS, TRACKBAR_CLASSW,
};
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetFocus, IsWindowEnabled};
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use windows_sys::core::w;

use crate::paint::{self, ACCENT, BG, BORDER, Canvas, FAINT, KNOB, MUTED, RED, SURFACE, SURFACE_HI, TEXT, TRACK_OFF};
use crate::settings::{self, F_ENABLED, F_HISTORY, F_KEEP_TIME, F_SOUND, Flag};
use crate::{clipboard, history, sound, tray};

pub const WM_REFRESH: u32 = WM_APP + 2;

/// The open window, or null. One instance at most: show() re-focuses it.
static WND: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static FONT: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static FONT_BOLD: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

pub fn hwnd() -> HWND {
    WND.load(Relaxed)
}

// Style/message numbers spelled out locally: they're u32 here, while
// windows-sys types some of them as i32 (local items shadow the glob import).
const BS_OWNERDRAW: u32 = 0xB;
const BN_CLICKED: u32 = 0;
const BN_DOUBLECLICKED: u32 = 5; // owner-draw buttons send this for a fast 2nd click instead of BN_CLICKED
const SS_OWNERDRAW: u32 = 0xD;
const LBS_NOTIFY: u32 = 0x1;
const LBS_OWNERDRAWFIXED: u32 = 0x10;
const LBS_NOINTEGRALHEIGHT: u32 = 0x100;
const LB_ADDSTRING: u32 = 0x180;
const LB_RESETCONTENT: u32 = 0x184;
const LB_GETCURSEL: u32 = 0x188;
const LB_SETITEMHEIGHT: u32 = 0x1A0;
const LBN_DBLCLK: u32 = 2;
const SS_NOPREFIX: u32 = 0x80; // don't eat '&' in URLs as mnemonic markers
const SS_ENDELLIPSIS: u32 = 0x4000;
const TBM_GETPOS: u32 = WM_USER;
const UISF_HIDEFOCUS: usize = 1;

const ID_STATUS: i32 = 100;
const ID_PAUSE: i32 = 101;
const ID_TIME: i32 = 102;
const ID_SOUND: i32 = 103;
const ID_HISTORY: i32 = 104;
const ID_AUTOSTART: i32 = 105;
const ID_VOL_LBL: i32 = 106;
const ID_VOLUME: i32 = 107;
const ID_LAST_HDR: i32 = 201;
const ID_ORIG: i32 = 202;
const ID_SHORT: i32 = 203;
const ID_UNDO: i32 = 204;
const ID_HIST_HDR: i32 = 301;
const ID_HINT: i32 = 302;
const ID_CLEAR: i32 = 303;
const ID_LIST: i32 = 304;
const ID_STATS: i32 = 401;
const IDCANCEL_: i32 = 2; // what IsDialogMessage sends for Esc

/// Grey "secondary" text.
const MUTED_IDS: &[i32] = &[ID_ORIG, ID_HINT, ID_STATS, ID_VOL_LBL];

/// Layout in 96-DPI units ("DIPs"), scaled at runtime. A data table instead of
/// a sizer framework: the window is fixed-size, so absolute layout is honest.
const W: i32 = 340;
const H: i32 = 552;
const M: i32 = 16;
const SEPARATORS: [i32; 3] = [56, 224, 316];
/// Rounded frame drawn by the parent; the (borderless) listbox sits inset inside it.
const FRAME: (i32, i32, i32, i32) = (M, 362, W - 2 * M, 150);
const ROW_H: i32 = 26;

struct Ctl {
    id: i32,
    class: *const u16,
    text: *const u16,
    style: u32,
    r: (i32, i32, i32, i32),
    bold: bool,
}

const OWNER_BTN: u32 = BS_OWNERDRAW | WS_TABSTOP;
const LABEL: u32 = SS_NOPREFIX | SS_ENDELLIPSIS;

fn controls() -> [Ctl; 17] {
    let c = |id, class, text, style, r, bold| Ctl { id, class, text, style, r, bold };
    let (fx, fy, fw, fh) = FRAME;
    // Creation order = Tab order.
    [
        c(ID_STATUS, w!("STATIC"), w!(""), SS_OWNERDRAW, (M, 14, W - 2 * M - 96, 28), true),
        c(ID_PAUSE, w!("BUTTON"), w!("Pause"), OWNER_BTN, (W - M - 88, 14, 88, 28), false),
        c(ID_TIME, w!("BUTTON"), w!("Keep YouTube timestamps"), OWNER_BTN, (M, 64, W - 2 * M, 30), false),
        c(ID_SOUND, w!("BUTTON"), w!("Play a sound on trim"), OWNER_BTN, (M, 94, W - 2 * M, 30), false),
        c(ID_VOL_LBL, w!("STATIC"), w!("Volume"), LABEL, (M, 130, 60, 20), false),
        c(ID_VOLUME, TRACKBAR_CLASSW, w!(""), TBS_BOTH | TBS_NOTICKS | WS_TABSTOP, (M + 56, 124, W - 2 * M - 56, 30), false),
        c(ID_HISTORY, w!("BUTTON"), w!("Keep history"), OWNER_BTN, (M, 154, W - 2 * M, 30), false),
        c(ID_AUTOSTART, w!("BUTTON"), w!("Start with Windows"), OWNER_BTN, (M, 184, W - 2 * M, 30), false),
        c(ID_LAST_HDR, w!("STATIC"), w!("Last trim"), LABEL, (M, 238, 150, 20), true),
        c(ID_UNDO, w!("BUTTON"), w!("Undo"), OWNER_BTN, (W - M - 72, 234, 72, 28), false),
        c(ID_ORIG, w!("STATIC"), w!(""), LABEL, (M, 268, W - 2 * M, 18), false),
        c(ID_SHORT, w!("STATIC"), w!(""), LABEL, (M, 288, W - 2 * M, 18), false),
        c(ID_HIST_HDR, w!("STATIC"), w!("History"), LABEL, (M, 330, 60, 20), true),
        c(ID_HINT, w!("STATIC"), w!("double-click to copy"), LABEL, (M + 62, 331, 160, 18), false),
        c(ID_CLEAR, w!("BUTTON"), w!("Clear"), OWNER_BTN, (W - M - 72, 326, 72, 28), false),
        c(
            ID_LIST,
            w!("LISTBOX"),
            w!(""),
            LBS_NOTIFY | LBS_OWNERDRAWFIXED | LBS_NOINTEGRALHEIGHT | WS_VSCROLL | WS_TABSTOP,
            (fx + 4, fy + 4, fw - 8, fh - 8),
            false,
        ),
        c(ID_STATS, w!("STATIC"), w!(""), LABEL, (M, fy + fh + 10, W - 2 * M, 18), false),
    ]
}

// WS_CLIPCHILDREN: our WM_PAINT blits the whole client area, and must not paint over the children.
const STYLE: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;

/// Lazily created, process-lifetime brushes (two GDI objects, never worth freeing).
static BRUSH_BG: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static BRUSH_SURFACE: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

fn brush(slot: &AtomicPtr<c_void>, color: u32) -> HBRUSH {
    let b = slot.load(Relaxed);
    if !b.is_null() {
        return b;
    }
    let b = unsafe { CreateSolidBrush(paint::cref(color)) };
    slot.store(b, Relaxed);
    b
}

/// Dark context menu for the tray. Windows has no public API for this:
/// uxtheme ordinal 135 is SetPreferredAppMode (Win10 1903+), the same call
/// Explorer, Notepad++ and Windows Terminal rely on. 2 = ForceDark. On older
/// builds that ordinal was AllowDarkModeForApp(BOOL), and 2 is just "true",
/// so the worst case is that nothing happens. Must run before the first menu
/// is created, because menus cache their theme.
pub fn dark_menus() {
    unsafe {
        let ux = LoadLibraryW(w!("uxtheme.dll"));
        if ux.is_null() {
            return;
        }
        if let Some(f) = GetProcAddress(ux, std::ptr::without_provenance(135)) {
            let set: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            set(2);
        }
    }
}

pub fn show() {
    let existing = hwnd();
    if !existing.is_null() {
        unsafe {
            ShowWindow(existing, SW_SHOWNORMAL);
            SetForegroundWindow(existing);
        }
        return;
    }
    unsafe {
        let hinst = GetModuleHandleW(null());
        static REGISTERED: AtomicBool = AtomicBool::new(false);
        if !REGISTERED.swap(true, Relaxed) {
            // The trackbar lives in comctl32 and must be registered first. BUTTON,
            // STATIC and LISTBOX are user32 and always available.
            let icc = INITCOMMONCONTROLSEX { dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_BAR_CLASSES };
            InitCommonControlsEx(&icc);
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(proc),
                hInstance: hinst,
                hIcon: tray::icon(false),
                hIconSm: tray::icon(false),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                hbrBackground: brush(&BRUSH_BG, BG),
                lpszClassName: w!("InscriptioSettings"),
                ..std::mem::zeroed()
            };
            RegisterClassExW(&wc);
        }

        // Open on the monitor the cursor is on (where the tray click happened),
        // tucked into the bottom-right corner like a tray flyout.
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..std::mem::zeroed() };
        GetMonitorInfoW(MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST), &mut mi);
        let work = mi.rcWork;

        // Create it tiny *inside* that monitor so GetDpiForWindow reports that
        // monitor's DPI, then size it for real. It stays hidden until laid out, so there's no flicker.
        let h = CreateWindowExW(0, w!("InscriptioSettings"), w!("Inscriptio"), STYLE, work.right - 50, work.bottom - 50, 1, 1, null_mut(), null_mut(), hinst, null());
        if h.is_null() {
            return;
        }
        WND.store(h, Relaxed);

        // Dark title bar (documented since Win11 / Win10 20H1), then paint the
        // caption the same black as the client area (Win11 22000+; ignored before).
        let dark: i32 = 1;
        DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE as u32, (&dark as *const i32).cast(), 4);
        let caption = paint::cref(BG);
        DwmSetWindowAttribute(h, DWMWA_CAPTION_COLOR as u32, (&caption as *const u32).cast(), 4);

        for c in controls() {
            CreateWindowExW(0, c.class, c.text, WS_CHILD | WS_VISIBLE | c.style, 0, 0, 0, 0, h, c.id as isize as _, hinst, null());
        }
        let vol = GetDlgItem(h, ID_VOLUME);
        SendMessageW(vol, TBM_SETRANGE, 0, 100 << 16); // MAKELONG(min 0, max 100)
        SendMessageW(vol, TBM_SETPAGESIZE, 0, 10);
        SendMessageW(vol, TBM_SETLINESIZE, 0, 5);
        // The listbox scrollbar is the one piece we can't owner-draw. This theme
        // class gives a dark one (honoured once dark_menus() opted the app in).
        SetWindowTheme(GetDlgItem(h, ID_LIST), w!("DarkMode_Explorer"), null());

        let (ww, wh) = apply_dpi(h, GetDpiForWindow(h));
        let gap = scale(12, GetDpiForWindow(h));
        SetWindowPos(h, null_mut(), work.right - ww - gap, work.bottom - wh - gap, ww, wh, SWP_NOZORDER | SWP_NOACTIVATE);
        refresh_now(h);
        ShowWindow(h, SW_SHOW);
        SetForegroundWindow(h);
    }
}

/// Ask the window to re-read state. Posted, not sent: callers (the clipboard
/// handler) shouldn't run UI code synchronously, and repeated posts are harmless.
pub fn refresh() {
    let h = hwnd();
    if !h.is_null() {
        unsafe { PostMessageW(h, WM_REFRESH, 0, 0) };
    }
}

#[inline]
fn scale(v: i32, dpi: u32) -> i32 {
    (v * dpi as i32 + 48) / 96 // rounded integer MulDiv
}

/// (Re)create fonts for `dpi`, place every control, and return the outer window size.
fn apply_dpi(h: HWND, dpi: u32) -> (i32, i32) {
    unsafe {
        // The user's actual UI font (Segoe UI Variable / Segoe UI), at this monitor's DPI.
        let mut ncm: NONCLIENTMETRICSW = std::mem::zeroed();
        ncm.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
        SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS, ncm.cbSize, (&mut ncm as *mut NONCLIENTMETRICSW).cast(), 0, dpi);
        let normal = CreateFontIndirectW(&ncm.lfMessageFont);
        let mut lf = ncm.lfMessageFont;
        lf.lfWeight = 600; // semibold section headers
        let bold = CreateFontIndirectW(&lf);
        // Swap in the new fonts and free the old ones (GDI objects leak across DPI changes otherwise).
        for (slot, f) in [(&FONT, normal), (&FONT_BOLD, bold)] {
            let old = slot.swap(f, Relaxed);
            if !old.is_null() {
                DeleteObject(old);
            }
        }

        for c in controls() {
            let ctl = GetDlgItem(h, c.id);
            let (x, y, cw, ch) = c.r;
            MoveWindow(ctl, scale(x, dpi), scale(y, dpi), scale(cw, dpi), scale(ch, dpi), 0);
            let f = if c.bold { bold } else { normal };
            SendMessageW(ctl, WM_SETFONT, f as WPARAM, 1);
        }
        // WM_MEASUREITEM only fires once, at creation; after a DPI change we set the row height ourselves.
        SendDlgItemMessageW(h, ID_LIST, LB_SETITEMHEIGHT, 0, scale(ROW_H, dpi) as LPARAM);

        let mut r = RECT { left: 0, top: 0, right: scale(W, dpi), bottom: scale(H, dpi) };
        AdjustWindowRectExForDpi(&mut r, STYLE, 0, 0, dpi);
        InvalidateRect(h, null(), 1);
        (r.right - r.left, r.bottom - r.top)
    }
}

/// Strip the scheme for display: it's noise in a narrow list.
fn bare(link: &str) -> &str {
    link.strip_prefix("https://").or_else(|| link.strip_prefix("http://")).unwrap_or(link)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn set_text(h: HWND, id: i32, s: &str) {
    unsafe { SetDlgItemTextW(h, id, wide(s).as_ptr()) };
}

fn human_bytes(b: u64) -> String {
    match b {
        0..1024 => format!("{b} B"),
        1024..1_048_576 => format!("{:.1} KB", b as f64 / 1024.0),
        _ => format!("{:.1} MB", b as f64 / 1_048_576.0),
    }
}

fn refresh_now(h: HWND) {
    unsafe {
        let vol = GetDlgItem(h, ID_VOLUME);
        EnableWindow(vol, F_SOUND.get() as i32);
        SendMessageW(vol, TBM_SETPOS, 1, settings::VOLUME.load(Relaxed) as LPARAM);

        clipboard::with_last(|l| match l {
            Some(l) => {
                set_text(h, ID_ORIG, bare(&l.original));
                let suffix = if l.undone { "   (undone)" } else { "" };
                set_text(h, ID_SHORT, &format!("→ {}{suffix}", bare(&l.short)));
            }
            None => {
                set_text(h, ID_ORIG, "Nothing trimmed yet. Copy a link.");
                set_text(h, ID_SHORT, "");
            }
        });
        EnableWindow(GetDlgItem(h, ID_UNDO), clipboard::can_undo() as i32);

        let list = GetDlgItem(h, ID_LIST);
        // WM_SETREDRAW off while refilling: one repaint instead of one per row.
        SendMessageW(list, WM_SETREDRAW, 0, 0);
        SendMessageW(list, LB_RESETCONTENT, 0, 0);
        let empty = history::with(|items| {
            // Owner-drawn rows read history at paint time, so the list only needs the row count.
            for _ in items {
                SendMessageW(list, LB_ADDSTRING, 0, 0);
            }
            items.is_empty()
        });
        SendMessageW(list, WM_SETREDRAW, 1, 0);
        EnableWindow(GetDlgItem(h, ID_CLEAR), (!empty) as i32);

        let n = settings::TRIMMED.load(Relaxed);
        let stats = match n {
            0 => "No links trimmed yet".to_owned(),
            _ => format!(
                "{n} link{} trimmed · {} of junk removed",
                if n == 1 { "" } else { "s" },
                human_bytes(settings::SAVED_BYTES.load(Relaxed))
            ),
        };
        set_text(h, ID_STATS, &stats);

        // Owner-drawn controls read their state while painting, so "refresh" = repaint them all.
        RedrawWindow(h, null(), null_mut(), RDW_INVALIDATE | RDW_ALLCHILDREN);
    }
}

// ---------------------------------------------------------------- drawing

fn toggle_flag(id: i32) -> Option<Flag> {
    match id {
        ID_TIME => Some(F_KEEP_TIME),
        ID_SOUND => Some(F_SOUND),
        ID_HISTORY => Some(F_HISTORY),
        _ => None,
    }
}

fn is_on(id: i32) -> bool {
    match toggle_flag(id) {
        Some(f) => f.get(),
        None => settings::autostart_enabled(), // ID_AUTOSTART: the Run key is the source of truth
    }
}

fn label(id: i32) -> &'static str {
    match id {
        ID_PAUSE if F_ENABLED.get() => "Pause",
        ID_PAUSE => "Resume",
        ID_TIME => "Keep YouTube timestamps",
        ID_SOUND => "Play a sound on trim",
        ID_HISTORY => "Keep history",
        ID_AUTOSTART => "Start with Windows",
        ID_UNDO => "Undo",
        _ => "Clear",
    }
}

const ONE_LINE: u32 = DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS;

fn draw_item(d: &DRAWITEMSTRUCT) {
    let r = d.rcItem;
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    let k = unsafe { GetDpiForWindow(d.hwndItem) } as f32 / 96.0;
    let s = |v: f32| v * k;
    let (font, bold) = (FONT.load(Relaxed), FONT_BOLD.load(Relaxed));
    let full = RECT { left: 0, top: 0, right: w, bottom: h };
    let id = d.CtlID as i32;
    let (wf, hf) = (w as f32, h as f32);
    // Focus ring only for keyboard users: ODS_NOFOCUSRECT is set while Windows hides focus cues.
    let focus = d.itemState & ODS_FOCUS != 0 && d.itemState & ODS_NOFOCUSRECT == 0;

    let c = match d.CtlType {
        ODT_LISTBOX => {
            // itemID = -1 means "empty list, draw focus only". Nothing to draw then.
            let Some((when, link)) = history::with(|items| items.get(d.itemID as usize).map(|e| (e.when.clone(), e.link.clone()))) else {
                return;
            };
            let bg = if d.itemState & ODS_SELECTED != 0 { SURFACE_HI } else { SURFACE };
            let Some(mut c) = Canvas::new(d.hDC, w, h, bg) else { return };
            let pad = s(10.0) as i32;
            let t = c.text(&when, RECT { left: pad, ..full }, MUTED, font, ONE_LINE | DT_CALCRECT);
            c.text(&when, RECT { left: pad, ..full }, MUTED, font, ONE_LINE);
            c.text(bare(&link), RECT { left: t.right + s(12.0) as i32, right: w - pad, ..full }, TEXT, font, ONE_LINE);
            c
        }
        ODT_STATIC => {
            // Status row: a coloured dot that matches the tray badge, plus the state in words.
            let Some(mut c) = Canvas::new(d.hDC, w, h, BG) else { return };
            let on = F_ENABLED.get();
            let dot = s(10.0);
            if on {
                c.fill(s(1.0) - s(3.0), (hf - dot) / 2.0 - s(3.0), dot + s(6.0), dot + s(6.0), dot / 2.0 + s(3.0), 0x1A3A2B); // soft glow
            }
            c.fill(s(1.0), (hf - dot) / 2.0, dot, dot, dot / 2.0, if on { ACCENT } else { RED });
            let text = if on { "Trimming links" } else { "Paused" };
            c.text(text, RECT { left: (s(1.0) + dot + s(10.0)) as i32, ..full }, TEXT, bold, ONE_LINE);
            c
        }
        _ if toggle_flag(id).is_some() || id == ID_AUTOSTART => {
            // Label on the left, pill switch on the right; the whole row is the hit target.
            let Some(mut c) = Canvas::new(d.hDC, w, h, BG) else { return };
            let on = is_on(id);
            let (tw, th) = (s(36.0), s(20.0));
            let (tx, ty) = (wf - tw - s(1.0), (hf - th) / 2.0);
            if focus {
                c.stroke(tx - s(3.0), ty - s(3.0), tw + s(6.0), th + s(6.0), th / 2.0 + s(3.0), s(1.5), MUTED);
            }
            c.fill(tx, ty, tw, th, th / 2.0, if on { ACCENT } else { TRACK_OFF });
            let kd = th - s(6.0);
            let kx = if on { tx + tw - s(3.0) - kd } else { tx + s(3.0) };
            c.fill(kx, ty + s(3.0), kd, kd, kd / 2.0, KNOB);
            c.text(label(id), RECT { right: (tx - s(8.0)) as i32, ..full }, TEXT, font, ONE_LINE);
            c
        }
        _ => {
            // Push buttons. "Resume" is the one call to action, so it alone gets the accent fill.
            let Some(mut c) = Canvas::new(d.hDC, w, h, BG) else { return };
            let disabled = d.itemState & ODS_DISABLED != 0;
            let pressed = d.itemState & ODS_SELECTED != 0;
            let primary = id == ID_PAUSE && !F_ENABLED.get();
            let rad = s(6.0);
            let (fill, fg) = match (primary, pressed, disabled) {
                (true, _, _) => (ACCENT, BG),
                (_, _, true) => (SURFACE, FAINT),
                (_, true, _) => (SURFACE_HI, TEXT),
                _ => (SURFACE, TEXT),
            };
            c.fill(0.0, 0.0, wf, hf, rad, fill);
            if !primary {
                c.stroke(0.0, 0.0, wf, hf, rad, 1.0, BORDER);
            }
            if focus {
                c.stroke(0.0, 0.0, wf, hf, rad, s(1.5), MUTED);
            }
            c.text(label(id), full, fg, font, ONE_LINE | DT_CENTER);
            c
        }
    };
    c.blit(d.hDC, r.left, r.top);
}

/// Volume slider: thin track, accent fill up to the thumb, round knob. Drawn in
/// one go at the channel stage; the stock thumb and tics are suppressed.
fn draw_slider(cd: &NMCUSTOMDRAW) {
    unsafe {
        let hw = cd.hdr.hwndFrom;
        let mut cr: RECT = std::mem::zeroed();
        GetClientRect(hw, &mut cr);
        let mut th: RECT = std::mem::zeroed();
        SendMessageW(hw, TBM_GETTHUMBRECT, 0, &mut th as *mut RECT as LPARAM);
        let k = GetDpiForWindow(hw) as f32 / 96.0;
        let s = |v: f32| v * k;
        let enabled = IsWindowEnabled(hw) != 0;
        let focus = GetFocus() == hw && SendMessageW(hw, WM_QUERYUISTATE, 0, 0) as usize & UISF_HIDEFOCUS == 0;
        let Some(mut c) = Canvas::new(cd.hdc, cr.right, cr.bottom, BG) else { return };

        // cd.rc is the channel: it spans thumb-centre-at-0 to thumb-centre-at-100.
        let (x0, x1) = (cd.rc.left as f32, cd.rc.right as f32);
        let (cx, cy) = ((th.left + th.right) as f32 / 2.0, (th.top + th.bottom) as f32 / 2.0);
        let t = s(4.0);
        c.fill(x0, cy - t / 2.0, x1 - x0, t, t / 2.0, TRACK_OFF);
        c.fill(x0, cy - t / 2.0, (cx - x0).max(t), t, t / 2.0, if enabled { ACCENT } else { FAINT });
        let kr = s(8.0);
        if focus {
            c.stroke(cx - kr - s(3.0), cy - kr - s(3.0), 2.0 * kr + s(6.0), 2.0 * kr + s(6.0), kr + s(3.0), s(1.5), MUTED);
        }
        c.fill(cx - kr, cy - kr, 2.0 * kr, 2.0 * kr, kr, if enabled { KNOB } else { FAINT });
        c.blit(cd.hdc, 0, 0);
    }
}

/// Window background, separators and the list's rounded frame.
fn paint_bg(h: HWND, hdc: HDC) {
    unsafe {
        let mut cr: RECT = std::mem::zeroed();
        GetClientRect(h, &mut cr);
        let dpi = GetDpiForWindow(h);
        let sc = |v: i32| scale(v, dpi) as f32;
        let Some(mut c) = Canvas::new(hdc, cr.right, cr.bottom, BG) else { return };
        for y in SEPARATORS {
            c.fill(sc(M), sc(y), sc(W - 2 * M), 1.0, 0.0, BORDER); // exactly 1 px at any scale: a hairline
        }
        let (fx, fy, fw, fh) = FRAME;
        let rad = sc(8);
        c.fill(sc(fx), sc(fy), sc(fw), sc(fh), rad, SURFACE);
        c.stroke(sc(fx), sc(fy), sc(fw), sc(fh), rad, 1.0, BORDER);
        c.blit(hdc, 0, 0);
    }
}

// ---------------------------------------------------------------- messages

unsafe extern "system" fn proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_COMMAND => {
                let (id, code) = ((wp & 0xFFFF) as i32, ((wp >> 16) & 0xFFFF) as u32);
                let clicked = code == BN_CLICKED || code == BN_DOUBLECLICKED;
                match id {
                    ID_PAUSE if clicked => tray::set_enabled(!F_ENABLED.get()), // also repaints the tray badge
                    ID_TIME | ID_SOUND | ID_HISTORY | ID_AUTOSTART if clicked => {
                        let on = !is_on(id);
                        match toggle_flag(id) {
                            Some(f) => f.set(on),
                            None => settings::set_autostart(on),
                        }
                        InvalidateRect(GetDlgItem(h, id), null(), 0);
                        if id == ID_SOUND {
                            let vol = GetDlgItem(h, ID_VOLUME);
                            EnableWindow(vol, on as i32);
                            InvalidateRect(vol, null(), 0);
                            if on {
                                sound::play(); // preview: you hear what you just enabled
                            }
                        }
                    }
                    ID_UNDO if clicked => clipboard::undo(),
                    ID_CLEAR if clicked => {
                        history::clear();
                        refresh_now(h);
                    }
                    ID_LIST if code == LBN_DBLCLK => {
                        let sel = SendDlgItemMessageW(h, ID_LIST, LB_GETCURSEL, 0, 0);
                        // LB_ERR is -1, so the usize conversion fails and we skip.
                        if let Ok(i) = usize::try_from(sel)
                            && let Some(link) = history::with(|items| items.get(i).map(|e| e.link.clone()))
                        {
                            clipboard::copy(&link);
                            set_text(h, ID_HINT, "copied ✓");
                            SetTimer(h, 1, 1500, None); // restore the hint shortly
                        }
                    }
                    IDCANCEL_ => {
                        DestroyWindow(h); // Esc closes, via IsDialogMessage in the main loop
                    }
                    _ => {}
                }
                0
            }
            WM_HSCROLL => {
                let ctl = lp as HWND;
                if GetDlgCtrlID(ctl) == ID_VOLUME {
                    // The trackbar only invalidates the old and new thumb rects; the
                    // accent fill between them is ours, so repaint the whole control.
                    InvalidateRect(ctl, null(), 0);
                    // Save and preview on release (or keyup), not on every drag pixel.
                    if (wp & 0xFFFF) as u32 == TB_ENDTRACK {
                        settings::set_volume(SendMessageW(ctl, TBM_GETPOS, 0, 0) as u32);
                        sound::play();
                    }
                }
                0
            }
            WM_NOTIFY => {
                let hdr = &*(lp as *const NMHDR);
                if hdr.code == NM_CUSTOMDRAW && hdr.idFrom == ID_VOLUME as usize {
                    let cd = &*(lp as *const NMCUSTOMDRAW);
                    return match cd.dwDrawStage {
                        CDDS_PREPAINT => CDRF_NOTIFYITEMDRAW as LRESULT,
                        CDDS_ITEMPREPAINT => {
                            if cd.dwItemSpec == TBCD_CHANNEL as usize {
                                draw_slider(cd);
                            }
                            CDRF_SKIPDEFAULT as LRESULT // tics and thumb: already drawn, or not wanted
                        }
                        _ => CDRF_DODEFAULT as LRESULT,
                    };
                }
                0
            }
            WM_DRAWITEM => {
                draw_item(&*(lp as *const DRAWITEMSTRUCT));
                1
            }
            WM_MEASUREITEM => {
                (*(lp as *mut MEASUREITEMSTRUCT)).itemHeight = scale(ROW_H, GetDpiForWindow(h)) as u32;
                1
            }
            WM_TIMER => {
                KillTimer(h, 1);
                set_text(h, ID_HINT, "double-click to copy");
                0
            }
            WM_REFRESH => {
                refresh_now(h);
                0
            }
            WM_CTLCOLORSTATIC => {
                // Statics (and the trackbar's background) ask the parent for colours.
                let hdc = wp as HDC;
                let id = GetDlgCtrlID(lp as HWND);
                SetBkColor(hdc, paint::cref(BG));
                SetTextColor(hdc, paint::cref(if MUTED_IDS.contains(&id) { MUTED } else { TEXT }));
                brush(&BRUSH_BG, BG) as LRESULT
            }
            WM_CTLCOLORBTN => brush(&BRUSH_BG, BG) as LRESULT,
            WM_CTLCOLORLISTBOX => brush(&BRUSH_SURFACE, SURFACE) as LRESULT, // the area below the last row
            WM_ERASEBKGND => 1, // WM_PAINT covers every pixel; erasing first would only flicker
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let hdc = BeginPaint(h, &mut ps);
                paint_bg(h, hdc);
                EndPaint(h, &ps);
                0
            }
            WM_DPICHANGED => {
                // Moved to a monitor with different scaling: rebuild fonts and
                // layout, then take the rect Windows suggests (keeps the window under the cursor).
                apply_dpi(h, (wp & 0xFFFF) as u32);
                let r = &*(lp as *const RECT);
                SetWindowPos(h, null_mut(), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                0
            }
            WM_CLOSE => {
                DestroyWindow(h);
                0
            }
            WM_DESTROY => {
                // Only this window goes away. No PostQuitMessage: the app keeps running in the tray.
                WND.store(null_mut(), Relaxed);
                for slot in [&FONT, &FONT_BOLD] {
                    let f = slot.swap(null_mut(), Relaxed);
                    if !f.is_null() {
                        DeleteObject(f);
                    }
                }
                0
            }
            _ => DefWindowProcW(h, msg, wp, lp),
        }
    }
}
