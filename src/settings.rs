//! Persistent settings + stats in HKCU\Software\Inscriptio.
//!
//! Why the registry and not a config file: no parser, no path juggling, atomic
//! per-value writes, and it's where Windows apps keep this kind of thing.
//! Values are mirrored in atomics, so the hot path (every clipboard change)
//! never touches the registry for reads.

use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows_sys::Win32::System::Registry::*;
use windows_sys::core::w;

pub static ENABLED: AtomicBool = AtomicBool::new(true);
pub static KEEP_TIME: AtomicBool = AtomicBool::new(true);
pub static SOUND: AtomicBool = AtomicBool::new(true);
pub static HISTORY: AtomicBool = AtomicBool::new(true);
/// Chime volume, 0–100. 0 is effectively mute, but the Sound switch stays the real off switch.
pub static VOLUME: AtomicU32 = AtomicU32::new(60);
pub static TRIMMED: AtomicU32 = AtomicU32::new(0);
pub static SAVED_BYTES: AtomicU64 = AtomicU64::new(0);

/// Typed handle to one boolean setting: its atomic plus its registry value name.
#[derive(Clone, Copy)]
pub struct Flag(pub &'static AtomicBool, *const u16);
// The raw pointer is a 'static wide-string literal from w!(), so sharing it is fine.
unsafe impl Sync for Flag {}

pub const F_ENABLED: Flag = Flag(&ENABLED, w!("Enabled"));
pub const F_KEEP_TIME: Flag = Flag(&KEEP_TIME, w!("KeepTimestamps"));
pub const F_SOUND: Flag = Flag(&SOUND, w!("Sound"));
pub const F_HISTORY: Flag = Flag(&HISTORY, w!("History"));

impl Flag {
    #[inline]
    pub fn get(self) -> bool {
        self.0.load(Relaxed)
    }
    pub fn set(self, on: bool) {
        self.0.store(on, Relaxed);
        write_u32(self.1, on as u32);
    }
}

const APP_KEY: *const u16 = w!("Software\\Inscriptio");
const RUN_KEY: *const u16 = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VAL: *const u16 = w!("Inscriptio");

/// RAII registry key so every branch closes the handle.
struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

fn app_key() -> Option<Key> {
    let mut k: HKEY = null_mut();
    // RegCreateKeyExW opens the key if it exists, so the first run needs no special case.
    let rc = unsafe {
        RegCreateKeyExW(HKEY_CURRENT_USER, APP_KEY, 0, null(), 0, KEY_READ | KEY_WRITE, null(), &mut k, null_mut())
    };
    (rc == 0).then(|| Key(k))
}

fn read<const N: usize>(k: &Key, name: *const u16) -> Option<[u8; N]> {
    let mut buf = [0u8; N];
    let mut len = N as u32;
    let rc = unsafe { RegQueryValueExW(k.0, name, null(), null_mut(), buf.as_mut_ptr(), &mut len) };
    (rc == 0 && len as usize == N).then_some(buf)
}

fn write_u32(name: *const u16, v: u32) {
    if let Some(k) = app_key() {
        unsafe { RegSetValueExW(k.0, name, 0, REG_DWORD, v.to_le_bytes().as_ptr(), 4) };
    }
}

fn write_u64(name: *const u16, v: u64) {
    if let Some(k) = app_key() {
        unsafe { RegSetValueExW(k.0, name, 0, REG_QWORD, v.to_le_bytes().as_ptr(), 8) };
    }
}

/// Called once at startup. Missing values keep the compiled-in defaults.
pub fn load() {
    let Some(k) = app_key() else { return };
    for f in [F_ENABLED, F_KEEP_TIME, F_SOUND, F_HISTORY] {
        if let Some(b) = read::<4>(&k, f.1) {
            f.0.store(u32::from_le_bytes(b) != 0, Relaxed);
        }
    }
    if let Some(b) = read::<4>(&k, w!("Volume")) {
        VOLUME.store(u32::from_le_bytes(b).min(100), Relaxed);
    }
    if let Some(b) = read::<4>(&k, w!("Trimmed")) {
        TRIMMED.store(u32::from_le_bytes(b), Relaxed);
    }
    if let Some(b) = read::<8>(&k, w!("SavedBytes")) {
        SAVED_BYTES.store(u64::from_le_bytes(b), Relaxed);
    }
}

pub fn set_volume(v: u32) {
    let v = v.min(100);
    VOLUME.store(v, Relaxed);
    write_u32(w!("Volume"), v);
}

/// One trim happened: bump the counters and persist them. Two tiny registry
/// writes per *trim* (not per clipboard change), so the cost is negligible.
pub fn record_trim(saved: usize) {
    let n = TRIMMED.fetch_add(1, Relaxed) + 1;
    let b = SAVED_BYTES.fetch_add(saved as u64, Relaxed) + saved as u64;
    write_u32(w!("Trimmed"), n);
    write_u64(w!("SavedBytes"), b);
}

// ---------------------------------------------------------------- autostart

fn run_key(access: u32) -> Option<Key> {
    let mut k: HKEY = null_mut();
    (unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, 0, access, &mut k) } == 0).then(|| Key(k))
}

pub fn autostart_enabled() -> bool {
    run_key(KEY_QUERY_VALUE)
        .is_some_and(|k| unsafe { RegQueryValueExW(k.0, RUN_VAL, null(), null_mut(), null_mut(), null_mut()) } == 0)
}

pub fn set_autostart(on: bool) {
    let Some(k) = run_key(KEY_SET_VALUE) else { return };
    unsafe {
        if !on {
            RegDeleteValueW(k.0, RUN_VAL);
            return;
        }
        let mut buf = [0u16; 1024];
        let n = GetModuleFileNameW(null_mut(), buf.as_mut_ptr(), buf.len() as u32) as usize;
        if n == 0 || n >= buf.len() {
            return; // truncated path: writing it would register a broken command
        }
        // Quote the path: "C:\Program Files\…" would otherwise split at the space.
        let cmd: Vec<u16> = std::iter::once(b'"' as u16)
            .chain(buf[..n].iter().copied())
            .chain([b'"' as u16, 0])
            .collect();
        RegSetValueExW(k.0, RUN_VAL, 0, REG_SZ, cmd.as_ptr().cast(), (cmd.len() * 2) as u32);
    }
}
