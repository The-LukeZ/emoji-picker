//! Drops winit's raw input registration, so the Win+. hook keeps working over our own windows.
//!
//! winit registers the process for raw keyboard and mouse input (`RegisterRawInputDevices`)
//! as soon as its event loop exists. While a window of a process registered for raw keyboard
//! input has the foreground, Windows skips that process's `WH_KEYBOARD_LL` hook: the picker
//! went deaf to Win+. (and missed Win key-ups) whenever it was in front. Slint and this app
//! never use winit's device events, so the registration is removed when the loop starts.

use slint::winit_030::winit::event::StartCause;
use slint::winit_030::winit::event_loop::{ActiveEventLoop, DeviceEvents};
use slint::winit_030::{CustomApplicationHandler, EventResult};
use windows::Win32::UI::Input::{GetRegisteredRawInputDevices, RAWINPUTDEVICE};

/// Slint application handler that removes the raw input registration on the first wake-up.
#[derive(Default)]
pub struct NoRawInput {
    done: bool,
}

impl CustomApplicationHandler for NoRawInput {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, _cause: StartCause) -> EventResult {
        if !self.done {
            self.done = true;
            stop_raw_input(event_loop);
        }
        EventResult::Propagate
    }
}

/// Unregisters raw keyboard and mouse input for this process (winit's `RIDEV_REMOVE`).
pub fn stop_raw_input(event_loop: &ActiveEventLoop) {
    event_loop.listen_device_events(DeviceEvents::Never);
    crate::log!(
        "raw-input: device events off, {} raw input devices still registered",
        registered_raw_input_devices()
    );
}

/// How many raw input devices this process has registered.
pub fn registered_raw_input_devices() -> u32 {
    let mut count = 0u32;
    // SAFETY: a null buffer asks only for the count, written to `count`.
    let _ = unsafe {
        GetRegisteredRawInputDevices(None, &mut count, size_of::<RAWINPUTDEVICE>() as u32)
    };
    count
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slint::winit_030::winit::application::ApplicationHandler;
    use slint::winit_030::winit::event::WindowEvent;
    use slint::winit_030::winit::event_loop::EventLoop;
    use slint::winit_030::winit::platform::pump_events::EventLoopExtPumpEvents;
    use slint::winit_030::winit::platform::windows::EventLoopBuilderExtWindows;
    use slint::winit_030::winit::window::WindowId;

    use super::*;

    struct App {
        handler: NoRawInput,
    }

    impl ApplicationHandler for App {
        fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
            let _ = self.handler.new_events(event_loop, cause);
        }
        fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}
        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    }

    /// Headless: no window is created or shown, no input is sent, no hook is installed.
    /// winit allows one event loop per process, so this is the only test that makes one.
    #[test]
    fn winit_raw_input_registration_is_removed() {
        let mut event_loop = EventLoop::builder()
            .with_any_thread(true)
            .build()
            .expect("winit event loop");
        // winit registers keyboard and mouse for raw input on its hidden message window.
        assert_eq!(registered_raw_input_devices(), 2);
        let mut app = App {
            handler: NoRawInput::default(),
        };
        for _ in 0..10 {
            let _ = event_loop.pump_app_events(Some(Duration::ZERO), &mut app);
            if app.handler.done {
                break;
            }
        }
        assert!(app.handler.done);
        assert_eq!(registered_raw_input_devices(), 0);
    }
}
