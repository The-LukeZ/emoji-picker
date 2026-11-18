//! Settings, first-run and About windows, plus [`AppState`], which owns the service handle
//! and the picker.
//!
//! The windows are rare, so each one is created on demand and dropped when it closes.
//!
//! Config writes have one writer: the [`Picker`]. It saves tone and size itself, and every
//! settings change goes through [`AppState::apply`], which starts from [`Picker::config`] (the
//! newest copy) and saves through [`Picker::save_config`]. No stale copy can overwrite a tone
//! or size change.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use emoji_core::config::{Config, InsertMode, SizeStep};
use emoji_core::hotkey::{Hotkey, HotkeyError, Mods};
use emoji_core::policy::exe_file_name;
use slint::winit_030::winit::event::{ElementState, WindowEvent};
use slint::winit_030::winit::keyboard::ModifiersState;
use slint::winit_030::winit::platform::scancode::PhysicalKeyExtScancode;
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, VecModel};
use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VSC_TO_VK_EX};

use crate::picker::Picker;
use crate::ui::{AboutWindow, FirstRunWindow, SettingsWindow};
use crate::win::service::{ServiceCmd, ServiceHandle};
use crate::win::sys;

const VK_ESCAPE: u16 = 0x1B;
const OWN_EXE: &str = "emoji-picker.exe";

const ABOUT_LINES: [&str; 4] = [
    "Emoji graphics: Twemoji by Twitter and contributors, CC-BY 4.0 (github.com/jdecked/twemoji)",
    "Emoji data: Emojibase (MIT)",
    "Made with Slint",
    "Emoji picker is released under the MIT license.",
];

const ALTGR_WARNING: &str = "Ctrl+Alt is AltGr on many keyboard layouts (German, for example): \
    this hotkey can stop you typing characters such as the euro sign or @.";

const SIZES: [SizeStep; 4] = [
    SizeStep::Small,
    SizeStep::Medium,
    SizeStep::Large,
    SizeStep::Huge,
];

/// The hotkey for a recorded key-down: `vk` is the virtual key, `mods` the modifiers held.
/// Win+. comes back as [`Hotkey::WinPeriod`] (the low-level hook), not as a combination.
pub(crate) fn recorded_hotkey(vk: u16, mods: Mods) -> Result<Hotkey, HotkeyError> {
    let combo = Hotkey::Combo { mods, vk };
    // Parsing the canonical text normalizes Win+. to `WinPeriod`.
    let hotkey = combo.to_string().parse::<Hotkey>().unwrap_or(combo);
    hotkey.validate().map(|()| hotkey)
}

/// Index of `hotkey` among the three choices: 0 Win+., 1 Ctrl+Shift+Space, 2 Custom.
fn choice_of(hotkey: Option<Hotkey>) -> i32 {
    let presets = Hotkey::presets();
    match hotkey {
        None => 0,
        Some(h) if h == presets[0] => 0,
        Some(h) if h == presets[1] => 1,
        Some(_) => 2,
    }
}

/// What a key-down means to a recorder.
#[derive(Debug, PartialEq)]
enum RecordStep {
    /// Not recording (any more): a second key-down queued in the same loop turn as the one
    /// that ended the recording.
    Ignore,
    /// Esc without modifiers: stop recording, keep the old hotkey.
    Cancel,
    /// A modifier alone: keep waiting for the main key.
    Wait,
    /// The combination cannot be used: stop recording and show this text.
    Reject(String),
    /// The combination is valid.
    Accept(Hotkey),
}

/// The step for a key-down `vk` with `mods` held. Only the first key-down while `recording`
/// counts (modifiers alone excepted).
fn record_step(recording: bool, vk: u16, mods: Mods) -> RecordStep {
    if !recording {
        return RecordStep::Ignore;
    }
    if vk == VK_ESCAPE && mods == Mods::default() {
        return RecordStep::Cancel;
    }
    match recorded_hotkey(vk, mods) {
        Err(HotkeyError::ModifierKeyAsKey) => RecordStep::Wait,
        Err(e) => RecordStep::Reject(format!(
            "{} cannot be used: {e}.",
            Hotkey::Combo { mods, vk }
        )),
        Ok(h) => RecordStep::Accept(h),
    }
}

/// What selecting a hotkey option in the settings window does.
#[derive(Debug, PartialEq)]
enum ChoiceAction {
    /// Save this hotkey.
    Apply(Hotkey),
    /// Start recording a custom hotkey.
    Record,
    /// Nothing to do (Custom is already the current hotkey).
    Keep,
}

/// Selecting option `i` (with Space or a click; the arrow keys only move the focus) in the
/// settings window. `custom` is the window's last valid custom hotkey, `current` the saved one.
fn settings_choice(i: i32, custom: Option<Hotkey>, current: Option<Hotkey>) -> ChoiceAction {
    let presets = Hotkey::presets();
    match i {
        0 => ChoiceAction::Apply(presets[0]),
        1 => ChoiceAction::Apply(presets[1]),
        _ => match custom {
            Some(h) => ChoiceAction::Apply(h),
            None if choice_of(current) == 2 => ChoiceAction::Keep,
            None => ChoiceAction::Record,
        },
    }
}

/// The first-run hotkey for option `choice`, or `None` for Custom without a valid recording.
fn first_run_pick(choice: i32, custom: Option<Hotkey>) -> Option<Hotkey> {
    let presets = Hotkey::presets();
    match choice {
        0 => Some(presets[0]),
        1 => Some(presets[1]),
        _ => custom,
    }
}

/// The first-run hotkey when the dialog is closed with the X: the selected option, or Win+.
/// while Custom has no valid recording.
fn first_run_close_pick(choice: i32, custom: Option<Hotkey>) -> Hotkey {
    first_run_pick(choice, custom).unwrap_or(Hotkey::WinPeriod)
}

/// A config save failed. The log is empty in release builds, so a balloon tells the user.
pub fn config_save_failed(path: &std::path::Path, e: &std::io::Error) {
    log!("settings: saving {} failed: {e}", path.display());
    with_state(|s| {
        s.service.send(ServiceCmd::Balloon {
            title: "Emoji picker".into(),
            text: format!("Settings could not be saved: {e}"),
        });
    });
}

/// `input` as a blocklist or paste list entry: trimmed, file name only, lowercased, with
/// `.exe` added when there is no extension. `None` for an empty name.
fn clean_exe(input: &str) -> Option<String> {
    let name = exe_file_name(input.trim());
    let name = name.trim();
    if name.is_empty() || name.chars().all(|c| c == '.') {
        return None;
    }
    if name.contains('.') {
        Some(name.to_string())
    } else {
        Some(format!("{name}.exe"))
    }
}

/// `list` cleaned (see [`clean_exe`]), without empty entries and without duplicates
/// (case-insensitive), keeping the first occurrence.
fn clean_exes(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in list.iter().filter_map(|e| clean_exe(e)) {
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// `list` (cleaned) with `input` added, or the text to show when `input` is rejected.
fn add_exe(list: &[String], input: &str) -> Result<Vec<String>, String> {
    let Some(name) = clean_exe(input) else {
        return Err("Type an exe name first, for example game.exe.".into());
    };
    let mut out = clean_exes(list);
    if out.contains(&name) {
        return Err(format!("{name} is already in the list."));
    }
    out.push(name);
    Ok(out)
}

/// The two diagnostics lines of the About window (spec section 8 measurements).
fn about_diagnostics(last_open_ms: Option<u32>, private_bytes: usize) -> [String; 2] {
    let open = match last_open_ms {
        Some(ms) => format!("Last picker open: {ms} ms"),
        None => "Last picker open: not opened yet".to_string(),
    };
    let mb = private_bytes as f64 / (1024.0 * 1024.0);
    [open, format!("Memory: {mb:.1} MB private")]
}

/// Text for a hotkey that is invalid or could not be registered.
fn failed_text(hotkey: Option<Hotkey>) -> String {
    match hotkey {
        Some(h) => match h.validate() {
            Err(e) => format!("Hotkey {h} cannot be used: {e}. Choose another one."),
            Ok(()) => format!(
                "Hotkey {h} could not be registered. Another app may use it. Choose another one."
            ),
        },
        None => "No hotkey is set. Choose one.".to_string(),
    }
}

fn model(items: &[String]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        items
            .iter()
            .map(SharedString::from)
            .collect::<Vec<SharedString>>(),
    ))
}

/// Shows `window` (again) and brings it to the front.
fn present(window: &slint::Window) {
    if let Err(e) = window.show() {
        log!("settings: showing a window failed: {e}");
        return;
    }
    window.with_winit_window(|w| w.focus_window());
}

/// Drops `value` on the next loop turn, outside the callback that is running now.
fn drop_later<T: 'static>(value: T) {
    Timer::single_shot(Duration::ZERO, move || drop(value));
}

/// Records the next key combination pressed in a window: while `recording` is set, key-downs
/// are kept from the window's widgets and passed (with the held modifiers) to the handler.
/// The window losing the focus is passed as Esc (cancel). While any window records, the
/// service releases the hotkey (see [`AppState::sync_recording`]).
struct Recorder {
    recording: Cell<bool>,
    mods: Cell<ModifiersState>,
}

impl Recorder {
    /// Starts or stops recording and suspends or restores the hotkey to match.
    fn set(&self, on: bool) {
        if self.recording.replace(on) != on {
            with_state(AppState::sync_recording);
        }
    }

    fn install(window: &slint::Window, on_key: impl Fn(u16, Mods) + 'static) -> Rc<Recorder> {
        let rec = Rc::new(Recorder {
            recording: Cell::new(false),
            mods: Cell::new(ModifiersState::empty()),
        });
        let on_key = Rc::new(on_key);
        let weak = Rc::downgrade(&rec);
        window.on_winit_window_event(move |_, event| {
            let Some(rec) = weak.upgrade() else {
                return EventResult::Propagate;
            };
            match event {
                WindowEvent::ModifiersChanged(m) => {
                    rec.mods.set(m.state());
                    EventResult::Propagate
                }
                WindowEvent::KeyboardInput { event, .. } if rec.recording.get() => {
                    if event.state == ElementState::Pressed && !event.repeat {
                        if let Some(vk) = event.physical_key.to_scancode().and_then(vk_of_scancode)
                        {
                            let m = rec.mods.get();
                            let mods = Mods {
                                ctrl: m.control_key(),
                                alt: m.alt_key(),
                                shift: m.shift_key(),
                                win: m.super_key(),
                            };
                            let on_key = on_key.clone();
                            // Touch the UI on the next loop turn, outside winit's dispatch.
                            Timer::single_shot(Duration::ZERO, move || on_key(vk, mods));
                        }
                    }
                    EventResult::PreventDefault
                }
                WindowEvent::Focused(false) if rec.recording.get() => {
                    // Keys pressed in other windows never come here: cancel, so the hotkey
                    // does not stay suspended.
                    let on_key = on_key.clone();
                    Timer::single_shot(Duration::ZERO, move || on_key(VK_ESCAPE, Mods::default()));
                    EventResult::Propagate
                }
                _ => EventResult::Propagate,
            }
        });
        rec
    }
}

/// Virtual key of a scancode (winit marks extended keys with `0xE000`, as `MapVirtualKeyW`
/// expects), in the current keyboard layout.
fn vk_of_scancode(scancode: u32) -> Option<u16> {
    // SAFETY: plain value conversion, no pointers.
    let vk = unsafe { MapVirtualKeyW(scancode, MAPVK_VSC_TO_VK_EX) };
    u16::try_from(vk).ok().filter(|&vk| vk != 0)
}

struct SettingsUi {
    ui: SettingsWindow,
    rec: Rc<Recorder>,
    /// The last valid custom recording of this window, offered again when Custom is chosen.
    custom: Cell<Option<Hotkey>>,
}

struct FirstRunUi {
    ui: FirstRunWindow,
    rec: Rc<Recorder>,
    custom: Cell<Option<Hotkey>>,
}

pub struct AppState {
    pub service: ServiceHandle,
    pub picker: Picker,
    /// The service reported that the current hotkey could not be registered.
    hotkey_failed: Cell<bool>,
    /// The service was asked to release the hotkey while a window records one.
    hotkey_suspended: Cell<bool>,
    settings: RefCell<Option<SettingsUi>>,
    first_run: RefCell<Option<FirstRunUi>>,
    about: RefCell<Option<AboutWindow>>,
}

thread_local! {
    /// The app state, created once on the UI thread before the event loop runs.
    static STATE: OnceCell<AppState> = const { OnceCell::new() };
}

/// Makes `state` the app state of this (the UI) thread.
pub fn install(state: AppState) {
    STATE.with(|s| {
        if s.set(state).is_err() {
            log!("settings: app state installed twice");
        }
    });
}

/// Runs `f` with the app state; `None` before [`install`].
pub fn with_state<R>(f: impl FnOnce(&AppState) -> R) -> Option<R> {
    STATE.with(|s| s.get().map(f))
}

impl AppState {
    pub fn new(service: ServiceHandle, picker: Picker) -> AppState {
        AppState {
            service,
            picker,
            hotkey_failed: Cell::new(false),
            hotkey_suspended: Cell::new(false),
            settings: RefCell::new(None),
            first_run: RefCell::new(None),
            about: RefCell::new(None),
        }
    }

    /// The current config (the picker's copy, which is the newest).
    pub fn config(&self) -> Arc<Config> {
        self.picker.config()
    }

    /// Applies `change` to the current config.
    fn update(&self, change: impl FnOnce(&mut Config)) {
        let mut cfg = (*self.config()).clone();
        change(&mut cfg);
        self.apply(cfg);
    }

    /// Saves `new` (through the picker, the only config writer), then tells the service and
    /// the picker, and updates the Run value.
    pub fn apply(&self, new: Config) {
        let old = self.config();
        if *old == new {
            return;
        }
        let new = Arc::new(new);
        self.picker.save_config(new.clone());
        log!("settings: saved {new:?}");
        if old.recents_enabled && !new.recents_enabled {
            self.picker.forget_recents();
        }
        if old.hotkey != new.hotkey
            || old.blocklist != new.blocklist
            || old.disable_in_fullscreen != new.disable_in_fullscreen
        {
            if old.hotkey != new.hotkey {
                self.hotkey_failed.set(false);
            }
            self.service.send(ServiceCmd::SetConfig(new.clone()));
        }
        // The first run sets the Run value to the default (on).
        if old.start_with_windows != new.start_with_windows || old.hotkey.is_none() {
            if let Err(e) = sys::set_autostart(new.start_with_windows) {
                log!("settings: start with Windows failed: {e}");
                self.service.send(ServiceCmd::Balloon {
                    title: "Emoji picker".into(),
                    text: format!("Start with Windows could not be changed: {e}"),
                });
            }
        }
        self.refresh_settings();
    }

    /// Suspends the hotkey while the settings or first-run window records one, and restores
    /// it once neither does. A closed window does not count, so closing never leaves the
    /// hotkey suspended.
    fn sync_recording(&self) {
        let recording = self
            .settings
            .borrow()
            .as_ref()
            .is_some_and(|s| s.rec.recording.get())
            || self
                .first_run
                .borrow()
                .as_ref()
                .is_some_and(|s| s.rec.recording.get());
        if self.hotkey_suspended.replace(recording) != recording {
            log!("settings: hotkey suspended={recording}");
            self.service.send(ServiceCmd::SuspendHotkey(recording));
        }
    }

    /// The service could not register the hotkey: the settings field turns red.
    pub fn on_hotkey_failed(&self) {
        self.hotkey_failed.set(true);
        self.refresh_settings();
    }

    /// Updates an open settings window from the current config.
    pub fn refresh_settings(&self) {
        let settings = self.settings.borrow();
        let Some(s) = settings.as_ref() else {
            return;
        };
        let cfg = self.config();
        let ui = &s.ui;
        if !s.rec.recording.get() {
            ui.set_hotkey_choice(choice_of(cfg.hotkey));
        }
        let custom = match cfg.hotkey {
            Some(h) if choice_of(Some(h)) == 2 => Some(h),
            _ => s.custom.get(),
        };
        ui.set_custom_text(custom.map(|h| h.to_string()).unwrap_or_default().into());
        let invalid = cfg.hotkey.is_some_and(|h| h.validate().is_err());
        let failed = self.hotkey_failed.get() || invalid;
        ui.set_hotkey_failed(failed);
        ui.set_hotkey_failed_text(failed_text(cfg.hotkey).into());
        if failed {
            log!("settings: hotkey field red: {}", failed_text(cfg.hotkey));
        }
        ui.set_blocklist(model(&cfg.blocklist));
        ui.set_disable_in_fullscreen(cfg.disable_in_fullscreen);
        ui.set_recents_disabled(!cfg.recents_enabled);
        ui.set_skin_tone(i32::from(cfg.skin_tone.min(5)));
        ui.set_size(SIZES.iter().position(|&z| z == cfg.size).unwrap_or(1) as i32);
        ui.set_insert_mode(match cfg.insert_mode {
            InsertMode::Type => 0,
            InsertMode::Paste => 1,
        });
        ui.set_paste_exes(model(&cfg.paste_exes));
        ui.set_start_with_windows(cfg.start_with_windows);
        if ui.get_running_open() {
            ui.set_running(model(&running_apps(&cfg)));
        }
    }
}

/// Running apps with visible windows that are not blocked yet (and not this app).
fn running_apps(cfg: &Config) -> Vec<String> {
    let blocked = clean_exes(&cfg.blocklist);
    sys::visible_window_exes()
        .into_iter()
        .filter(|e| e != OWN_EXE && !blocked.contains(e))
        .collect()
}

/// Runs `f` with the open settings window.
fn with_settings(f: impl FnOnce(&AppState, &SettingsUi)) {
    with_state(|state| {
        let settings = state.settings.borrow();
        if let Some(s) = settings.as_ref() {
            f(state, s);
        }
    });
}

/// Opens the settings window, or brings it to the front when it is open.
pub fn show_settings(state: &AppState) {
    if let Some(s) = state.settings.borrow().as_ref() {
        present(s.ui.window());
        return;
    }
    let ui = match SettingsWindow::new() {
        Ok(ui) => ui,
        Err(e) => {
            log!("settings: creating the window failed: {e}");
            return;
        }
    };
    ui.set_dark(!sys::apps_use_light_theme());
    let rec = Recorder::install(ui.window(), |vk, mods| {
        with_settings(|state, s| settings_recorded(state, s, vk, mods));
    });
    wire_settings(&ui);
    // A valid custom hotkey from the config is offered again after choosing a preset.
    let custom = state
        .config()
        .hotkey
        .filter(|h| choice_of(Some(*h)) == 2 && h.validate().is_ok());
    *state.settings.borrow_mut() = Some(SettingsUi {
        ui: ui.clone_strong(),
        rec,
        custom: Cell::new(custom),
    });
    state.refresh_settings();
    present(ui.window());
    ui.invoke_focus_hotkey();
    log!("settings: shown");
    #[cfg(any(debug_assertions, feature = "diag"))]
    ui_test::start();
}

fn close_settings() {
    with_state(|state| {
        let taken = state.settings.borrow_mut().take();
        if let Some(s) = taken {
            let _ = s.ui.hide();
            s.rec.set(false);
            log!("settings: closed");
            drop_later(s);
        }
        // A closed window never keeps the hotkey suspended.
        state.sync_recording();
    });
}

/// A key-down while recording in the settings window.
fn settings_recorded(state: &AppState, s: &SettingsUi, vk: u16, mods: Mods) {
    let ui = &s.ui;
    match record_step(s.rec.recording.get(), vk, mods) {
        RecordStep::Ignore | RecordStep::Wait => {}
        RecordStep::Cancel => {
            s.rec.set(false);
            ui.set_recording(false);
            ui.set_hotkey_choice(choice_of(state.config().hotkey));
        }
        RecordStep::Reject(text) => {
            s.rec.set(false);
            ui.set_recording(false);
            ui.set_hotkey_warning(SharedString::new());
            log!("settings: recorded hotkey rejected: {text}");
            ui.set_hotkey_error(text.into());
            ui.set_hotkey_choice(choice_of(state.config().hotkey));
        }
        RecordStep::Accept(h) => {
            ui.set_recording(false);
            ui.set_hotkey_error(SharedString::new());
            let warning = if h.altgr_risk() { ALTGR_WARNING } else { "" };
            if h.altgr_risk() {
                log!("settings: AltGr warning for {h}");
            }
            ui.set_hotkey_warning(warning.into());
            if choice_of(Some(h)) == 2 {
                s.custom.set(Some(h));
            }
            log!("settings: recorded {h}");
            // Save while the old hotkey is still suspended, then let the service arm the new
            // one.
            state.update(|c| c.hotkey = Some(h));
            s.rec.set(false);
            state.refresh_settings();
        }
    }
}

fn start_recording_settings(s: &SettingsUi) {
    s.rec.set(true);
    s.ui.set_recording(true);
    s.ui.set_hotkey_choice(2);
    s.ui.set_hotkey_error(SharedString::new());
    s.ui.set_hotkey_warning(SharedString::new());
}

fn wire_settings(ui: &SettingsWindow) {
    ui.on_hotkey_chosen(|i| {
        with_settings(|state, s| {
            let ui = &s.ui;
            ui.set_hotkey_error(SharedString::new());
            ui.set_hotkey_warning(SharedString::new());
            match settings_choice(i, s.custom.get(), state.config().hotkey) {
                ChoiceAction::Apply(h) => {
                    if h.altgr_risk() {
                        ui.set_hotkey_warning(ALTGR_WARNING.into());
                    }
                    ui.set_recording(false);
                    state.update(|c| c.hotkey = Some(h));
                    s.rec.set(false);
                    state.refresh_settings();
                }
                ChoiceAction::Keep => {}
                ChoiceAction::Record => start_recording_settings(s),
            }
        });
    });
    ui.on_record(|| with_settings(|_, s| start_recording_settings(s)));

    ui.on_block_add(|text| {
        with_settings(|state, s| match add_exe(&state.config().blocklist, &text) {
            Ok(list) => {
                s.ui.set_block_input(SharedString::new());
                s.ui.set_block_error(SharedString::new());
                state.update(|c| c.blocklist = list);
            }
            Err(e) => {
                log!("settings: blocklist entry {text:?} rejected: {e}");
                s.ui.set_block_error(e.into());
            }
        });
    });
    ui.on_block_remove(|i| {
        with_state(|state| {
            state.update(|c| {
                if let Ok(i) = usize::try_from(i) {
                    if i < c.blocklist.len() {
                        c.blocklist.remove(i);
                    }
                }
            });
        });
    });
    ui.on_show_running(|| {
        with_settings(|state, s| {
            let open = !s.ui.get_running_open();
            s.ui.set_running_open(open);
            if open {
                let apps = running_apps(&state.config());
                log!("settings: running apps {apps:?}");
                s.ui.set_running(model(&apps));
            }
        });
    });
    ui.on_add_running(|exe| {
        with_settings(|state, s| {
            if let Ok(list) = add_exe(&state.config().blocklist, &exe) {
                s.ui.set_block_error(SharedString::new());
                state.update(|c| c.blocklist = list);
            }
        });
    });
    ui.on_fullscreen_toggled(|on| {
        with_state(|state| state.update(|c| c.disable_in_fullscreen = on));
    });
    ui.on_recents_toggled(|disabled| {
        with_state(|state| state.update(|c| c.recents_enabled = !disabled));
    });
    ui.on_tone_chosen(|i| {
        with_state(|state| state.update(|c| c.skin_tone = i.clamp(0, 5) as u8));
    });
    ui.on_size_chosen(|i| {
        with_state(|state| {
            let size = SIZES[i.clamp(0, 3) as usize];
            state.update(|c| c.size = size);
        });
    });
    ui.on_insert_mode_chosen(|i| {
        with_state(|state| {
            let mode = if i == 1 {
                InsertMode::Paste
            } else {
                InsertMode::Type
            };
            state.update(|c| c.insert_mode = mode);
        });
    });
    ui.on_paste_add(|text| {
        with_settings(
            |state, s| match add_exe(&state.config().paste_exes, &text) {
                Ok(list) => {
                    s.ui.set_paste_input(SharedString::new());
                    s.ui.set_paste_error(SharedString::new());
                    state.update(|c| c.paste_exes = list);
                }
                Err(e) => s.ui.set_paste_error(e.into()),
            },
        );
    });
    ui.on_paste_remove(|i| {
        with_state(|state| {
            state.update(|c| {
                if let Ok(i) = usize::try_from(i) {
                    if i < c.paste_exes.len() {
                        c.paste_exes.remove(i);
                    }
                }
            });
        });
    });
    ui.on_autostart_toggled(|on| {
        with_state(|state| state.update(|c| c.start_with_windows = on));
    });
    ui.on_check_updates(|| {
        // Wired to Velopack in Task 16.
        with_settings(|_, s| {
            s.ui.set_update_status("Update checks are not available in this build yet.".into());
        });
    });
    ui.on_about(|| {
        with_state(show_about);
    });
    ui.on_close_window(close_settings);
    ui.window().on_close_requested(|| {
        close_settings();
        slint::CloseRequestResponse::HideWindow
    });
}

/// Runs `f` with the open first-run window.
fn with_first_run(f: impl FnOnce(&AppState, &FirstRunUi)) {
    with_state(|state| {
        let first = state.first_run.borrow();
        if let Some(s) = first.as_ref() {
            f(state, s);
        }
    });
}

/// Opens the first-run dialog (hotkey choice), or brings it to the front.
pub fn show_first_run(state: &AppState) {
    if let Some(s) = state.first_run.borrow().as_ref() {
        present(s.ui.window());
        return;
    }
    let ui = match FirstRunWindow::new() {
        Ok(ui) => ui,
        Err(e) => {
            log!("first-run: creating the window failed: {e}");
            // Without the dialog, take the preselected option so the hotkey works.
            state.update(|c| c.hotkey = Some(Hotkey::WinPeriod));
            return;
        }
    };
    ui.set_dark(!sys::apps_use_light_theme());
    ui.set_choice(0);
    let rec = Recorder::install(ui.window(), |vk, mods| {
        with_first_run(|_, s| first_run_recorded(s, vk, mods));
    });
    // Space or a click selects an option (the arrow keys only move the focus). Custom
    // without a valid recording starts recording.
    ui.on_chosen(|i| {
        with_first_run(|_, s| {
            s.ui.set_choice(i);
            s.ui.set_error_text(SharedString::new());
            s.ui.set_warning_text(SharedString::new());
            match first_run_pick(i, s.custom.get()) {
                Some(h) => {
                    if h.altgr_risk() {
                        s.ui.set_warning_text(ALTGR_WARNING.into());
                    }
                    s.rec.set(false);
                    s.ui.set_recording(false);
                    s.ui.set_can_accept(true);
                }
                None => start_recording_first_run(s),
            }
        });
    });
    ui.on_record(|| with_first_run(|_, s| start_recording_first_run(s)));
    ui.on_confirm(|| {
        let mut chosen = None;
        with_first_run(|_, s| chosen = first_run_pick(s.ui.get_choice(), s.custom.get()));
        if let Some(h) = chosen {
            finish_first_run(h, "ok");
        }
    });
    ui.window().on_close_requested(|| {
        // The X takes the selected option, or Win+. while Custom has no valid recording.
        let mut chosen = Hotkey::WinPeriod;
        with_first_run(|_, s| chosen = first_run_close_pick(s.ui.get_choice(), s.custom.get()));
        finish_first_run(chosen, "closed");
        slint::CloseRequestResponse::HideWindow
    });
    *state.first_run.borrow_mut() = Some(FirstRunUi {
        ui: ui.clone_strong(),
        rec,
        custom: Cell::new(None),
    });
    present(ui.window());
    ui.invoke_focus_choice();
    log!("first-run: shown");
}

fn start_recording_first_run(s: &FirstRunUi) {
    s.rec.set(true);
    s.ui.set_recording(true);
    s.ui.set_choice(2);
    s.ui.set_can_accept(false);
    s.ui.set_error_text(SharedString::new());
    s.ui.set_warning_text(SharedString::new());
}

fn first_run_recorded(s: &FirstRunUi, vk: u16, mods: Mods) {
    let ui = &s.ui;
    let stop = || {
        s.rec.set(false);
        ui.set_recording(false);
    };
    match record_step(s.rec.recording.get(), vk, mods) {
        RecordStep::Ignore | RecordStep::Wait => {}
        RecordStep::Cancel => {
            stop();
            if s.custom.get().is_none() {
                ui.set_choice(0);
            }
            ui.set_can_accept(true);
        }
        RecordStep::Reject(text) => {
            stop();
            log!("first-run: recorded hotkey rejected: {text}");
            ui.set_error_text(text.into());
            ui.set_warning_text(SharedString::new());
            ui.set_can_accept(s.custom.get().is_some());
        }
        RecordStep::Accept(h) => {
            stop();
            log!("first-run: recorded {h}");
            ui.set_error_text(SharedString::new());
            let warning = if h.altgr_risk() { ALTGR_WARNING } else { "" };
            ui.set_warning_text(warning.into());
            let choice = choice_of(Some(h));
            if choice == 2 {
                s.custom.set(Some(h));
                ui.set_custom_text(h.to_string().into());
            }
            ui.set_choice(choice);
            ui.set_can_accept(true);
        }
    }
}

/// Applies the first-run choice and closes the dialog (once).
fn finish_first_run(hotkey: Hotkey, how: &str) {
    with_state(|state| {
        let taken = state.first_run.borrow_mut().take();
        let Some(s) = taken else {
            return;
        };
        let _ = s.ui.hide();
        log!("first-run: applied {hotkey} ({how})");
        drop_later(s);
        // Save while a recording still suspends the hotkey, then restore it: the dialog is
        // gone, so it no longer counts as recording.
        state.update(|c| c.hotkey = Some(hotkey));
        state.sync_recording();
    });
}

/// Opens the About window (credits and diagnostics), or refreshes it and brings it to the
/// front.
pub fn show_about(state: &AppState) {
    let existing = state.about.borrow().as_ref().map(|a| a.clone_strong());
    let ui = match existing {
        Some(ui) => ui,
        None => {
            let ui = match AboutWindow::new() {
                Ok(ui) => ui,
                Err(e) => {
                    log!("about: creating the window failed: {e}");
                    return;
                }
            };
            ui.set_dark(!sys::apps_use_light_theme());
            let heading = format!("Emoji picker {}", env!("CARGO_PKG_VERSION"));
            ui.set_heading(heading.clone().into());
            let lines: Vec<String> = ABOUT_LINES.iter().map(|l| l.to_string()).collect();
            ui.set_lines(model(&lines));
            ui.on_close_window(close_about);
            ui.window().on_close_requested(|| {
                close_about();
                slint::CloseRequestResponse::HideWindow
            });
            *state.about.borrow_mut() = Some(ui.clone_strong());
            log!("about: {heading}");
            for line in ABOUT_LINES {
                log!("about: {line}");
            }
            ui
        }
    };
    let diag = about_diagnostics(state.picker.last_open_ms(), sys::private_bytes());
    for line in &diag {
        log!("about: {line}");
    }
    ui.set_diagnostics(model(&diag));
    present(ui.window());
    log!("about: shown");
}

fn close_about() {
    with_state(|state| {
        if let Some(ui) = state.about.borrow_mut().take() {
            let _ = ui.hide();
            log!("about: closed");
            drop_later(ui);
        }
    });
}

/// Scripted settings changes for the desktop test driver (debug and `diag` builds only).
///
/// `EMOJI_PICKER_UI_TEST` holds steps separated by `;`, run 400 ms apart once the settings
/// window is shown. Each step invokes the same callback the widget would (or, for `record`,
/// the recorder's key handler), so only the widget-to-callback binding is skipped:
/// `hotkey=N`, `record=VK,mods` (mods like `ctrl+alt`, VK in hex), `block-add=TEXT`,
/// `block-remove=N`, `running`, `add-running=EXE`, `fullscreen=on|off`, `recents=on|off`,
/// `autostart=on|off`, `tone=N`, `size=N`, `insert=N`, `paste-add=TEXT`, `updates`, `about`,
/// `close-about`, `close`.
#[cfg(any(debug_assertions, feature = "diag"))]
mod ui_test {
    use super::*;

    thread_local! {
        static STARTED: Cell<bool> = const { Cell::new(false) };
    }

    pub fn start() {
        let Ok(script) = std::env::var("EMOJI_PICKER_UI_TEST") else {
            return;
        };
        if STARTED.with(|s| s.replace(true)) {
            return;
        }
        let steps: Vec<String> = script
            .split(';')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        log!("ui-test: {} steps", steps.len());
        run(Rc::new(steps), 0);
    }

    fn run(steps: Rc<Vec<String>>, i: usize) {
        Timer::single_shot(Duration::from_millis(400), move || {
            let Some(step) = steps.get(i) else {
                log!("ui-test: done");
                return;
            };
            log!("ui-test: step {step}");
            step_once(step);
            run(steps.clone(), i + 1);
        });
    }

    fn parse_mods(text: &str) -> Mods {
        let mut m = Mods::default();
        for part in text.split('+').map(str::trim) {
            match part {
                "ctrl" => m.ctrl = true,
                "alt" => m.alt = true,
                "shift" => m.shift = true,
                "win" => m.win = true,
                _ => {}
            }
        }
        m
    }

    fn step_once(step: &str) {
        let (name, arg) = step.split_once('=').unwrap_or((step, ""));
        let num = || arg.trim().parse::<i32>().unwrap_or(0);
        let on = arg.trim() == "on";
        if name == "close-about" {
            close_about();
            return;
        }
        let Some(ui) =
            with_state(|s| s.settings.borrow().as_ref().map(|s| s.ui.clone_strong())).flatten()
        else {
            log!("ui-test: settings window is not open");
            return;
        };
        match name {
            "hotkey" => ui.invoke_hotkey_chosen(num()),
            "record" => {
                let (vk, mods) = arg.split_once(',').unwrap_or((arg, ""));
                let vk = u16::from_str_radix(vk.trim().trim_start_matches("0x"), 16).unwrap_or(0);
                let mods = parse_mods(mods);
                with_settings(|_, s| start_recording_settings(s));
                with_settings(|state, s| settings_recorded(state, s, vk, mods));
            }
            "block-add" => {
                ui.set_block_input(arg.into());
                ui.invoke_block_add(arg.into());
            }
            "block-remove" => ui.invoke_block_remove(num()),
            "running" => ui.invoke_show_running(),
            "add-running" => ui.invoke_add_running(arg.into()),
            "fullscreen" => {
                ui.set_disable_in_fullscreen(on);
                ui.invoke_fullscreen_toggled(on);
            }
            "recents" => {
                ui.set_recents_disabled(!on);
                ui.invoke_recents_toggled(!on);
            }
            "autostart" => {
                ui.set_start_with_windows(on);
                ui.invoke_autostart_toggled(on);
            }
            "tone" => ui.invoke_tone_chosen(num()),
            "size" => ui.invoke_size_chosen(num()),
            "insert" => ui.invoke_insert_mode_chosen(num()),
            "paste-add" => ui.invoke_paste_add(arg.into()),
            "updates" => ui.invoke_check_updates(),
            "about" => ui.invoke_about(),
            "close" => ui.invoke_close_window(),
            other => log!("ui-test: unknown step {other}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(ctrl: bool, alt: bool, shift: bool, win: bool) -> Mods {
        Mods {
            ctrl,
            alt,
            shift,
            win,
        }
    }

    #[test]
    fn recorder_maps_keys() {
        assert_eq!(
            recorded_hotkey(0xBE, mods(false, false, false, true)),
            Ok(Hotkey::WinPeriod)
        );
        assert_eq!(
            recorded_hotkey(0x41, mods(false, false, true, false)),
            Err(HotkeyError::ShiftOnly)
        );
        assert_eq!(
            recorded_hotkey(0x11, mods(true, false, false, false)),
            Err(HotkeyError::ModifierKeyAsKey)
        );
        assert_eq!(
            recorded_hotkey(0x20, mods(true, false, true, false)),
            Ok(Hotkey::presets()[1])
        );
    }

    #[test]
    fn recorder_edge_cases() {
        // Win+Shift+. is a normal combination, not the Win+. hook.
        let h = recorded_hotkey(0xBE, mods(false, false, true, true)).unwrap();
        assert_ne!(h, Hotkey::WinPeriod);
        assert_eq!(h.to_string(), "Shift+Win+.");
        assert_eq!(
            recorded_hotkey(0x41, Mods::default()),
            Err(HotkeyError::NoModifier)
        );
        // Modifier keys are rejected before the modifier checks.
        assert_eq!(
            recorded_hotkey(0xA0, Mods::default()),
            Err(HotkeyError::ModifierKeyAsKey)
        );
        // Unnamed keys survive (no parse round trip loss).
        let oem = recorded_hotkey(0xBA, mods(true, false, false, false)).unwrap();
        assert_eq!(
            oem,
            Hotkey::Combo {
                mods: mods(true, false, false, false),
                vk: 0xBA
            }
        );
        assert!(recorded_hotkey(0x45, mods(true, true, false, false))
            .unwrap()
            .altgr_risk());
    }

    #[test]
    fn choices_map_presets() {
        let p = Hotkey::presets();
        assert_eq!(choice_of(None), 0);
        assert_eq!(choice_of(Some(p[0])), 0);
        assert_eq!(choice_of(Some(p[1])), 1);
        assert_eq!(choice_of(Some("Ctrl+Alt+K".parse().unwrap())), 2);
    }

    #[test]
    fn exe_entries_are_cleaned() {
        assert_eq!(clean_exe("  Game.EXE "), Some("game.exe".into()));
        assert_eq!(clean_exe(r"C:\Games\Foo.exe"), Some("foo.exe".into()));
        assert_eq!(clean_exe("valorant"), Some("valorant.exe".into()));
        assert_eq!(clean_exe(""), None);
        assert_eq!(clean_exe("   "), None);
        assert_eq!(clean_exe(r"C:\Games\"), None);
        assert_eq!(clean_exe("."), None);
    }

    #[test]
    fn add_exe_rejects_empty_and_duplicates() {
        let list = vec!["game.exe".to_string()];
        assert!(add_exe(&list, "   ").is_err());
        assert_eq!(
            add_exe(&list, " GAME.exe "),
            Err("game.exe is already in the list.".to_string())
        );
        assert_eq!(
            add_exe(&list, "Other.exe"),
            Ok(vec!["game.exe".to_string(), "other.exe".to_string()])
        );
        // A hand-edited list loses empty and duplicate entries on the next add.
        let messy = vec![
            "".to_string(),
            " A.exe".to_string(),
            "a.EXE".to_string(),
            "  ".to_string(),
        ];
        assert_eq!(
            add_exe(&messy, "b.exe"),
            Ok(vec!["a.exe".to_string(), "b.exe".to_string()])
        );
    }

    #[test]
    fn about_diagnostics_format() {
        assert_eq!(
            about_diagnostics(Some(42), 12 * 1024 * 1024 + 512 * 1024),
            [
                "Last picker open: 42 ms".to_string(),
                "Memory: 12.5 MB private".to_string()
            ]
        );
        assert_eq!(
            about_diagnostics(None, 0)[0],
            "Last picker open: not opened yet"
        );
    }

    #[test]
    fn failed_text_names_reason() {
        let bad: Hotkey = "A".parse().unwrap();
        assert!(failed_text(Some(bad)).contains("cannot be used"));
        assert!(failed_text(Some(Hotkey::presets()[1])).contains("could not be registered"));
    }

    #[test]
    fn about_texts_have_no_long_dashes() {
        for text in ABOUT_LINES.iter().chain([&ALTGR_WARNING]) {
            assert!(!text.contains(['\u{2014}', '\u{2015}']), "{text}");
        }
    }

    #[test]
    fn record_step_takes_only_the_first_key() {
        let ctrl_alt = mods(true, true, false, false);
        // After the recording ended, queued key-downs (Up, Tab, Esc) are ignored.
        assert_eq!(
            record_step(false, 0x26, Mods::default()),
            RecordStep::Ignore
        );
        assert_eq!(record_step(false, 0x4B, ctrl_alt), RecordStep::Ignore);
        assert_eq!(
            record_step(false, VK_ESCAPE, Mods::default()),
            RecordStep::Ignore
        );
        assert_eq!(
            record_step(true, VK_ESCAPE, Mods::default()),
            RecordStep::Cancel
        );
        // Esc with a modifier is a combination, not a cancel.
        assert!(matches!(
            record_step(true, VK_ESCAPE, mods(true, false, false, false)),
            RecordStep::Accept(_)
        ));
        assert_eq!(
            record_step(true, 0x11, mods(true, false, false, false)),
            RecordStep::Wait
        );
        assert_eq!(
            record_step(true, 0x41, mods(false, false, true, false)),
            RecordStep::Reject(format!(
                "Shift+A cannot be used: {}.",
                HotkeyError::ShiftOnly
            ))
        );
        assert_eq!(
            record_step(true, 0x4B, ctrl_alt),
            RecordStep::Accept("Ctrl+Alt+K".parse().unwrap())
        );
    }

    #[test]
    fn settings_choice_actions() {
        let p = Hotkey::presets();
        let k: Hotkey = "Ctrl+Alt+K".parse().unwrap();
        assert_eq!(settings_choice(0, None, Some(k)), ChoiceAction::Apply(p[0]));
        assert_eq!(
            settings_choice(1, Some(k), Some(p[0])),
            ChoiceAction::Apply(p[1])
        );
        assert_eq!(
            settings_choice(2, Some(k), Some(p[0])),
            ChoiceAction::Apply(k)
        );
        assert_eq!(settings_choice(2, None, Some(p[0])), ChoiceAction::Record);
        assert_eq!(settings_choice(2, None, Some(k)), ChoiceAction::Keep);
    }

    #[test]
    fn first_run_picks() {
        let p = Hotkey::presets();
        let k: Hotkey = "Ctrl+Alt+K".parse().unwrap();
        assert_eq!(first_run_pick(0, Some(k)), Some(p[0]));
        assert_eq!(first_run_pick(1, None), Some(p[1]));
        assert_eq!(first_run_pick(2, Some(k)), Some(k));
        assert_eq!(first_run_pick(2, None), None);
        // The X applies the selected option; Custom without a recording falls back to Win+.
        assert_eq!(first_run_close_pick(1, None), p[1]);
        assert_eq!(first_run_close_pick(2, Some(k)), k);
        assert_eq!(first_run_close_pick(2, None), Hotkey::WinPeriod);
    }

    /// Headless check of the hotkey radios: Up and Down only move the focus, Space selects
    /// the focused option.
    #[test]
    fn hotkey_radio_arrows_only_move_focus() {
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        use slint::platform::{Key, Platform, WindowAdapter, WindowEvent as SlintEvent};

        struct Headless;
        impl Platform for Headless {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
                Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
            }
        }
        // The platform is per thread; another test on this thread may have set it.
        let _ = slint::platform::set_platform(Box::new(Headless));

        let press = |w: &slint::Window, text: SharedString| {
            w.dispatch_event(SlintEvent::KeyPressed { text: text.clone() });
            w.dispatch_event(SlintEvent::KeyReleased { text });
        };
        let chosen: Rc<RefCell<Vec<i32>>> = Rc::default();

        let ui = SettingsWindow::new().unwrap();
        let log = chosen.clone();
        ui.on_hotkey_chosen(move |i| log.borrow_mut().push(i));
        ui.show().unwrap();
        ui.invoke_focus_hotkey();
        // From Win+.: Up wraps to Custom, Down wraps to Win+., then Ctrl+Shift+Space,
        // Custom, and Up goes back to Ctrl+Shift+Space.
        for key in [
            Key::UpArrow,
            Key::DownArrow,
            Key::DownArrow,
            Key::DownArrow,
            Key::UpArrow,
        ] {
            press(ui.window(), key.into());
        }
        assert!(
            chosen.borrow().is_empty(),
            "arrows selected {:?}",
            chosen.borrow()
        );
        assert_eq!(ui.get_hotkey_choice(), 0);
        // The focus is on Ctrl+Shift+Space: Space selects it.
        press(ui.window(), " ".into());
        assert_eq!(*chosen.borrow(), vec![1]);
        ui.hide().unwrap();
        drop(ui);

        chosen.borrow_mut().clear();
        let ui = FirstRunWindow::new().unwrap();
        let log = chosen.clone();
        ui.on_chosen(move |i| log.borrow_mut().push(i));
        let confirmed = Rc::new(Cell::new(0));
        let c = confirmed.clone();
        ui.on_confirm(move || c.set(c.get() + 1));
        ui.show().unwrap();
        ui.invoke_focus_choice();
        press(ui.window(), Key::DownArrow.into());
        press(ui.window(), Key::DownArrow.into());
        assert!(
            chosen.borrow().is_empty(),
            "arrows selected {:?}",
            chosen.borrow()
        );
        assert_eq!(ui.get_choice(), 0);
        // Enter confirms the selected option (Win+.), not the focused one.
        press(ui.window(), Key::Return.into());
        assert_eq!(confirmed.get(), 1);
        assert!(chosen.borrow().is_empty());
        press(ui.window(), " ".into());
        assert_eq!(*chosen.borrow(), vec![2]);
        ui.hide().unwrap();
    }

    #[test]
    #[ignore = "writes PNG snapshots of the settings, first-run and about windows; run with --ignored"]
    fn render_window_snapshots() {
        use slint::platform::software_renderer::{
            MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
        };
        use slint::platform::{Platform, WindowAdapter};

        thread_local! {
            static LAST: RefCell<Option<Rc<MinimalSoftwareWindow>>> = const { RefCell::new(None) };
        }
        struct Fwd;
        impl Platform for Fwd {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
                let w = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
                LAST.with(|l| *l.borrow_mut() = Some(w.clone()));
                Ok(w)
            }
        }
        slint::platform::set_platform(Box::new(Fwd)).unwrap();
        let out = std::env::var_os("SNAPSHOT_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let render = |name: &str, w: u32, h: u32| {
            let window = LAST.with(|l| l.borrow().clone()).unwrap();
            window.set_size(slint::PhysicalSize::new(w, h));
            let mut buf = vec![PremultipliedRgbaColor::default(); (w * h) as usize];
            for _ in 0..3 {
                slint::platform::update_timers_and_animations();
                window.request_redraw();
                window.draw_if_needed(|r| {
                    r.render(&mut buf, w as usize);
                });
            }
            let bytes: Vec<u8> = buf
                .iter()
                .flat_map(|p| [p.red, p.green, p.blue, p.alpha])
                .collect();
            let path = out.join(name);
            let file = std::fs::File::create(&path).unwrap();
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header()
                .unwrap()
                .write_image_data(&bytes)
                .unwrap();
            eprintln!("wrote {} ({w}x{h})", path.display());
        };

        let ui = SettingsWindow::new().unwrap();
        ui.set_hotkey_choice(2);
        ui.set_custom_text("Ctrl+Alt+K".into());
        ui.set_hotkey_warning(ALTGR_WARNING.into());
        ui.set_hotkey_failed(true);
        ui.set_hotkey_failed_text(failed_text(Some("Ctrl+Alt+K".parse().unwrap())).into());
        ui.set_blocklist(model(&[
            "game.exe".into(),
            "valorant-win64-shipping.exe".into(),
        ]));
        ui.set_block_error("game.exe is already in the list.".into());
        ui.set_running_open(true);
        ui.set_running(model(&["chrome.exe".into(), "notepad.exe".into()]));
        ui.set_disable_in_fullscreen(true);
        ui.set_skin_tone(3);
        ui.set_size(2);
        ui.set_insert_mode(0);
        ui.set_start_with_windows(true);
        ui.show().unwrap();
        render("settings-light.png", 520, 1400);
        ui.set_dark(true);
        render("settings-dark.png", 520, 1400);
        render("settings-default-size.png", 520, 640);
        ui.hide().unwrap();
        drop(ui);

        let ui = FirstRunWindow::new().unwrap();
        ui.show().unwrap();
        render("first-run-light.png", 460, 360);
        ui.set_dark(true);
        ui.set_choice(2);
        ui.set_recording(true);
        ui.set_error_text(
            "Shift+A cannot be used: Shift alone is not enough, add Ctrl, Alt or Win.".into(),
        );
        render("first-run-dark.png", 460, 360);
        ui.hide().unwrap();
        drop(ui);

        let ui = AboutWindow::new().unwrap();
        ui.set_heading("Emoji picker 0.1.0".into());
        let lines: Vec<String> = ABOUT_LINES.iter().map(|l| l.to_string()).collect();
        ui.set_lines(model(&lines));
        ui.set_diagnostics(model(&about_diagnostics(Some(42), 14 << 20)));
        ui.show().unwrap();
        render("about-light.png", 460, 300);
    }
}
