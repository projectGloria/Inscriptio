// GUI subsystem: no console window flashes on launch or at autostart.
#![windows_subsystem = "windows"]

mod clipboard;
mod history;
mod paint;
mod rules;
mod settings;
mod sound;
mod tray;
mod ui;
mod url;

use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::DataExchange::{AddClipboardFormatListener, RemoveClipboardFormatListener};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use windows_sys::core::w;

use clipboard::trace;

/// Explorer broadcasts this after it restarts. Re-adding the icon on it keeps
/// the tray entry from silently vanishing after an explorer.exe crash.
/// Posted by a second `--settings` launch (see main) to the running instance.
const WM_SHOW_SETTINGS: u32 = WM_APP + 3;

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

fn main() {
    unsafe {
        // Single instance: two listeners would each rewrite the other's output.
        // The handle is intentionally leaked: the OS releases it at process exit.
        let _mutex = CreateMutexW(null(), 0, w!("Local\\Inscriptio.SingleInstance"));
        let already_running = GetLastError() == ERROR_ALREADY_EXISTS;
        if std::env::args().any(|a| a == "--trace") {
            clipboard::TRACE.store(true, Relaxed);
        }
        let open_settings = std::env::args().any(|a| a == "--settings");
        if already_running {
            // Easy to trip over: `--trace` while the tray copy runs does nothing,
            // because the first instance owns the listener.
            trace!("another instance is already running, exiting (quit the tray one first)");
            if open_settings {
                // The Start-menu shortcut runs `--settings`: hand the request to
                // the running copy instead of silently exiting.
                let main = FindWindowW(w!("InscriptioWnd"), null());
                if !main.is_null() {
                    // We were just launched by the user, so *we* hold the foreground
                    // right. Pass it on, or Windows would only flash the taskbar.
                    let mut pid = 0;
                    GetWindowThreadProcessId(main, &mut pid);
                    AllowSetForegroundWindow(pid);
                    PostMessageW(main, WM_SHOW_SETTINGS, 0, 0);
                }
            }
            return;
        }
        trace!("--- started (log: {})", std::env::temp_dir().join("inscriptio.log").display());

        settings::load();
        clipboard::init();
        ui::dark_menus(); // before the first menu exists: menus cache their theme
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Relaxed);

        let hinst = GetModuleHandleW(null());
        let class = w!("InscriptioWnd");
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst, lpszClassName: class, ..std::mem::zeroed() };
        RegisterClassW(&wc);

        // A hidden top-level window (never shown), deliberately *not* HWND_MESSAGE:
        // message-only windows don't receive broadcasts like TaskbarCreated.
        // WS_EX_TOOLWINDOW keeps it out of Alt+Tab even if something shows it.
        let hwnd = CreateWindowExW(WS_EX_TOOLWINDOW, class, w!("Inscriptio"), WS_POPUP, 0, 0, 0, 0, null_mut(), null_mut(), hinst, null());
        if hwnd.is_null() {
            return;
        }
        tray::MAIN.store(hwnd, Relaxed);

        // Event-driven: the thread sleeps in GetMessageW (0% CPU) until the
        // clipboard actually changes. No polling, no timers.
        if AddClipboardFormatListener(hwnd) == 0 {
            trace!("AddClipboardFormatListener FAILED, error {}", GetLastError());
        } else {
            trace!("listening for clipboard changes");
        }
        tray::add(hwnd);
        if open_settings {
            ui::show(); // open the window straight away (handy for a Start-menu shortcut)
        }

        let mut msg: MSG = std::mem::zeroed();
        // > 0, not != 0: GetMessageW returns -1 on error, and that must not loop forever.
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            // Gives the settings window dialog keyboard behaviour (Tab between
            // controls, Space toggles, Esc closes) without being a real dialog.
            let ui = ui::hwnd();
            if !ui.is_null() && IsDialogMessageW(ui, &msg) != 0 {
                continue;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CLIPBOARDUPDATE => {
                if !settings::F_ENABLED.get() {
                    trace!("clipboard changed, but paused");
                } else {
                    clipboard::on_update(hwnd);
                }
                0
            }
            WM_SHOW_SETTINGS => {
                ui::show();
                0
            }
            tray::WM_TRAY => {
                tray::on_tray(hwnd, lp);
                0
            }
            WM_DESTROY => {
                let ui = ui::hwnd();
                if !ui.is_null() {
                    DestroyWindow(ui);
                }
                RemoveClipboardFormatListener(hwnd);
                tray::remove(hwnd);
                PostQuitMessage(0);
                0
            }
            // Registered messages are runtime values, so they can't be match patterns.
            m if m != 0 && m == TASKBAR_CREATED.load(Relaxed) => {
                tray::add(hwnd);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
