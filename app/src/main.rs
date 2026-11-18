#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![deny(unsafe_op_in_unsafe_fn)]
//! Emoji picker: tray app with a global hotkey that opens a picker window.

/// Debug builds (and release builds with the `diag` feature) log to stderr; release builds
/// stay quiet.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        if cfg!(any(debug_assertions, feature = "diag")) {
            eprintln!($($arg)*);
        }
    };
}

mod assets;
mod picker;
mod win;

/// Generated Slint components (root `app/ui/app.slint`).
#[allow(dead_code)]
mod ui {
    slint::include_modules!();
}

use std::cell::OnceCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use emoji_core::config::Config;
use emoji_core::recents::Recents;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, LPARAM, WPARAM};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, PostMessageW, HWND_MESSAGE};

use picker::{OpenRequest, Picker};
use win::service::{spawn_service, ServiceCmd, UiEvent, SERVICE_CLASS, WM_OPEN_SETTINGS};

thread_local! {
    /// The picker, created once on the UI thread before the event loop runs.
    static PICKER: OnceCell<Picker> = const { OnceCell::new() };
}

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

/// `%APPDATA%\emoji-picker`; debug and `diag` builds honour `EMOJI_PICKER_CONFIG_DIR` for
/// testing.
pub(crate) fn config_dir() -> PathBuf {
    if cfg!(any(debug_assertions, feature = "diag")) {
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
            log!(
                "ui: OpenPicker target={target:#x} caret={caret:?} cursor={cursor:?} after {:?} (private {private_kb} KB)",
                at.elapsed()
            );
            PICKER.with(|p| {
                if let Some(p) = p.get() {
                    p.open(OpenRequest {
                        target,
                        caret,
                        cursor,
                        at,
                    });
                }
            });
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
    let config = Arc::new(config);

    // The backend must exist before the service starts: until then
    // `invoke_from_event_loop` fails with `NoEventLoopProvider` and early events are lost.
    if let Err(e) = slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .with_winit_window_attributes_hook(picker::window_attributes)
        .select()
    {
        log!("selecting the Slint backend failed: {e}");
        return;
    }

    let recents = if config.recents_enabled {
        Recents::load(&config_dir().join("recents.txt"))
    } else {
        Recents::default()
    };
    let picker = Picker::new(config.clone(), recents, |req| {
        // Typing into the target window comes with Task 14.
        log!("ui: insert {req:?}");
    });
    #[cfg(any(debug_assertions, feature = "diag"))]
    if let Some(cycles) = std::env::var("EMOJI_PICKER_SELFTEST")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    {
        picker.selftest(cycles);
    }
    PICKER.with(|p| {
        let _ = p.set(picker);
    });

    let service = spawn_service(config, |event| {
        let text = format!("{event:?}");
        if let Err(e) = slint::invoke_from_event_loop(move || on_ui_event(event)) {
            log!("ui: dropped {text}: {e}");
        }
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
