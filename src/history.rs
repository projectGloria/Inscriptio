//! Recent trimmed links, newest first, capped at 50.
//! Stored in %LOCALAPPDATA%\Inscriptio\history.tsv as `MM-DD HH:MM<TAB>link` lines.
//!
//! Only the *trimmed* link is kept: the original's tracking junk is exactly
//! what this app exists to throw away. The callers in clipboard.rs decide
//! what's safe to record (no incognito copies, no token-bearing links).

use std::cell::RefCell;
use std::path::PathBuf;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;

const CAP: usize = 50;

pub struct Entry {
    pub when: String,
    pub link: String,
}

// All UI and clipboard work runs on the single message-loop thread, so a
// thread_local RefCell is enough. No Mutex: there's nobody to contend with.
// Option = lazily loaded: no disk I/O until history is first needed.
thread_local! {
    static ITEMS: RefCell<Option<Vec<Entry>>> = const { RefCell::new(None) };
}

fn path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Inscriptio").join("history.tsv"))
}

fn load() -> Vec<Entry> {
    let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else { return Vec::new() };
    text.lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(w, l)| Entry { when: w.to_owned(), link: l.to_owned() })
        .take(CAP)
        .collect()
}

fn save(items: &[Entry]) {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let body: String = items.iter().map(|e| format!("{}\t{}\n", e.when, e.link)).collect();
    // Best-effort: a failed write must never break trimming.
    let _ = std::fs::write(p, body);
}

/// Borrow the list (loading it on first use).
pub fn with<R>(f: impl FnOnce(&[Entry]) -> R) -> R {
    ITEMS.with_borrow_mut(|slot| f(slot.get_or_insert_with(load)))
}

pub fn add(link: &str) {
    ITEMS.with_borrow_mut(|slot| {
        let items = slot.get_or_insert_with(load);
        // Copying the same link twice shouldn't produce duplicate rows: move it to the top.
        items.retain(|e| e.link != link);
        items.insert(0, Entry { when: now(), link: link.to_owned() });
        items.truncate(CAP);
        save(items);
    });
}

pub fn clear() {
    ITEMS.with_borrow_mut(|slot| *slot = Some(Vec::new()));
    if let Some(p) = path() {
        let _ = std::fs::remove_file(p);
    }
}

fn now() -> String {
    // Out-param API: SYSTEMTIME is plain data, so zeroed() is a valid initial value.
    let mut t = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut t) };
    format!("{:02}-{:02} {:02}:{:02}", t.wMonth, t.wDay, t.wHour, t.wMinute)
}
