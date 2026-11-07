//! Clipboard read → shorten → write, with guards that keep us from looping
//! on our own writes and from touching password-manager secrets.

use std::cell::RefCell;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use windows_sys::Win32::Foundation::{GlobalFree, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardFormatNameW,
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
    SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::System::Threading::Sleep;
use windows_sys::core::w;

use crate::settings::{self, F_HISTORY, F_KEEP_TIME, F_SOUND};
use crate::{history, rules, sound, tray, ui};

/// Set by `--trace`. When it's off, every trace!() is one relaxed load and a
/// not-taken branch: format_args! is never evaluated.
pub static TRACE: AtomicBool = AtomicBool::new(false);

macro_rules! trace {
    ($($a:tt)*) => {
        if $crate::clipboard::TRACE.load(std::sync::atomic::Ordering::Relaxed) {
            $crate::clipboard::log(format_args!($($a)*));
        }
    };
}
pub(crate) use trace;

/// Appends to %TEMP%\inscriptio.log. It opens per line on purpose: no handle
/// stays alive, and trace mode isn't a hot path.
pub fn log(args: fmt::Arguments) {
    use std::io::Write;
    let path = std::env::temp_dir().join("inscriptio.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{args}");
    }
}

/// Sequence number produced by *our* last write. WM_CLIPBOARDUPDATE fires for
/// our own SetClipboardData too. Without this guard we'd re-read our output
/// and could ping-pong with another clipboard tool.
static OWN_SEQ: AtomicU32 = AtomicU32::new(0);

// Registered format IDs, resolved once at startup (0 = not registered).
static FMT_EXCLUDE: AtomicU32 = AtomicU32::new(0);
static FMT_IGNORE: AtomicU32 = AtomicU32::new(0);
static FMT_HISTORY: AtomicU32 = AtomicU32::new(0);
static FMT_CLOUD: AtomicU32 = AtomicU32::new(0);

const CF_TEXT_W: u32 = CF_UNICODETEXT as u32;
/// Same as rules::MAX_LEN, in UTF-16 units. Bigger payloads aren't read at all.
const MAX_UNITS: usize = 4096;

/// The soft privacy markers a copy carried. We re-apply them on every write
/// derived from that copy (the trim *and* its undo).
#[derive(Clone, Copy, Default)]
struct Markers {
    history: Option<u32>,
    cloud: Option<u32>,
}
impl Markers {
    /// 0 = "don't persist". Incognito/private windows set it on every copy.
    fn private(self) -> bool {
        self.history == Some(0) || self.cloud == Some(0)
    }
}

/// Last trim, for Undo and the settings window's preview.
pub struct Last {
    pub original: String,
    pub short: String,
    pub undone: bool,
    markers: Markers,
}

// Single UI thread, so thread_local + RefCell (see history.rs for the same reasoning).
thread_local! {
    static LAST: RefCell<Option<Last>> = const { RefCell::new(None) };
}

pub fn with_last<R>(f: impl FnOnce(Option<&Last>) -> R) -> R {
    LAST.with_borrow(|l| f(l.as_ref()))
}

pub fn can_undo() -> bool {
    with_last(|l| l.is_some_and(|l| !l.undone))
}

pub fn init() {
    unsafe {
        // Hard "monitors keep out" markers: password managers (KeePassXC,
        // Bitwarden) and other clipboard tools. We never even read these copies.
        FMT_EXCLUDE.store(RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")), Relaxed);
        FMT_IGNORE.store(RegisterClipboardFormatW(w!("Clipboard Viewer Ignore")), Relaxed);
        // Soft "don't persist" markers: we may rewrite, but must carry them over.
        FMT_HISTORY.store(RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")), Relaxed);
        FMT_CLOUD.store(RegisterClipboardFormatW(w!("CanUploadToCloudClipboard")), Relaxed);
    }
}

/// RAII close. Every early return still releases the clipboard. Holding it
/// open blocks copy and paste system-wide.
struct Open;
impl Drop for Open {
    fn drop(&mut self) {
        unsafe { CloseClipboard() };
    }
}

fn open(hwnd: HWND) -> Option<Open> {
    // Whoever just wrote may still hold the clipboard (some apps re-open it to
    // add more formats). Retry briefly instead of dropping the event.
    for _ in 0..5 {
        if unsafe { OpenClipboard(hwnd) } != 0 {
            return Some(Open);
        }
        unsafe { Sleep(10) };
    }
    None
}

#[inline]
fn has(fmt: u32) -> bool {
    fmt != 0 && unsafe { IsClipboardFormatAvailable(fmt) } != 0
}

pub fn on_update(hwnd: HWND) {
    let seq = unsafe { GetClipboardSequenceNumber() };
    if seq == OWN_SEQ.load(Relaxed) {
        trace!("#{seq} own write, ignored");
        return;
    }
    let (original, short, markers) = match process(hwnd) {
        Ok(t) => t,
        Err(why) => {
            trace!("#{seq} skipped: {why}");
            return;
        }
    };
    // process() has returned, so the clipboard is closed and the sequence
    // number reflects our write. Everything below happens *after* we released
    // the clipboard: other apps are never blocked by our bookkeeping.
    OWN_SEQ.store(unsafe { GetClipboardSequenceNumber() }, Relaxed);
    trace!("#{seq} REWROTE -> {short}");

    settings::record_trim(original.len().saturating_sub(short.len()));
    if F_SOUND.get() {
        sound::play();
    }
    if F_HISTORY.get() {
        if markers.private() {
            trace!("   not recorded: private/incognito copy");
        } else if rules::is_sensitive(&short) {
            trace!("   not recorded: looks like it carries a credential");
        } else {
            history::add(&short);
        }
    }
    LAST.set(Some(Last { original, short, undone: false, markers }));
    ui::refresh();
}

fn process(hwnd: HWND) -> Result<(String, String, Markers), &'static str> {
    // Cheapest checks first. IsClipboardFormatAvailable doesn't need the clipboard open.
    if !has(CF_TEXT_W) {
        return Err("no unicode text on clipboard");
    }
    if has(FMT_EXCLUDE.load(Relaxed)) || has(FMT_IGNORE.load(Relaxed)) {
        return Err("source marked it monitor-excluded (password manager?)");
    }

    let _guard = open(hwnd).ok_or("OpenClipboard kept failing (another app holds it)")?;
    trace!("   formats: {}", FormatList);

    // Snapshot the soft markers *before* EmptyClipboard wipes them.
    let markers = Markers { history: read_dword(FMT_HISTORY.load(Relaxed)), cloud: read_dword(FMT_CLOUD.load(Relaxed)) };

    let text = read_text().ok_or("text unreadable or longer than 4096 chars")?;
    let opts = rules::Opts { keep_time: F_KEEP_TIME.get() };
    let short = rules::shorten(&text, opts).ok_or("not a single http(s) link, or nothing to trim")?;
    if !replace(&short, markers) {
        return Err("SetClipboardData failed");
    }
    Ok((text.trim().to_owned(), short, markers))
}

/// Put the original link back (same privacy markers it arrived with).
pub fn undo() {
    let Some((original, markers)) = with_last(|l| l.filter(|l| !l.undone).map(|l| (l.original.clone(), l.markers)))
    else {
        return;
    };
    if write_own(&original, markers) {
        LAST.with_borrow_mut(|l| {
            if let Some(l) = l {
                l.undone = true;
            }
        });
        ui::refresh();
    }
}

/// Copy a link from the history list.
pub fn copy(text: &str) {
    write_own(text, Markers::default());
}

/// Replace the clipboard from our side, flagged as our own write so the
/// listener doesn't immediately re-trim it (an undo would be pointless otherwise).
fn write_own(text: &str, markers: Markers) -> bool {
    let ok = {
        let Some(_g) = open(tray::main_hwnd()) else { return false };
        replace(text, markers)
    }; // closed here, before we read the final sequence number
    if ok {
        OWN_SEQ.store(unsafe { GetClipboardSequenceNumber() }, Relaxed);
    }
    ok
}

/// Inside an open clipboard session: wipe it and write text plus markers.
fn replace(text: &str, m: Markers) -> bool {
    // UTF-16LE bytes plus a NUL terminator. x86/ARM Windows is always little-endian.
    let bytes: Vec<u8> = text.encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect();
    // EmptyClipboard makes the window passed to OpenClipboard the owner. It
    // also drops the source's HTML/RTF formats, so rich pastes become plain links.
    unsafe { EmptyClipboard() };
    if !put(CF_TEXT_W, &bytes) {
        return false;
    }
    // Re-assert the source's privacy intent. Otherwise a link copied in
    // incognito would slip into Win+V history the moment we rewrote it.
    if let Some(v) = m.history {
        put(FMT_HISTORY.load(Relaxed), &v.to_le_bytes());
    }
    if let Some(v) = m.cloud {
        put(FMT_CLOUD.load(Relaxed), &v.to_le_bytes());
    }
    true
}

fn read_dword(fmt: u32) -> Option<u32> {
    if !has(fmt) {
        return None;
    }
    unsafe {
        let h = GetClipboardData(fmt);
        // Marker present but malformed: treat it as 0 (the restrictive value).
        if h.is_null() || GlobalSize(h) < 4 {
            return Some(0);
        }
        let p = GlobalLock(h) as *const u32;
        if p.is_null() {
            return Some(0);
        }
        let v = p.read_unaligned();
        GlobalUnlock(h);
        Some(v)
    }
}

fn read_text() -> Option<String> {
    unsafe {
        let h = GetClipboardData(CF_TEXT_W);
        if h.is_null() {
            return None;
        }
        // GlobalSize bounds the read. Never trust the NUL alone, because a
        // malformed producer could hand us an unterminated buffer.
        let cap = GlobalSize(h) / 2;
        let p = GlobalLock(h) as *const u16;
        if p.is_null() {
            return None;
        }
        let buf = std::slice::from_raw_parts(p, cap.min(MAX_UNITS + 1));
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let out = (len <= MAX_UNITS).then(|| String::from_utf16(&buf[..len]).ok()).flatten();
        GlobalUnlock(h);
        out
    }
}

/// Copies `bytes` into a fresh HGLOBAL and hands it to the clipboard.
/// The caller must already have called EmptyClipboard in this open session.
fn put(fmt: u32, bytes: &[u8]) -> bool {
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
        if h.is_null() {
            return false;
        }
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            GlobalFree(h);
            return false;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        GlobalUnlock(h);
        if SetClipboardData(fmt, h).is_null() {
            GlobalFree(h); // only free on failure. On success the OS owns it.
            return false;
        }
        true
    }
}

/// Lazily formats the open clipboard's format list. It runs only when trace!()
/// actually formats it, so normal operation never enumerates.
struct FormatList;
impl fmt::Display for FormatList {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut id = 0;
        loop {
            id = unsafe { EnumClipboardFormats(id) };
            if id == 0 {
                return Ok(());
            }
            let mut buf = [0u16; 128];
            // Registered formats have names. Standard ones (CF_*) return 0, so print the number.
            let n = unsafe { GetClipboardFormatNameW(id, buf.as_mut_ptr(), buf.len() as i32) } as usize;
            if n > 0 {
                write!(f, "{} ", String::from_utf16_lossy(&buf[..n]))?;
            } else {
                write!(f, "#{id} ")?;
            }
        }
    }
}
