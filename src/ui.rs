//! Fixed-size, per-monitor-DPI settings window. Native controls provide keyboard
//! and accessibility behavior while GDI draws their dark appearance.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering::Relaxed};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CAPTION_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::UI::Controls::{
    CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_DODEFAULT, CDRF_NOTIFYITEMDRAW, CDRF_SKIPDEFAULT, DRAWITEMSTRUCT, ICC_BAR_CLASSES,
    INITCOMMONCONTROLSEX, InitCommonControlsEx, MEASUREITEMSTRUCT, NM_CUSTOMDRAW, NMCUSTOMDRAW, NMHDR, ODS_DISABLED, ODS_FOCUS,
    ODS_NOFOCUSRECT, ODS_SELECTED, ODT_LISTBOX, TB_ENDTRACK, TBCD_CHANNEL, TBM_GETTHUMBRECT, TBM_SETLINESIZE,
    TBM_SETPAGESIZE, TBM_SETPOS, TBM_SETRANGE, TBS_BOTH, TBS_NOTICKS, TRACKBAR_CLASSW, WM_MOUSELEAVE,
};
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetCapture, GetFocus, GetKeyState, IsWindowEnabled, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE,
    TRACKMOUSEEVENT, TrackMouseEvent, VK_C, VK_CONTROL,
};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use windows_sys::core::w;

use crate::paint::{self, ACCENT, BG, BORDER, Canvas, FAINT, KNOB, MUTED, RED, SURFACE, SURFACE_HI, TEXT, TRACK_OFF, mix};
use crate::settings::{self, F_ENABLED, F_HISTORY, F_KEEP_TIME, F_SOUND, Flag};
use crate::{clipboard, history, sound, tray};

pub const WM_REFRESH: u32 = WM_APP + 2;

/// The open window, or null. One instance at most: show() re-focuses it.
static WND: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static FONT: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static FONT_TITLE: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static FONT_BOLD: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
/// History generation last loaded into ID_LIST (0 = not yet populated).
static LAST_HIST_GEN: AtomicU32 = AtomicU32::new(0);

const ACCENT_PRESSED: u32 = 0x31B781;
const ACTIVE_PANEL: u32 = 0x142520;
const PAUSED_PANEL: u32 = 0x241C23;
const LINK_BG: u32 = 0x13181F;
const BUTTON_IDLE: u32 = 0x252D37;
const BUTTON_HOVER: u32 = 0x344151;
const BUTTON_ACTIVE: u32 = 0x3D4B5D;
const FOCUS: u32 = 0x94D9BC;

const COPY_TIMER: usize = 1;
const ANIMATION_TIMER: usize = 2;
const TRANSITION: Duration = Duration::from_millis(150);

pub fn hwnd() -> HWND {
    WND.load(Relaxed)
}

// Style/message numbers spelled out locally: they're u32 here, while
// windows-sys types some of them as i32 (local items shadow the glob import).
const BS_OWNERDRAW: u32 = 0xB;
const BN_CLICKED: u32 = 0;
const BN_DOUBLECLICKED: u32 = 5; // owner-draw buttons send this for a fast 2nd click instead of BN_CLICKED
const LBS_NOTIFY: u32 = 0x1;
const LBS_OWNERDRAWFIXED: u32 = 0x10;
const LBS_HASSTRINGS: u32 = 0x40; // exposes row text to screen readers & type-ahead
const LBS_NOINTEGRALHEIGHT: u32 = 0x100;
const LBS_WANTKEYBOARDINPUT: u32 = 0x400; // enables WM_VKEYTOITEM for Ctrl+C in the list
const LB_ADDSTRING: u32 = 0x180;
const LB_RESETCONTENT: u32 = 0x184;
const LB_GETCOUNT: u32 = 0x18B;
const LB_GETTOPINDEX: u32 = 0x18E;
const LB_SETTOPINDEX: u32 = 0x197;
const LB_GETCURSEL: u32 = 0x188;
const LB_SETITEMHEIGHT: u32 = 0x1A0;
const LBN_DBLCLK: u32 = 2;
const SS_NOPREFIX: u32 = 0x80; // don't eat '&' in URLs as mnemonic markers
const SS_CENTERIMAGE: u32 = 0x200;
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
const ID_STATUS_HINT: i32 = 108;
const ID_PREFS: i32 = 109;
const ID_VOL_VALUE: i32 = 110;
const ID_FROM: i32 = 205;
const ID_TO: i32 = 206;
const ID_EMPTY: i32 = 305;
const ID_TOTAL: i32 = 402;
const ID_SAVED: i32 = 403;
const ID_TOTAL_LABEL: i32 = 404;
const ID_SAVED_LABEL: i32 = 405;
const ID_LAST_HDR: i32 = 201;
const ID_ORIG: i32 = 202;
const ID_SHORT: i32 = 203;
const ID_UNDO: i32 = 204;
const ID_HIST_HDR: i32 = 301;
const ID_HINT: i32 = 302;
const ID_CLEAR: i32 = 303;
const ID_LIST: i32 = 304;
const IDOK_: i32 = 1; // what IsDialogMessage sends for Enter
const IDCANCEL_: i32 = 2; // what IsDialogMessage sends for Esc

const MUTED_IDS: &[i32] = &[ID_HINT, ID_VOL_LBL, ID_FROM, ID_TO, ID_ORIG, ID_TOTAL_LABEL, ID_SAVED_LABEL];

/// Panel and control bounds at 96 DPI.
const W: i32 = 560;
const H: i32 = 550;
const HERO: (i32, i32, i32, i32) = (20, 18, 520, 78);
const PREFS: (i32, i32, i32, i32) = (20, 112, 236, 218);
const LAST: (i32, i32, i32, i32) = (268, 112, 272, 146);
const STATS: (i32, i32, i32, i32) = (268, 270, 272, 60);
const HISTORY: (i32, i32, i32, i32) = (20, 346, 520, 184);
const ROW_H: i32 = 26;
const VISIBLE_ROWS: i32 = 5;
const LIST: (i32, i32, i32, i32) = (32, 390, 480, ROW_H * VISIBLE_ROWS);
const SCROLL_GUTTER: i32 = 512;
const SCROLL_X: i32 = 522;
const SCROLL_W: i32 = 5;

#[derive(Clone, Copy)]
enum Font {
    Body,
    Heading,
    Title,
}

struct Ctl {
    id: i32,
    class: *const u16,
    text: &'static str,
    style: u32,
    r: (i32, i32, i32, i32),
    font: Font,
}

const OWNER_BTN: u32 = BS_OWNERDRAW | WS_TABSTOP;
const LABEL: u32 = SS_NOPREFIX | SS_ENDELLIPSIS | SS_CENTERIMAGE;

fn controls() -> [Ctl; 26] {
    use Font::{Body, Heading, Title};
    let c = |id, class, text, style, r, font| Ctl { id, class, text, style, r, font };
    let text = |id, value, r, font| c(id, w!("STATIC"), value, LABEL, r, font);
    let button = |id, r| c(id, w!("BUTTON"), label(id), OWNER_BTN, r, Body);
    // Creation order determines the Tab order. The empty state replaces the list itself.
    [
        text(ID_STATUS, "", (58, 30, 324, 30), Title),
        text(ID_STATUS_HINT, "", (58, 62, 354, 20), Body),
        button(ID_PAUSE, (426, 38, 98, 38)),
        text(ID_PREFS, "Preferences", (34, 121, 202, 24), Heading),
        button(ID_TIME, (30, 150, 216, 34)),
        button(ID_SOUND, (30, 184, 216, 34)),
        text(ID_VOL_LBL, "Volume", (40, 220, 51, 24), Body),
        c(ID_VOLUME, TRACKBAR_CLASSW, "Volume", TBS_BOTH | TBS_NOTICKS | WS_TABSTOP, (93, 218, 109, 28), Body),
        text(ID_VOL_VALUE, "", (209, 220, 37, 24), Body),
        button(ID_HISTORY, (30, 250, 216, 34)),
        button(ID_AUTOSTART, (30, 284, 216, 34)),
        text(ID_LAST_HDR, "Last trim", (282, 121, 151, 27), Heading),
        button(ID_UNDO, (449, 121, 76, 28)),
        text(ID_FROM, "Original", (292, 159, 52, 30), Body),
        text(ID_ORIG, "", (351, 159, 162, 30), Body),
        text(ID_TO, "Cleaned", (292, 201, 52, 30), Body),
        text(ID_SHORT, "", (351, 201, 162, 30), Body),
        text(ID_TOTAL, "", (282, 276, 111, 27), Title),
        text(ID_TOTAL_LABEL, "Links trimmed", (282, 304, 112, 17), Body),
        text(ID_SAVED, "", (417, 276, 111, 27), Title),
        text(ID_SAVED_LABEL, "Data removed", (417, 304, 111, 17), Body),
        text(ID_HIST_HDR, "History", (34, 354, 64, 27), Heading),
        text(ID_HINT, "", (103, 354, 299, 27), Body),
        button(ID_CLEAR, (449, 354, 76, 28)),
        c(ID_LIST, w!("LISTBOX"), "", LBS_NOTIFY | LBS_OWNERDRAWFIXED | LBS_HASSTRINGS | LBS_NOINTEGRALHEIGHT
            | LBS_WANTKEYBOARDINPUT | WS_TABSTOP, LIST, Body),
        text(ID_EMPTY, "", (44, 436, 453, 32), Body),
    ]
}

// WS_CLIPCHILDREN: our WM_PAINT blits the whole client area, and must not paint over the children.
const STYLE: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;

/// Lazily create one process-lifetime brush for each static-control background.
static BRUSH_BG: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static BRUSH_SURFACE: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static BRUSH_ACTIVE: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static BRUSH_PAUSED: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static BRUSH_LINK: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

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
        settings::refresh_autostart();
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
                hIcon: tray::large_icon(),
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
        LAST_HIST_GEN.store(0, Relaxed);

        // Dark title bar (documented since Win11 / Win10 20H1), then paint the
        // caption the same black as the client area (Win11 22000+; ignored before).
        let dark: i32 = 1;
        DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE as u32, (&dark as *const i32).cast(), 4);
        let caption = paint::cref(BG);
        DwmSetWindowAttribute(h, DWMWA_CAPTION_COLOR as u32, (&caption as *const u32).cast(), 4);

        for c in controls() {
            let text_w = wide(c.text);
            CreateWindowExW(0, c.class, text_w.as_ptr(), WS_CHILD | WS_VISIBLE | c.style, 0, 0, 0, 0, h, c.id as isize as _, hinst, null());
        }
        let vol = GetDlgItem(h, ID_VOLUME);
        for id in [ID_PAUSE, ID_TIME, ID_SOUND, ID_VOLUME, ID_HISTORY, ID_AUTOSTART, ID_UNDO, ID_CLEAR, ID_LIST] {
            let child = GetDlgItem(h, id);
            SetWindowSubclass(child, Some(control_proc), 0, 0);
        }
        SendMessageW(vol, TBM_SETRANGE, 0, 100 << 16); // MAKELONG(min 0, max 100)
        SendMessageW(vol, TBM_SETPAGESIZE, 0, 10);
        SendMessageW(vol, TBM_SETLINESIZE, 0, 5);
        let dpi = GetDpiForWindow(h);
        let (ww, wh) = apply_dpi(h, dpi);
        let gap = scale(12, dpi);
        // Clamp to work.left/top so short high-DPI displays never push the titlebar off-screen.
        let x = (work.right - ww - gap).max(work.left);
        let y = (work.bottom - wh - gap).max(work.top);
        SetWindowPos(h, null_mut(), x, y, ww, wh, SWP_NOZORDER | SWP_NOACTIVATE);
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
        lf.lfHeight = -scale(22, dpi);
        let title = CreateFontIndirectW(&lf);
        let old_fonts = [
            FONT.swap(normal, Relaxed),
            FONT_BOLD.swap(bold, Relaxed),
            FONT_TITLE.swap(title, Relaxed),
        ];

        for c in controls() {
            let ctl = GetDlgItem(h, c.id);
            let (x, y, cw, ch) = c.r;
            let height = if c.id == ID_LIST { VISIBLE_ROWS * scale(ROW_H, dpi) } else { scale(ch, dpi) };
            MoveWindow(ctl, scale(x, dpi), scale(y, dpi), scale(cw, dpi), height, 0);
            let f = match c.font {
                Font::Body => normal,
                Font::Heading => bold,
                Font::Title => title,
            };
            SendMessageW(ctl, WM_SETFONT, f as WPARAM, 1);
        }
        for old in old_fonts {
            if !old.is_null() {
                DeleteObject(old);
            }
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

fn sync_toggle_text(h: HWND, id: i32) {
    let state = if is_on(id) { "on" } else { "off" };
    set_text(h, id, &format!("{}, {state}", label(id)));
}

fn trimming() -> bool {
    tray::LISTENER_OK.load(Relaxed) && F_ENABLED.get()
}

fn status() -> (&'static str, &'static str) {
    if !tray::LISTENER_OK.load(Relaxed) {
        ("Clipboard listener failed", "Restart Inscriptio to try again.")
    } else if trimming() {
        ("Trimming is on", "Copy a link to remove tracking automatically.")
    } else {
        ("Trimming is paused", "Copied links stay as they are.")
    }
}

fn show_volume(h: HWND, value: u32) {
    set_text(h, ID_VOL_VALUE, &format!("{value}%"));
}

fn refresh_now(h: HWND) {
    unsafe {
        // Keep Win32 control text in sync for screen readers (Narrator/NVDA).
        let (title, hint) = status();
        set_text(h, ID_STATUS, title);
        set_text(h, ID_STATUS_HINT, hint);
        set_text(h, ID_PAUSE, label(ID_PAUSE));
        for id in [ID_TIME, ID_SOUND, ID_HISTORY, ID_AUTOSTART] {
            sync_toggle_text(h, id);
        }

        let vol = GetDlgItem(h, ID_VOLUME);
        EnableWindow(vol, F_SOUND.get() as i32);
        if GetCapture() != vol {
            let value = settings::VOLUME.load(Relaxed);
            SendMessageW(vol, TBM_SETPOS, 1, value as LPARAM);
            show_volume(h, value);
        }

        // Clone strings before calling SetDlgItemTextW so no RefCell borrow is
        // held across synchronous Win32 messages.
        let (orig_txt, short_txt) = clipboard::with_last(|last| match last {
            Some(l) => {
                let original = if l.private { "Private link" } else { bare(&l.original) };
                let cleaned = if l.private { "Hidden" } else { bare(&l.short) };
                let status = if l.undone { " (undone)" } else { "" };
                (original.to_owned(), format!("{cleaned}{status}"))
            }
            None => ("No link copied yet".to_owned(), "—".to_owned()),
        });
        set_text(h, ID_ORIG, &orig_txt);
        set_text(h, ID_SHORT, &short_txt);
        EnableWindow(GetDlgItem(h, ID_UNDO), clipboard::can_undo() as i32);

        // Only rebuild ID_LIST when history actually mutated, preserving selection & scroll.
        let hist_gen = history::generation();
        if LAST_HIST_GEN.swap(hist_gen, Relaxed) != hist_gen {
            let rows: Vec<Vec<u16>> = history::with(|items| {
                items.iter().map(|e| wide(&format!("{} {}", e.when, bare(&e.link)))).collect()
            });
            let list = GetDlgItem(h, ID_LIST);
            // WM_SETREDRAW off while refilling: one repaint instead of one per row.
            SendMessageW(list, WM_SETREDRAW, 0, 0);
            SendMessageW(list, LB_RESETCONTENT, 0, 0);
            for row in &rows {
                SendMessageW(list, LB_ADDSTRING, 0, row.as_ptr() as LPARAM);
            }
            SendMessageW(list, WM_SETREDRAW, 1, 0);
            let empty = rows.is_empty();
            if empty && [list, GetDlgItem(h, ID_CLEAR)].contains(&GetFocus()) {
                SetFocus(GetDlgItem(h, ID_HISTORY));
            }
            ShowWindow(list, if empty { SW_HIDE } else { SW_SHOWNA });
            ShowWindow(GetDlgItem(h, ID_EMPTY), if empty { SW_SHOWNA } else { SW_HIDE });
            EnableWindow(GetDlgItem(h, ID_CLEAR), (!empty) as i32);
        }
        update_history_text(h);

        set_text(h, ID_TOTAL, &settings::TRIMMED.load(Relaxed).to_string());
        set_text(h, ID_SAVED, &human_bytes(settings::SAVED_BYTES.load(Relaxed)));

        // Owner-drawn controls read their state while painting, so "refresh" = repaint them all.
        RedrawWindow(h, null(), null_mut(), RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_FRAME);
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

    if d.CtlType == ODT_LISTBOX {
        let Some((when, link)) = history::with(|items| items.get(d.itemID as usize).map(|e| (e.when.clone(), e.link.clone()))) else {
            return;
        };
        let selected = d.itemState & ODS_SELECTED != 0;
        Canvas::with_scratch(d.hDC, w, h, SURFACE, |c| {
            if selected {
                c.fill(0.0, s(1.0), wf, hf - s(2.0), s(5.0), SURFACE_HI);
                c.fill(s(2.0), s(6.0), s(2.0), hf - s(12.0), s(1.0), ACCENT);
            }
            if focus {
                c.stroke(0.0, s(1.0), wf, hf - s(2.0), s(5.0), s(1.0), FOCUS);
            }
            let pad = s(12.0).round() as i32;
            let time = RECT { left: pad, ..full };
            let time_width = c.text(&when, time, MUTED, font, ONE_LINE | DT_CALCRECT).right - pad;
            c.text(&when, time, MUTED, font, ONE_LINE);
            c.text(bare(&link), RECT { left: 2 * pad + time_width, right: w - pad, ..full }, TEXT, font, ONE_LINE);
            c.blit(d.hDC, r.left, r.top);
        });
        return;
    }

    let disabled = d.itemState & ODS_DISABLED != 0;
    let pressed = d.itemState & ODS_SELECTED != 0;
    let hover = if disabled { 0.0 } else { hover_amount(d.hwndItem) };
    let toggle = toggle_flag(id).is_some() || id == ID_AUTOSTART;
    let bg = if id == ID_PAUSE { hero_color() } else { SURFACE };
    Canvas::with_scratch(d.hDC, w, h, bg, |c| {
        let radius = s(7.0);
        let primary = id == ID_PAUSE && !F_ENABLED.get();
        let (fill, fg) = if disabled {
            (bg, MUTED)
        } else if primary {
            (if pressed { ACCENT_PRESSED } else { mix(ACCENT, FOCUS, hover) }, BG)
        } else if pressed {
            (BUTTON_ACTIVE, TEXT)
        } else {
            (mix(if toggle { bg } else { BUTTON_IDLE }, BUTTON_HOVER, hover), TEXT)
        };
        c.fill(0.0, 0.0, wf, hf, radius, fill);
        if !toggle && !primary {
            c.stroke(0.0, 0.0, wf, hf, radius, 1.0, BORDER);
        }
        if focus {
            c.stroke(0.0, 0.0, wf, hf, radius, s(1.5), if primary { BG } else { FOCUS });
        }
        if toggle {
            let (tw, th) = (s(34.0), s(18.0));
            let (tx, ty) = (wf - tw - s(10.0), (hf - th) / 2.0);
            let (track, knob, offset) = if is_on(id) && !disabled {
                (ACCENT, ACTIVE_PANEL, tw - th)
            } else {
                (TRACK_OFF, if disabled { FAINT } else { KNOB }, 0.0)
            };
            c.fill(tx, ty, tw, th, th / 2.0, track);
            let kd = th - s(6.0);
            c.fill(tx + s(3.0) + offset, ty + s(3.0), kd, kd, kd / 2.0, knob);
            c.text(label(id), RECT { left: s(10.0) as i32, right: (tx - s(8.0)) as i32, ..full }, fg, font, ONE_LINE);
        } else {
            c.text(label(id), full, fg, bold, ONE_LINE | DT_CENTER);
        }
        c.blit(d.hDC, r.left, r.top);
    });
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
        let hover = hover_amount(hw);
        let active = GetCapture() == hw;
        Canvas::with_scratch(cd.hdc, cr.right, cr.bottom, SURFACE, |c| {
            let kr = s(if active { 8.0 } else { 6.0 + hover });
            let pad = kr + s(3.5);
            // cd.rc is the channel: it spans thumb-centre-at-0 to thumb-centre-at-100.
            let x0 = (cd.rc.left as f32).max(pad);
            let x1 = (cd.rc.right as f32).min(cr.right as f32 - pad).max(x0);
            let cx = ((th.left + th.right) as f32 / 2.0).clamp(x0, x1);
            let cy = (th.top + th.bottom) as f32 / 2.0;
            let t = s(4.0);
            c.fill(x0, cy - t / 2.0, x1 - x0, t, t / 2.0, TRACK_OFF);
            c.fill(x0, cy - t / 2.0, cx - x0, t, t / 2.0, if enabled { ACCENT } else { FAINT });
            if focus {
                c.stroke(cx - kr - s(3.0), cy - kr - s(3.0), 2.0 * kr + s(6.0), 2.0 * kr + s(6.0), kr + s(3.0), s(1.5), FOCUS);
            }
            c.fill(cx - kr, cy - kr, 2.0 * kr, 2.0 * kr, kr, if enabled { KNOB } else { FAINT });
            c.blit(cd.hdc, 0, 0);
        });
    }
}

fn hero_color() -> u32 {
    if trimming() { ACTIVE_PANEL } else { PAUSED_PANEL }
}

fn paint_bg(h: HWND, hdc: HDC) {
    unsafe {
        let mut cr: RECT = std::mem::zeroed();
        GetClientRect(h, &mut cr);
        let dpi = GetDpiForWindow(h);
        let sc = |v| scale(v, dpi) as f32;
        let scroll = Scrollbar::read(h);
        Canvas::with_scratch(hdc, cr.right, cr.bottom, BG, |c| {
            for ((x, y, w, h), color, border) in [
                (HERO, hero_color(), if trimming() { 0x285244 } else { 0x50323F }),
                (PREFS, SURFACE, BORDER),
                (LAST, SURFACE, BORDER),
                (STATS, SURFACE, BORDER),
                (HISTORY, SURFACE, BORDER),
            ] {
                c.fill(sc(x), sc(y), sc(w), sc(h), sc(12), color);
                c.stroke(sc(x), sc(y), sc(w), sc(h), sc(12), 1.0, border);
            }
            let signal = if trimming() { ACCENT } else { RED };
            c.fill(sc(34), sc(41), sc(12), sc(12), sc(6), signal);
            for y in [159, 201] {
                c.fill(sc(282), sc(y), sc(244), sc(30), sc(6), LINK_BG);
            }
            c.fill(sc(404), sc(282), 1.0, sc(35), 0.0, BORDER);
            c.fill(sc(32), sc(386), sc(496), 1.0, 0.0, BORDER);
            if let Some(scroll) = scroll {
                let color = if SCROLL_DRAG.get().is_some() { FOCUS } else { MUTED };
                c.fill(sc(SCROLL_X), scroll.thumb_top() as f32, sc(SCROLL_W), scroll.thumb as f32, sc(SCROLL_W) / 2.0, color);
            }
            c.blit(hdc, 0, 0);
        });
    }
}

fn copy_selected_history(h: HWND) {
    let sel = unsafe { SendDlgItemMessageW(h, ID_LIST, LB_GETCURSEL, 0, 0) };
    // LB_ERR is -1, so the usize conversion fails and we skip.
    if let Ok(i) = usize::try_from(sel)
        && let Some(link) = history::with(|items| items.get(i).map(|e| e.link.clone()))
    {
        clipboard::copy(&link);
        COPY_FEEDBACK.set(true);
        set_text(h, ID_HINT, "Copied to clipboard");
        unsafe { SetTimer(h, COPY_TIMER, 1500, None) };
    }
}

thread_local! {
    static HOVER: RefCell<Vec<Hover>> = const { RefCell::new(Vec::new()) };
    static COPY_FEEDBACK: Cell<bool> = const { Cell::new(false) };
    static SCROLL_DRAG: Cell<Option<i32>> = const { Cell::new(None) };
    static WHEEL_REMAINDER: Cell<i32> = const { Cell::new(0) };
}

struct Hover {
    hwnd: HWND,
    start: Instant,
    from: f32,
    hot: bool,
}

impl Hover {
    fn amount(&self) -> f32 {
        let t = (self.start.elapsed().as_secs_f32() / TRANSITION.as_secs_f32()).min(1.0);
        let target = if self.hot { 1.0 } else { 0.0 };
        self.from + (target - self.from) * t
    }
}

fn hover_amount(hwnd: HWND) -> f32 {
    HOVER.with_borrow(|states| states.iter().find(|state| state.hwnd == hwnd).map_or(0.0, Hover::amount))
}

fn animate_controls() -> bool {
    HOVER.with_borrow_mut(|states| {
        let mut running = false;
        states.retain(|state| {
            let moving = state.start.elapsed() < TRANSITION;
            unsafe { InvalidateRect(state.hwnd, null(), 0) };
            running |= moving;
            moving || state.hot
        });
        running
    })
}

fn set_hot(child: HWND, hot: bool) {
    let changed = HOVER.with_borrow_mut(|states| {
        if let Some(state) = states.iter_mut().find(|state| state.hwnd == child) {
            if state.hot == hot {
                return false;
            }
            *state = Hover { hwnd: child, start: Instant::now(), from: state.amount(), hot };
        } else if hot {
            states.push(Hover { hwnd: child, start: Instant::now(), from: 0.0, hot });
        } else {
            return false;
        }
        true
    });
    if !changed {
        return;
    }
    unsafe {
        if hot {
            let mut tracking = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: child,
                dwHoverTime: 0,
            };
            TrackMouseEvent(&mut tracking);
        }
        let mut animate = 0i32;
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&mut animate as *mut i32).cast(), 0);
        if animate != 0 {
            SetTimer(GetParent(child), ANIMATION_TIMER, 16, None);
        } else {
            HOVER.with_borrow_mut(|states| {
                states.retain_mut(|state| {
                    if state.hwnd != child {
                        return true;
                    }
                    state.from = 1.0;
                    state.hot
                });
            });
        }
        InvalidateRect(child, null(), 0);
    }
}

fn update_history_text(h: HWND) {
    let available = !history::with(|items| items.is_empty());
    let enabled = F_HISTORY.get();
    let hint = match (COPY_FEEDBACK.get(), available, enabled) {
        (true, _, _) => "Copied to clipboard",
        (_, true, true) => "Double-click or press Enter to copy",
        (_, true, false) => "Recording off · Double-click or Enter to copy",
        _ => "",
    };
    set_text(h, ID_HINT, hint);
    if !available {
        set_text(h, ID_EMPTY, if enabled { "Cleaned links will appear here." } else { "History recording is off." });
    }
}

struct Scrollbar {
    list: HWND,
    top: i32,
    height: i32,
    thumb: i32,
    index: i32,
    maximum: i32,
}

impl Scrollbar {
    fn read(h: HWND) -> Option<Self> {
        unsafe {
            let list = GetDlgItem(h, ID_LIST);
            let count = SendMessageW(list, LB_GETCOUNT, 0, 0) as i32;
            let maximum = count - VISIBLE_ROWS;
            if maximum <= 0 {
                return None;
            }
            let dpi = GetDpiForWindow(h);
            let top = scale(LIST.1, dpi);
            let height = VISIBLE_ROWS * scale(ROW_H, dpi);
            let thumb = (height * VISIBLE_ROWS / count).max(scale(22, dpi));
            let index = (SendMessageW(list, LB_GETTOPINDEX, 0, 0) as i32).clamp(0, maximum);
            Some(Self { list, top, height, thumb, index, maximum })
        }
    }

    fn thumb_top(&self) -> i32 {
        self.top + (self.height - self.thumb) * self.index / self.maximum
    }

    fn scroll_to(&self, index: i32) {
        unsafe {
            SendMessageW(self.list, LB_SETTOPINDEX, index.clamp(0, self.maximum) as WPARAM, 0);
            invalidate_scrollbar(GetParent(self.list));
        }
    }

    fn drag(&self, y: i32, offset: i32) {
        let range = self.height - self.thumb;
        let distance = (y - offset - self.top).clamp(0, range);
        self.scroll_to((distance * self.maximum + range / 2) / range);
    }
}

fn invalidate_scrollbar(h: HWND) {
    unsafe {
        let dpi = GetDpiForWindow(h);
        let top = scale(LIST.1, dpi);
        let gutter = RECT {
            left: scale(SCROLL_GUTTER, dpi),
            top,
            right: scale(SCROLL_X + SCROLL_W + 8, dpi),
            bottom: top + VISIBLE_ROWS * scale(ROW_H, dpi),
        };
        InvalidateRect(h, &gutter, 0);
    }
}

fn scroll_wheel(h: HWND, wp: WPARAM) {
    let Some(scroll) = Scrollbar::read(h) else { return };
    let delta = (wp >> 16) as i16 as i32 + WHEEL_REMAINDER.get();
    WHEEL_REMAINDER.set(delta % WHEEL_DELTA as i32);
    let steps = delta / WHEEL_DELTA as i32;
    if steps == 0 {
        return;
    }
    let mut lines = 3u32;
    unsafe { SystemParametersInfoW(SPI_GETWHEELSCROLLLINES, 0, (&mut lines as *mut u32).cast(), 0) };
    let lines = if lines == u32::MAX { VISIBLE_ROWS } else { lines.min(scroll.maximum as u32) as i32 };
    scroll.scroll_to(scroll.index - steps * lines);
}

fn mouse_point(lp: LPARAM) -> (i32, i32) {
    (lp as i16 as i32, (lp >> 16) as i16 as i32)
}

unsafe extern "system" fn control_proc(child: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _: usize, _: usize) -> LRESULT {
    unsafe {
        let parent = GetParent(child);
        let id = GetDlgCtrlID(child);
        if msg == WM_NCDESTROY {
            HOVER.with_borrow_mut(|states| states.retain(|state| state.hwnd != child));
            RemoveWindowSubclass(child, Some(control_proc), 0);
        }
        if id == ID_LIST {
            if msg == WM_MOUSEWHEEL {
                scroll_wheel(parent, wp);
                return 0;
            }
            let result = DefSubclassProc(child, msg, wp, lp);
            if matches!(msg, WM_KEYDOWN | WM_CHAR | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_TIMER)
                || (msg == WM_MOUSEMOVE && GetCapture() == child)
            {
                invalidate_scrollbar(parent);
            }
            return result;
        }
        match msg {
            WM_MOUSEMOVE => set_hot(child, true),
            WM_MOUSELEAVE | WM_ENABLE => set_hot(child, false),
            _ => {}
        }
        let result = DefSubclassProc(child, msg, wp, lp);
        if id == ID_VOLUME && matches!(msg, WM_LBUTTONDOWN | WM_LBUTTONUP | WM_CAPTURECHANGED | WM_SETFOCUS | WM_KILLFOCUS) {
            InvalidateRect(child, null(), 0);
        }
        result
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
                        sync_toggle_text(h, id);
                        if id == ID_HISTORY {
                            update_history_text(h);
                        }
                        InvalidateRect(GetDlgItem(h, id), null(), 0);
                        if id == ID_SOUND {
                            let vol = GetDlgItem(h, ID_VOLUME);
                            EnableWindow(vol, on as i32);
                            InvalidateRect(vol, null(), 0);
                            InvalidateRect(GetDlgItem(h, ID_VOL_VALUE), null(), 1);
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
                    ID_LIST if code == LBN_DBLCLK => copy_selected_history(h),
                    IDOK_ => {
                        if GetFocus() == GetDlgItem(h, ID_LIST) {
                            copy_selected_history(h);
                        }
                    }
                    IDCANCEL_ => {
                        DestroyWindow(h); // Esc closes, via IsDialogMessage in the main loop
                    }
                    _ => {}
                }
                0
            }
            WM_VKEYTOITEM => {
                let vk = (wp & 0xFFFF) as u16;
                if vk == VK_C && (GetKeyState(VK_CONTROL as i32) as u16 & 0x8000) != 0 {
                    copy_selected_history(h);
                    return -2; // handled; suppress listbox default character navigation
                }
                -1 // default listbox keyboard handling
            }
            WM_HSCROLL => {
                let ctl = lp as HWND;
                if GetDlgCtrlID(ctl) == ID_VOLUME {
                    // The trackbar only invalidates the old and new thumb rects; the
                    // accent fill between them is ours, so repaint the whole control.
                    InvalidateRect(ctl, null(), 0);
                    let value = SendMessageW(ctl, TBM_GETPOS, 0, 0) as u32;
                    show_volume(h, value);
                    // Save and preview on release (or keyup), not on every drag pixel.
                    if (wp & 0xFFFF) as u32 == TB_ENDTRACK {
                        settings::set_volume(value);
                        sound::play();
                    }
                }
                0
            }
            WM_NOTIFY => {
                let Some(hdr) = (lp as *const NMHDR).as_ref() else { return 0 };
                if hdr.code == NM_CUSTOMDRAW && hdr.idFrom == ID_VOLUME as usize {
                    let Some(cd) = (lp as *const NMCUSTOMDRAW).as_ref() else { return 0 };
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
                if let Some(d) = (lp as *const DRAWITEMSTRUCT).as_ref() {
                    draw_item(d);
                    1
                } else {
                    0
                }
            }
            WM_MEASUREITEM => {
                if let Some(m) = (lp as *mut MEASUREITEMSTRUCT).as_mut() {
                    m.itemHeight = scale(ROW_H, GetDpiForWindow(h)) as u32;
                    1
                } else {
                    0
                }
            }
            WM_TIMER if wp == COPY_TIMER => {
                KillTimer(h, COPY_TIMER);
                COPY_FEEDBACK.set(false);
                update_history_text(h);
                0
            }
            WM_TIMER if wp == ANIMATION_TIMER => {
                if !animate_controls() {
                    KillTimer(h, ANIMATION_TIMER);
                }
                0
            }
            WM_MOUSEWHEEL => {
                let (x, y) = mouse_point(lp);
                let mut point = POINT { x, y };
                ScreenToClient(h, &mut point);
                let dpi = GetDpiForWindow(h);
                let (x, y, width, height) = HISTORY;
                if (scale(x, dpi)..scale(x + width, dpi)).contains(&point.x)
                    && (scale(y, dpi)..scale(y + height, dpi)).contains(&point.y)
                {
                    scroll_wheel(h, wp);
                }
                0
            }
            WM_LBUTTONDOWN => {
                let (x, y) = mouse_point(lp);
                let dpi = GetDpiForWindow(h);
                if x >= scale(SCROLL_GUTTER, dpi)
                    && x < scale(SCROLL_X + SCROLL_W + 8, dpi)
                    && let Some(scroll) = Scrollbar::read(h)
                    && (scroll.top..scroll.top + scroll.height).contains(&y)
                {
                    let thumb_top = scroll.thumb_top();
                    let offset = if (thumb_top..thumb_top + scroll.thumb).contains(&y) {
                        y - thumb_top
                    } else {
                        scroll.thumb / 2
                    };
                    SetFocus(scroll.list);
                    SCROLL_DRAG.set(Some(offset));
                    SetCapture(h);
                    scroll.drag(y, offset);
                }
                0
            }
            WM_MOUSEMOVE if GetCapture() == h => {
                if let Some(offset) = SCROLL_DRAG.get()
                    && let Some(scroll) = Scrollbar::read(h)
                {
                    scroll.drag(mouse_point(lp).1, offset);
                }
                0
            }
            WM_LBUTTONUP if GetCapture() == h => {
                ReleaseCapture();
                0
            }
            WM_CAPTURECHANGED => {
                SCROLL_DRAG.set(None);
                invalidate_scrollbar(h);
                0
            }
            WM_REFRESH => {
                refresh_now(h);
                0
            }
            WM_CTLCOLORSTATIC => {
                let hdc = wp as HDC;
                let id = GetDlgCtrlID(lp as HWND);
                let (slot, bg) = match id {
                    ID_STATUS | ID_STATUS_HINT if trimming() => (&BRUSH_ACTIVE, ACTIVE_PANEL),
                    ID_STATUS | ID_STATUS_HINT => (&BRUSH_PAUSED, PAUSED_PANEL),
                    ID_FROM | ID_TO | ID_ORIG | ID_SHORT => (&BRUSH_LINK, LINK_BG),
                    _ => (&BRUSH_SURFACE, SURFACE),
                };
                let fg = if id == ID_SHORT {
                    FOCUS
                } else if MUTED_IDS.contains(&id) || (id == ID_VOL_VALUE && !F_SOUND.get()) {
                    MUTED
                } else {
                    TEXT
                };
                SetBkColor(hdc, paint::cref(bg));
                SetTextColor(hdc, paint::cref(fg));
                brush(slot, bg) as LRESULT
            }
            WM_CTLCOLORBTN => brush(&BRUSH_SURFACE, SURFACE) as LRESULT,
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
                // layout, and size to the exact outer rect computed for the new DPI.
                let (ww, wh) = apply_dpi(h, (wp & 0xFFFF) as u32);
                if let Some(r) = (lp as *const RECT).as_ref() {
                    SetWindowPos(h, null_mut(), r.left, r.top, ww, wh, SWP_NOZORDER | SWP_NOACTIVATE);
                }
                0
            }
            WM_CLOSE => {
                DestroyWindow(h);
                0
            }
            WM_DESTROY => {
                // Only this window goes away. No PostQuitMessage: the app keeps running in the tray.
                WND.store(null_mut(), Relaxed);
                LAST_HIST_GEN.store(0, Relaxed);
                COPY_FEEDBACK.set(false);
                SCROLL_DRAG.set(None);
                WHEEL_REMAINDER.set(0);
                paint::release_scratch();
                for slot in [&FONT, &FONT_BOLD, &FONT_TITLE] {
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
