//! The service thread: message-only window, global hotkey, foreground tracking and tray.
//!
//! Everything Win32 that must keep running while the UI is idle lives on this one thread.
//! The UI thread talks to it through [`ServiceHandle`]; it reports back through `on_event`.

use std::cell::{Cell, RefCell};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use emoji_core::config::Config;
use emoji_core::geometry::{Point, Rect};
use emoji_core::hook_matcher::{VK_LWIN, VK_RWIN};
use emoji_core::hotkey::Hotkey;
use emoji_core::policy::{exe_file_name, hotkey_allowed, Foreground};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL,
    MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::Shell::NIN_SELECT;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetForegroundWindow, GetMessageW, GetSystemMetrics, GetWindowThreadProcessId, KillTimer,
    LoadIconW, LoadImageW, PostMessageW, PostQuitMessage, PostThreadMessageW, RegisterClassExW,
    RegisterWindowMessageW, SetTimer, TranslateMessage, ASFW_ANY, CHILDID_SELF,
    EVENT_OBJECT_LOCATIONCHANGE, EVENT_SYSTEM_FOREGROUND, HICON, HWND_MESSAGE, IDI_APPLICATION,
    IMAGE_ICON, LR_DEFAULTCOLOR, MSG, OBJID_WINDOW, SM_CXSMICON, SM_CYSMICON,
    WINEVENT_OUTOFCONTEXT, WM_APP, WM_COMMAND, WM_CONTEXTMENU, WM_HOTKEY, WM_TIMER,
    WM_WTSSESSION_CHANGE, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP,
};

use super::foreground::{caret_rect, cursor, is_window_fullscreen, probe};
use super::ll_hook::LlHook;
use super::tray::{
    Tray, CMD_ABOUT, CMD_OPEN_PICKER, CMD_PAUSE, CMD_QUIT, CMD_RESTART_UPDATE, CMD_SETTINGS,
    WM_TRAY,
};

/// Class of the message-only window; a second instance finds it with `FindWindowExW`.
pub const SERVICE_CLASS: PCWSTR = w!("EmojiPickerService");
/// Class of the hidden top-level window that owns the tray icon (gets `TaskbarCreated`).
const TRAY_CLASS: PCWSTR = w!("EmojiPickerTray");
/// Posted to the service window by a second instance: open settings.
pub const WM_OPEN_SETTINGS: u32 = WM_APP + 1;
/// Thread message that wakes the loop to drain the command channel.
const WM_WAKE: u32 = WM_APP;
/// Posted to the service window by the hook thread: Win+. was pressed.
const WM_HOOK_OPEN: u32 = WM_APP + 3;
/// Posted to the service window when installing the Win+. hook failed.
const WM_HOOK_FAILED: u32 = WM_APP + 4;
const HOTKEY_ID: i32 = 1;
/// Timer that reinstalls the Win+. hook: Windows removes a slow hook without telling us.
const REHOOK_TIMER: usize = 1;
const REHOOK_MS: u32 = 60_000;
/// One-shot timer: retry a failed hook install once before falling back to Ctrl+Shift+Space.
const RETRY_TIMER: usize = 2;
const RETRY_MS: u32 = 1_000;
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;

#[derive(Debug, Clone)]
pub enum UiEvent {
    OpenPicker {
        /// Foreground HWND when the hotkey was pressed (as an integer so it is `Send`).
        target: isize,
        caret: Option<Rect>,
        cursor: Point,
        at: Instant,
        /// Opened by the Win+. hook: the period was swallowed, so this process may lack
        /// foreground rights and the picker retries `SetForegroundWindow`.
        via_hook: bool,
    },
    /// Win+. could not be captured; the service uses this hotkey for the rest of the session.
    /// The saved config keeps Win+., so the next start tries Win+. again.
    HotkeySwitched(Hotkey),
    OpenSettings,
    OpenAbout,
    /// "Restart to update" in the tray menu.
    RestartToUpdate,
    Quit,
    HotkeyFailed,
}

#[derive(Debug)]
pub enum ServiceCmd {
    SetConfig(Arc<Config>),
    #[allow(dead_code)] // sent by the settings window (Task 14)
    SetPaused(bool),
    /// Releases the hotkey while the settings or first-run window records a new one (so the
    /// keys reach the recorder), and arms it again with `false`. Separate from the tray's
    /// pause, which it never changes.
    SuspendHotkey(bool),
    Balloon {
        title: String,
        text: String,
    },
    /// An update (`"1.2.3"`) is downloaded: the tray menu offers "Restart to update".
    UpdateReady(String),
    Shutdown,
}

pub struct ServiceHandle {
    thread_id: u32,
    tx: Sender<ServiceCmd>,
    join: std::cell::RefCell<Option<JoinHandle<()>>>,
}

impl ServiceHandle {
    /// Queues `cmd` and wakes the service loop.
    pub fn send(&self, cmd: ServiceCmd) {
        if self.tx.send(cmd).is_ok() {
            // SAFETY: plain post to a thread that has a message queue (created before spawn returns).
            let _ = unsafe { PostThreadMessageW(self.thread_id, WM_WAKE, WPARAM(0), LPARAM(0)) };
        }
    }

    /// Stops the service (removes the tray icon, releases the hotkey) and waits for it. Later
    /// calls do nothing.
    pub fn shutdown(&self) {
        let Some(join) = self.join.borrow_mut().take() else {
            return;
        };
        self.send(ServiceCmd::Shutdown);
        let _ = join.join();
    }
}

/// Starts the service thread and returns once its windows exist.
pub fn spawn_service(
    cfg: Arc<Config>,
    on_event: impl Fn(UiEvent) + Send + 'static,
) -> ServiceHandle {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let join = std::thread::Builder::new()
        .name("service".into())
        .spawn(move || run(cfg, Box::new(on_event), rx, ready_tx))
        .expect("spawning the service thread failed");
    let thread_id = ready_rx.recv().expect("service thread failed to start");
    ServiceHandle {
        thread_id,
        tx,
        join: std::cell::RefCell::new(Some(join)),
    }
}

struct Service {
    msg_hwnd: HWND,
    tray: Tray,
    taskbar_created: u32,
    rx: Receiver<ServiceCmd>,
    on_event: Box<dyn Fn(UiEvent)>,
    cfg: RefCell<Arc<Config>>,
    /// What the configured hotkey needs.
    mode: Cell<Mode>,
    paused: Cell<bool>,
    /// Downloaded update version, shown as "Restart to update" in the tray menu.
    update_ready: RefCell<Option<String>>,
    /// A hotkey is being recorded (see [`ServiceCmd::SuspendHotkey`]).
    suspended: Cell<bool>,
    /// The hotkey is live: registered (combo) or hook installed (Win+.).
    armed: Cell<bool>,
    /// Keyboard hook thread for Win+.; idle unless `mode` is `WinPeriod`.
    ll_hook: LlHook,
    /// Set after a failure balloon so foreground changes do not repeat it.
    fail_notified: Cell<bool>,
    /// The current hook install is the retry after a failure: a second failure falls back.
    retrying: Cell<bool>,
    /// Session-only stand-in for Win+. after the hook failed twice. Never saved: `cfg` keeps
    /// Win+., and a SetConfig that still says Win+. keeps using this. Cleared when the
    /// configured hotkey changes.
    session_fallback: Cell<Option<Hotkey>>,
    /// Last foreground window of another app, the target for "Open picker" from the tray.
    last_target: Cell<isize>,
    /// Current foreground window and its last probe, so resize events need no exe lookup.
    fg_hwnd: Cell<isize>,
    fg: RefCell<Foreground>,
    /// `EVENT_OBJECT_LOCATIONCHANGE` hook limited to the foreground window's process.
    location_hook: Cell<Option<HWINEVENTHOOK>>,
}

/// The hotkey to arm: the session fallback replaces Win+. only.
fn effective_hotkey(configured: Option<Hotkey>, fallback: Option<Hotkey>) -> Option<Hotkey> {
    match (configured, fallback) {
        (Some(Hotkey::WinPeriod), Some(f)) => Some(f),
        (h, _) => h,
    }
}

/// Out-of-context WinEvent hook for one event, optionally limited to one process.
fn win_event_hook(event: u32, pid: u32) -> HWINEVENTHOOK {
    // SAFETY: out-of-context hook; the callback runs on this thread inside GetMessageW.
    unsafe {
        SetWinEventHook(
            event,
            event,
            None,
            Some(on_win_event),
            pid,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    }
}

fn unhook(hook: Option<HWINEVENTHOOK>) {
    if let Some(hook) = hook.filter(|h| !h.is_invalid()) {
        // SAFETY: hook was returned by SetWinEventHook on this thread.
        let _ = unsafe { UnhookWinEvent(hook) };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// No hotkey: setup unfinished or the combination is invalid.
    Off,
    /// Modifiers and vk for `RegisterHotKey`.
    Combo(HOT_KEY_MODIFIERS, u32),
    /// Win+. through the low-level keyboard hook.
    WinPeriod,
}

thread_local! {
    static SERVICE: RefCell<Option<Rc<Service>>> = const { RefCell::new(None) };
}

fn service() -> Option<Rc<Service>> {
    SERVICE.with(|s| s.borrow().clone())
}

fn register_class(hinstance: HINSTANCE, name: PCWSTR) {
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: name,
        ..Default::default()
    };
    // SAFETY: class is fully initialised; a duplicate registration only fails harmlessly.
    unsafe { RegisterClassExW(&class) };
}

fn load_icon(hinstance: HINSTANCE) -> HICON {
    // SAFETY: plain metric query.
    let cx = unsafe { GetSystemMetrics(SM_CXSMICON) };
    // SAFETY: plain metric query.
    let cy = unsafe { GetSystemMetrics(SM_CYSMICON) };
    // SAFETY: resource id 1 is the app icon embedded by build.rs (MAKEINTRESOURCE form).
    let icon = unsafe {
        LoadImageW(
            Some(hinstance),
            PCWSTR(std::ptr::without_provenance(1)),
            IMAGE_ICON,
            cx,
            cy,
            LR_DEFAULTCOLOR,
        )
    };
    match icon {
        Ok(h) => HICON(h.0),
        // SAFETY: stock system icon as a fallback.
        Err(_) => unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default(),
    }
}

fn run(
    cfg: Arc<Config>,
    on_event: Box<dyn Fn(UiEvent)>,
    rx: Receiver<ServiceCmd>,
    ready: mpsc::SyncSender<u32>,
) {
    // SAFETY: null module name gives this exe's module handle.
    let hinstance: HINSTANCE = unsafe { GetModuleHandleW(None) }
        .expect("GetModuleHandleW failed")
        .into();
    register_class(hinstance, SERVICE_CLASS);
    register_class(hinstance, TRAY_CLASS);
    // SAFETY: registered class, HWND_MESSAGE parent makes it message-only.
    let msg_hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            SERVICE_CLASS,
            PCWSTR::null(),
            Default::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance),
            None,
        )
    }
    .expect("creating the service window failed");
    // SAFETY: registered class; never shown, a tool window keeps it off the taskbar.
    let tray_hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            TRAY_CLASS,
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        )
    }
    .expect("creating the tray window failed");
    // SAFETY: static string.
    let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    let tray = Tray::new(tray_hwnd, load_icon(hinstance));
    let hook = win_event_hook(EVENT_SYSTEM_FOREGROUND, 0);
    // Session lock, unlock and switch: a Win key-up can go missing there. The tray window is a
    // normal (hidden) top-level window, so it gets these notifications.
    // SAFETY: tray_hwnd is our window on this thread.
    if let Err(e) = unsafe { WTSRegisterSessionNotification(tray_hwnd, NOTIFY_FOR_THIS_SESSION) } {
        crate::log!("service: WTSRegisterSessionNotification failed: {e}");
    }
    let hook_target = msg_hwnd.0 as isize;
    let ll_hook = LlHook::spawn(move || {
        // SAFETY: plain post to our message-only window; it never blocks.
        let _ = unsafe {
            PostMessageW(
                Some(HWND(hook_target as *mut _)),
                WM_HOOK_OPEN,
                WPARAM(0),
                LPARAM(0),
            )
        };
    });

    let svc = Rc::new(Service {
        msg_hwnd,
        tray,
        taskbar_created,
        rx,
        on_event,
        cfg: RefCell::new(cfg.clone()),
        mode: Cell::new(Mode::Off),
        paused: Cell::new(false),
        update_ready: RefCell::new(None),
        suspended: Cell::new(false),
        armed: Cell::new(false),
        ll_hook,
        fail_notified: Cell::new(false),
        retrying: Cell::new(false),
        session_fallback: Cell::new(None),
        last_target: Cell::new(0),
        fg_hwnd: Cell::new(0),
        fg: RefCell::new(Foreground {
            exe: None,
            fullscreen: false,
        }),
        location_hook: Cell::new(None),
    });
    SERVICE.with(|s| *s.borrow_mut() = Some(svc.clone()));
    // SAFETY: no arguments.
    let _ = ready.send(unsafe { GetCurrentThreadId() });

    svc.set_config(cfg);
    // Commands may have been queued before SERVICE was set.
    svc.drain();

    let mut msg = MSG::default();
    // SAFETY: standard message loop on this thread's queue.
    // GetMessageW returns 0 on WM_QUIT and -1 on error: stop on both.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        if msg.hwnd.is_invalid() && msg.message == WM_WAKE {
            svc.drain();
            continue;
        }
        // SAFETY: msg was filled by GetMessageW.
        let _ = unsafe { TranslateMessage(&msg) };
        // SAFETY: msg was filled by GetMessageW.
        unsafe { DispatchMessageW(&msg) };
    }

    svc.disarm();
    // SAFETY: registered above on this window.
    let _ = unsafe { WTSUnRegisterSessionNotification(tray_hwnd) };
    unhook(Some(hook));
    unhook(svc.location_hook.take());
    SERVICE.with(|s| s.borrow_mut().take());
    // Dropping the last Rc drops the Tray, which deletes the icon.
    drop(svc);
    // SAFETY: tray_hwnd was created on this thread.
    let _ = unsafe { DestroyWindow(tray_hwnd) };
    // SAFETY: msg_hwnd was created on this thread.
    let _ = unsafe { DestroyWindow(msg_hwnd) };
    crate::log!("service: stopped");
}

fn to_mods(m: emoji_core::hotkey::Mods) -> HOT_KEY_MODIFIERS {
    let mut out = MOD_NOREPEAT;
    for (on, flag) in [
        (m.ctrl, MOD_CONTROL),
        (m.alt, MOD_ALT),
        (m.shift, MOD_SHIFT),
        (m.win, MOD_WIN),
    ] {
        if on {
            out |= flag;
        }
    }
    out
}

impl Service {
    fn emit(&self, event: UiEvent) {
        (self.on_event)(event);
    }

    fn drain(&self) {
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                ServiceCmd::SetConfig(cfg) => self.set_config(cfg),
                ServiceCmd::SetPaused(p) => {
                    self.paused.set(p);
                    self.refresh();
                }
                ServiceCmd::SuspendHotkey(s) => {
                    crate::log!("service: hotkey suspended={s}");
                    self.suspended.set(s);
                    self.refresh();
                }
                ServiceCmd::Balloon { title, text } => self.tray.balloon(&title, &text),
                ServiceCmd::UpdateReady(version) => *self.update_ready.borrow_mut() = Some(version),
                // SAFETY: ends this thread's message loop.
                ServiceCmd::Shutdown => unsafe { PostQuitMessage(0) },
            }
        }
    }

    fn set_config(&self, cfg: Arc<Config>) {
        // A config built before the fallback (still Win+.) must not undo it; a new hotkey does.
        if self.cfg.borrow().hotkey != cfg.hotkey {
            self.session_fallback.set(None);
        }
        *self.cfg.borrow_mut() = cfg;
        self.apply_hotkey();
    }

    /// Re-arms the hotkey from `cfg`, with the session fallback standing in for Win+.
    fn apply_hotkey(&self) {
        self.disarm();
        self.fail_notified.set(false);
        let hotkey = effective_hotkey(self.cfg.borrow().hotkey, self.session_fallback.get());
        let mode = match hotkey {
            Some(h @ Hotkey::Combo { mods, vk }) => match h.validate() {
                Ok(()) => Mode::Combo(to_mods(mods), u32::from(vk)),
                Err(e) => {
                    crate::log!("service: hotkey {h} rejected: {e}");
                    self.fail(&format!("Hotkey {h} cannot be used: {e}"));
                    Mode::Off
                }
            },
            Some(Hotkey::WinPeriod) => Mode::WinPeriod,
            // Setup is unfinished.
            None => Mode::Off,
        };
        self.mode.set(mode);
        self.refresh();
    }

    fn fail(&self, text: &str) {
        if !self.fail_notified.replace(true) {
            self.tray.balloon("Emoji picker", text);
            self.emit(UiEvent::HotkeyFailed);
        }
    }

    fn refresh(&self) {
        // SAFETY: no arguments.
        let fg = unsafe { GetForegroundWindow() };
        self.on_foreground(fg);
    }

    /// Probes the new foreground window, follows its resizes, then updates the hotkey.
    fn on_foreground(&self, fg: HWND) {
        let info = probe(fg);
        if self.fg_hwnd.replace(fg.0 as isize) != fg.0 as isize {
            let mut pid = 0u32;
            // SAFETY: fg comes from the system; a stale one gives pid 0.
            unsafe { GetWindowThreadProcessId(fg, Some(&mut pid)) };
            unhook(self.location_hook.take());
            if pid != 0 {
                let hook = win_event_hook(EVENT_OBJECT_LOCATIONCHANGE, pid);
                self.location_hook.set(Some(hook));
            }
        }
        *self.fg.borrow_mut() = info;
        self.update_hotkey("foreground");
    }

    /// The foreground window moved or resized (F11, video fullscreen, borderless games).
    fn on_location_change(&self, hwnd: HWND) {
        if hwnd.0 as isize != self.fg_hwnd.get() {
            return;
        }
        let fullscreen = is_window_fullscreen(hwnd);
        if self.fg.borrow().fullscreen == fullscreen {
            return;
        }
        self.fg.borrow_mut().fullscreen = fullscreen;
        self.update_hotkey("resize");
    }

    /// Arms or disarms the hotkey for the cached foreground state.
    fn update_hotkey(&self, why: &str) {
        let mode = self.mode.get();
        if mode == Mode::Off {
            self.disarm();
            return;
        }
        let info = self.fg.borrow().clone();
        let cfg = self.cfg.borrow().clone();
        let allowed = hotkey_allowed(&info, &cfg, self.paused.get() || self.suspended.get());
        crate::log!(
            "service: {why} {:?} fullscreen={} allowed={allowed}",
            info.exe.as_deref().map(exe_file_name),
            info.fullscreen
        );
        if !allowed {
            self.disarm();
            return;
        }
        if self.armed.get() {
            return;
        }
        match mode {
            Mode::Off => {}
            Mode::Combo(mods, vk) => {
                // SAFETY: msg_hwnd is our window on this thread.
                match unsafe { RegisterHotKey(Some(self.msg_hwnd), HOTKEY_ID, mods, vk) } {
                    Ok(()) => {
                        self.armed.set(true);
                        self.fail_notified.set(false);
                    }
                    Err(e) => {
                        crate::log!("service: RegisterHotKey failed: {e}");
                        if self.session_fallback.get().is_some() {
                            self.fail(&format!(
                                "Win+. could not be captured and {} is taken by another app",
                                Hotkey::presets()[1]
                            ));
                        } else {
                            self.fail("Hotkey is taken by another app");
                        }
                    }
                }
            }
            Mode::WinPeriod => self.arm_hook(),
        }
    }

    /// Installs the Win+. hook and starts its reinstall timer. A refused install is retried
    /// once after a second; only a failed retry falls back to Ctrl+Shift+Space.
    fn arm_hook(&self) {
        match self.ll_hook.install() {
            Some(true) => {}
            // The hook thread did not answer in time; it still installs. Keep Win+.: a real
            // failure shows up at the next reinstall.
            None => crate::log!("service: hook install unconfirmed, keeping Win+."),
            Some(false) => {
                if self.retrying.get() {
                    // Switch presets outside this call chain (apply_hotkey runs it again).
                    // SAFETY: plain post to our own window.
                    let _ = unsafe {
                        PostMessageW(Some(self.msg_hwnd), WM_HOOK_FAILED, WPARAM(0), LPARAM(0))
                    };
                } else {
                    crate::log!("service: hook install failed, retrying in {RETRY_MS} ms");
                    // SAFETY: msg_hwnd is our window on this thread.
                    unsafe { SetTimer(Some(self.msg_hwnd), RETRY_TIMER, RETRY_MS, None) };
                }
                return;
            }
        }
        self.armed.set(true);
        // SAFETY: msg_hwnd is our window on this thread; same id replaces the timer.
        unsafe { SetTimer(Some(self.msg_hwnd), REHOOK_TIMER, REHOOK_MS, None) };
    }

    /// The one-shot retry after a failed install.
    fn retry_hook(&self) {
        // SAFETY: the timer was set on msg_hwnd by this thread.
        let _ = unsafe { KillTimer(Some(self.msg_hwnd), RETRY_TIMER) };
        if self.mode.get() != Mode::WinPeriod || self.armed.get() {
            return;
        }
        self.retrying.set(true);
        self.update_hotkey("hook retry");
        self.retrying.set(false);
    }

    /// Releases the hotkey (unregister, or remove the hook and its reinstall timer).
    fn disarm(&self) {
        if !self.armed.replace(false) {
            return;
        }
        match self.mode.get() {
            Mode::Off => {}
            Mode::Combo(..) => {
                // SAFETY: the id was registered on msg_hwnd by this thread.
                let _ = unsafe { UnregisterHotKey(Some(self.msg_hwnd), HOTKEY_ID) };
            }
            Mode::WinPeriod => {
                // SAFETY: the timer, if any, was set on msg_hwnd by this thread.
                let _ = unsafe { KillTimer(Some(self.msg_hwnd), REHOOK_TIMER) };
                self.ll_hook.uninstall();
            }
        }
    }

    /// Periodic reinstall while Win+. is live. Skipped while a Win key is down: a reinstall
    /// resets the matcher and would lose the Start menu mask for that press.
    fn rehook(&self) {
        if self.mode.get() != Mode::WinPeriod || !self.armed.get() {
            return;
        }
        // SAFETY: plain key state query.
        let lwin = unsafe { GetAsyncKeyState(i32::from(VK_LWIN)) };
        // SAFETY: plain key state query.
        let rwin = unsafe { GetAsyncKeyState(i32::from(VK_RWIN)) };
        if lwin < 0 || rwin < 0 {
            return;
        }
        if self.ll_hook.install() == Some(false) {
            // The failed install already removed the old hook: disarm and retry once.
            self.armed.set(false);
            // SAFETY: the timer was set on msg_hwnd by this thread.
            let _ = unsafe { KillTimer(Some(self.msg_hwnd), REHOOK_TIMER) };
            crate::log!("service: hook reinstall failed, retrying in {RETRY_MS} ms");
            // SAFETY: msg_hwnd is our window on this thread.
            unsafe { SetTimer(Some(self.msg_hwnd), RETRY_TIMER, RETRY_MS, None) };
        }
    }

    /// Installing the hook failed: tell the user and use Ctrl+Shift+Space for this session.
    /// The config is not changed, so the next start tries Win+. again.
    fn hook_failed(&self) {
        if self.mode.get() != Mode::WinPeriod || self.armed.get() || !self.ll_hook.is_lost() {
            return;
        }
        let fallback = Hotkey::presets()[1];
        crate::log!("service: Win+. hook failed twice, using {fallback} until restart");
        self.session_fallback.set(Some(fallback));
        self.apply_hotkey();
        // If registering the fallback failed, `fail` already showed the combined text.
        if !self.fail_notified.get() {
            self.tray.balloon(
                "Emoji picker",
                &format!("Win+. could not be captured, using {fallback} until restart"),
            );
        }
        // The UI only shows this in settings; it does not save it.
        self.emit(UiEvent::HotkeySwitched(fallback));
    }

    fn track_target(&self, hwnd: HWND) {
        let mut pid = 0u32;
        // SAFETY: hwnd comes from the foreground event; a stale one gives pid 0.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        // SAFETY: no arguments.
        let own = pid == unsafe { GetCurrentProcessId() };
        let shell = probe_is_taskbar(hwnd);
        if pid != 0 && !own && !shell {
            self.last_target.set(hwnd.0 as isize);
        }
    }

    fn open_picker(&self, target: isize, via_hook: bool) {
        let at = Instant::now();
        let hwnd = HWND(target as *mut _);
        self.emit(UiEvent::OpenPicker {
            target,
            caret: caret_rect(hwnd),
            cursor: cursor(),
            at,
            via_hook,
        });
    }

    fn on_command(&self, id: usize) {
        match id {
            CMD_OPEN_PICKER => self.open_picker(self.last_target.get(), false),
            CMD_SETTINGS => self.emit(UiEvent::OpenSettings),
            CMD_PAUSE => {
                self.paused.set(!self.paused.get());
                self.refresh();
            }
            CMD_ABOUT => self.emit(UiEvent::OpenAbout),
            CMD_QUIT => self.emit(UiEvent::Quit),
            CMD_RESTART_UPDATE => self.emit(UiEvent::RestartToUpdate),
            _ => {}
        }
    }
}

/// The taskbar takes the foreground when the tray icon is clicked; it is not a target.
fn probe_is_taskbar(hwnd: HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    let mut buf = [0u16; 64];
    // SAFETY: buf is a valid writable slice.
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    let class = String::from_utf16_lossy(&buf[..len.max(0) as usize]);
    matches!(
        class.as_str(),
        "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "NotifyIconOverflowWindow"
    )
}

unsafe extern "system" fn on_win_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object != OBJID_WINDOW.0 || id_child != CHILDID_SELF as i32 {
        return;
    }
    // A panic must not unwind across this extern "system" boundary: log it and drop the event.
    if catch_unwind(AssertUnwindSafe(|| win_event(event, hwnd))).is_err() {
        crate::log!("service: panic in WinEvent callback (event {event:#x}), event ignored");
    }
}

fn win_event(event: u32, hwnd: HWND) {
    let Some(svc) = service() else {
        return;
    };
    match event {
        EVENT_SYSTEM_FOREGROUND => {
            svc.track_target(hwnd);
            svc.on_foreground(hwnd);
        }
        EVENT_OBJECT_LOCATIONCHANGE => svc.on_location_change(hwnd),
        _ => {}
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // A panic must not unwind across this extern "system" boundary: log it and let Windows
    // handle the message.
    match catch_unwind(AssertUnwindSafe(|| handle_message(hwnd, msg, wp, lp))) {
        Ok(res) => res,
        Err(_) => {
            crate::log!("service: panic handling message {msg:#x}, passed to DefWindowProcW");
            // SAFETY: default handling with the arguments Windows gave us.
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
    }
}

fn handle_message(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(svc) = service() else {
        // SAFETY: default handling for messages before the service is set up.
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    };
    match msg {
        WM_HOTKEY if wp.0 == HOTKEY_ID as usize => {
            // SAFETY: no arguments.
            let fg = unsafe { GetForegroundWindow() };
            // Re-check: the window may have gone fullscreen without an event we acted on.
            svc.on_foreground(fg);
            if svc.armed.get() {
                svc.open_picker(fg.0 as isize, false);
            }
        }
        WM_HOOK_OPEN => {
            // SAFETY: no arguments.
            let fg = unsafe { GetForegroundWindow() };
            // Same re-check as WM_HOTKEY; a newly blocked window also loses the hook here.
            svc.on_foreground(fg);
            if svc.armed.get() && svc.mode.get() == Mode::WinPeriod {
                // Lets the UI thread take the foreground if this process may (the hook ate
                // the key, so the target app got the last input, not us).
                // SAFETY: plain call; fails harmlessly without foreground rights.
                let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
                svc.open_picker(fg.0 as isize, true);
            } else {
                crate::log!("service: Win+. arrived while blocked, ignored");
            }
        }
        WM_HOOK_FAILED => svc.hook_failed(),
        WM_TIMER if wp.0 == REHOOK_TIMER => svc.rehook(),
        WM_TIMER if wp.0 == RETRY_TIMER => svc.retry_hook(),
        WM_WTSSESSION_CHANGE => {
            crate::log!("service: session change {}", wp.0);
            svc.ll_hook.reset();
        }
        WM_OPEN_SETTINGS => svc.emit(UiEvent::OpenSettings),
        WM_TRAY => match (lp.0 & 0xFFFF) as u32 {
            WM_CONTEXTMENU => {
                let update = svc.update_ready.borrow().clone();
                svc.tray.show_menu(svc.paused.get(), update.as_deref());
                // Thread wake messages posted during the modal menu loop are dropped.
                svc.drain();
            }
            NIN_SELECT | NIN_KEYSELECT => svc.open_picker(svc.last_target.get(), false),
            _ => {}
        },
        WM_COMMAND => svc.on_command(wp.0 & 0xFFFF),
        m if m == svc.taskbar_created && m != 0 => {
            svc.tray.add();
        }
        // SAFETY: default handling for everything else.
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
    LRESULT(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_replaces_only_win_period() {
        let fallback = Hotkey::presets()[1];
        let other: Hotkey = "Ctrl+Alt+K".parse().unwrap();
        assert_eq!(
            effective_hotkey(Some(Hotkey::WinPeriod), Some(fallback)),
            Some(fallback)
        );
        assert_eq!(
            effective_hotkey(Some(Hotkey::WinPeriod), None),
            Some(Hotkey::WinPeriod)
        );
        assert_eq!(effective_hotkey(Some(other), Some(fallback)), Some(other));
        assert_eq!(effective_hotkey(None, Some(fallback)), None);
    }
}
