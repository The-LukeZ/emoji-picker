#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![deny(unsafe_op_in_unsafe_fn)]
//! Emoji picker: tray app with a global hotkey that opens a picker window.

/// Debug builds log to stderr; release builds stay quiet.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        if cfg!(debug_assertions) {
            eprintln!($($arg)*);
        }
    };
}

mod assets;
mod win;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use emoji_core::config::Config;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, LPARAM, WPARAM};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, PostMessageW, HWND_MESSAGE};

use win::service::{spawn_service, ServiceCmd, UiEvent, SERVICE_CLASS, WM_OPEN_SETTINGS};

/// True when this is the first instance. The mutex handle is kept until the process exits.
fn claim_single_instance() -> bool {
    // SAFETY: named mutex creation with a static name.
    let created = unsafe { CreateMutexW(None, false, w!(r"Local\emoji-picker-instance")) };
    // SAFETY: reads this thread's last error, set by the call above.
    let last = unsafe { GetLastError() };
    created.is_err() || last != ERROR_ALREADY_EXISTS
}

/// Asks the running instance to open its settings window.
fn signal_first_instance() {
    // The first instance may still be starting: give it up to two seconds.
    for _ in 0..20 {
        // SAFETY: lookup of a message-only window by class name.
        let found =
            unsafe { FindWindowExW(Some(HWND_MESSAGE), None, SERVICE_CLASS, PCWSTR::null()) };
        if let Ok(hwnd) = found {
            // SAFETY: posting a private message to the other instance's window.
            let _ = unsafe { PostMessageW(Some(hwnd), WM_OPEN_SETTINGS, WPARAM(0), LPARAM(0)) };
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `%APPDATA%\emoji-picker`; debug builds honour `EMOJI_PICKER_CONFIG_DIR` for testing.
fn config_dir() -> PathBuf {
    if cfg!(debug_assertions) {
        if let Some(dir) = std::env::var_os("EMOJI_PICKER_CONFIG_DIR") {
            return PathBuf::from(dir);
        }
    }
    let appdata = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_default();
    appdata.join("emoji-picker")
}

/// Runs on the Slint event loop thread.
fn on_ui_event(event: UiEvent) {
    let private_kb = win::sys::private_bytes() / 1024;
    match event {
        UiEvent::OpenPicker {
            target,
            caret,
            cursor,
            at,
        } => {
            // The picker window comes with Task 12; for now the request is only logged.
            log!(
                "ui: OpenPicker target={target:#x} caret={caret:?} cursor={cursor:?} after {:?} (private {private_kb} KB)",
                at.elapsed()
            );
        }
        UiEvent::Quit => {
            log!("ui: Quit (private {private_kb} KB)");
            let _ = slint::quit_event_loop();
        }
        // The settings and about windows come with Task 14.
        other => log!("ui: {other:?} (private {private_kb} KB)"),
    }
}

fn main() {
    if !claim_single_instance() {
        log!("another instance is running: asking it to open settings");
        signal_first_instance();
        return;
    }

    let config_path = config_dir().join("config.toml");
    let (config, warning) = Config::load(&config_path);
    log!("config {}: {config:?}", config_path.display());

    let service = spawn_service(Arc::new(config), |event| {
        let _ = slint::invoke_from_event_loop(move || on_ui_event(event));
    });
    if let Some(text) = warning {
        service.send(ServiceCmd::Balloon {
            title: "Emoji picker settings".into(),
            text,
        });
    }

    if let Err(e) = slint::run_event_loop_until_quit() {
        log!("event loop failed: {e}");
    }
    service.shutdown();
}
