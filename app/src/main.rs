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
mod settings;
mod win;

/// Generated Slint components (root `app/ui/app.slint`).
#[allow(dead_code)]
mod ui {
    slint::include_modules!();
}

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use emoji_core::config::Config;
use emoji_core::recents::Recents;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, LPARAM, WPARAM};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, PostMessageW, HWND_MESSAGE};

use picker::{InsertRequest, OpenRequest, Picker};
use settings::{with_state, AppState};
use win::service::{spawn_service, ServiceCmd, UiEvent, SERVICE_CLASS, WM_OPEN_SETTINGS};

/// Called by the picker after it hid: types or pastes the pick into the target window, or
/// copies it and tells the user why it could not be inserted.
fn on_insert(req: InsertRequest) {
    let cfg = with_state(AppState::config).unwrap_or_default();
    let started = Instant::now();
    let result = win::insert::insert(&req, cfg.insert_mode, &cfg.paste_exes);
    log!(
        "ui: insert {:?} into {:#x}: {result:?} in {:?}",
        req.text,
        req.target,
        started.elapsed()
    );
    if let Some(text) = result.balloon_text() {
        with_state(|s| {
            s.service.send(ServiceCmd::Balloon {
                title: "Emoji picker".into(),
                text: text.into(),
            });
        });
    }
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
            via_hook,
        } => {
            log!(
                "ui: OpenPicker target={target:#x} caret={caret:?} cursor={cursor:?} after {:?} (private {private_kb} KB)",
                at.elapsed()
            );
            with_state(|s| {
                s.picker.open(OpenRequest {
                    target,
                    caret,
                    cursor,
                    at,
                    via_hook,
                });
            });
        }
        UiEvent::HotkeySwitched(hotkey) => {
            log!("ui: hotkey switched to {hotkey}");
            // The picker's config is the newest copy (it saves tone and size): change only
            // the hotkey there and save, so nothing else is reverted.
            with_state(|s| {
                s.picker.save_hotkey(hotkey);
                s.refresh_settings();
            });
        }
        UiEvent::OpenSettings => {
            log!("ui: OpenSettings (private {private_kb} KB)");
            with_state(|s| {
                if s.config().hotkey.is_none() {
                    settings::show_first_run(s);
                } else {
                    settings::show_settings(s);
                }
            });
        }
        UiEvent::OpenAbout => {
            log!("ui: OpenAbout (private {private_kb} KB)");
            with_state(settings::show_about);
        }
        UiEvent::HotkeyFailed => {
            log!("ui: HotkeyFailed");
            with_state(AppState::on_hotkey_failed);
        }
        UiEvent::Quit => {
            log!("ui: Quit (private {private_kb} KB)");
            // Give the user's clipboard back now if a paste restore is still waiting.
            win::insert::flush_pending_restore();
            let _ = slint::quit_event_loop();
        }
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
        // Without this the Win+. hook is skipped while one of our windows is in front.
        .with_winit_custom_application_handler(win::raw_input::NoRawInput::default())
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
    let picker = Picker::new(config.clone(), recents, on_insert);
    #[cfg(any(debug_assertions, feature = "diag"))]
    if let Some(cycles) = std::env::var("EMOJI_PICKER_SELFTEST")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    {
        picker.selftest(cycles);
    }
    let first_run = config.hotkey.is_none();
    let handle = spawn_service(config, |event| {
        let text = format!("{event:?}");
        if let Err(e) = slint::invoke_from_event_loop(move || on_ui_event(event)) {
            log!("ui: dropped {text}: {e}");
        }
    });
    if let Some(text) = warning {
        handle.send(ServiceCmd::Balloon {
            title: "Emoji picker settings".into(),
            text,
        });
    }
    settings::install(AppState::new(handle, picker));
    if first_run {
        with_state(settings::show_first_run);
    }

    if let Err(e) = slint::run_event_loop_until_quit() {
        log!("event loop failed: {e}");
    }
    with_state(|s| s.service.shutdown());
}
