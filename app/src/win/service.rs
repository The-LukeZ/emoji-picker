//! The service thread: message-only window, global hotkey, foreground tracking and tray.
//!
//! Everything Win32 that must keep running while the UI is idle lives on this one thread.
//! The UI thread talks to it through [`ServiceHandle`]; it reports back through `on_event`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use emoji_core::config::Config;
use emoji_core::geometry::{Point, Rect};
use emoji_core::hotkey::Hotkey;
use emoji_core::policy::{exe_file_name, hotkey_allowed};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::Shell::NIN_SELECT;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow,
    GetMessageW, GetSystemMetrics, GetWindowThreadProcessId, LoadIconW, LoadImageW,
    PostQuitMessage, PostThreadMessageW, RegisterClassExW, RegisterWindowMessageW,
    TranslateMessage, CHILDID_SELF, EVENT_SYSTEM_FOREGROUND, HICON, HWND_MESSAGE, IDI_APPLICATION,
    IMAGE_ICON, LR_DEFAULTCOLOR, MSG, OBJID_WINDOW, SM_CXSMICON, SM_CYSMICON,
    WINEVENT_OUTOFCONTEXT, WM_APP, WM_COMMAND, WM_CONTEXTMENU, WM_HOTKEY, WNDCLASSEXW,
    WS_EX_TOOLWINDOW, WS_POPUP,
};

use super::foreground::{caret_rect, cursor, probe};
use super::tray::{Tray, CMD_ABOUT, CMD_OPEN_PICKER, CMD_PAUSE, CMD_QUIT, CMD_SETTINGS, WM_TRAY};

/// Class of the message-only window; a second instance finds it with `FindWindowExW`.
pub const SERVICE_CLASS: PCWSTR = w!("EmojiPickerService");
/// Class of the hidden top-level window that owns the tray icon (gets `TaskbarCreated`).
const TRAY_CLASS: PCWSTR = w!("EmojiPickerTray");
/// Posted to the service window by a second instance: open settings.
pub const WM_OPEN_SETTINGS: u32 = WM_APP + 1;
/// Thread message that wakes the loop to drain the command channel.
const WM_WAKE: u32 = WM_APP;
const HOTKEY_ID: i32 = 1;
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;

#[derive(Debug, Clone)]
pub enum UiEvent {
    OpenPicker {
        /// Foreground HWND when the hotkey was pressed (as an integer so it is `Send`).
        target: isize,
        caret: Option<Rect>,
        cursor: Point,
        at: Instant,
    },
    OpenSettings,
    OpenAbout,
    Quit,
    HotkeyFailed,
}

#[derive(Debug)]
pub enum ServiceCmd {
    #[allow(dead_code)] // sent by the settings window (Task 14)
    SetConfig(Arc<Config>),
    #[allow(dead_code)] // sent by the settings window (Task 14)
    SetPaused(bool),
    Balloon {
        title: String,
        text: String,
    },
    Shutdown,
}

pub struct ServiceHandle {
    thread_id: u32,
    tx: Sender<ServiceCmd>,
    join: Option<JoinHandle<()>>,
}

impl ServiceHandle {
    /// Queues `cmd` and wakes the service loop.
    pub fn send(&self, cmd: ServiceCmd) {
        if self.tx.send(cmd).is_ok() {
            // SAFETY: plain post to a thread that has a message queue (created before spawn returns).
            let _ = unsafe { PostThreadMessageW(self.thread_id, WM_WAKE, WPARAM(0), LPARAM(0)) };
        }
    }

    /// Stops the service (removes the tray icon, releases the hotkey) and waits for it.
    pub fn shutdown(mut self) {
        self.send(ServiceCmd::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
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
        join: Some(join),
    }
}

struct Service {
    msg_hwnd: HWND,
    tray: Tray,
    taskbar_created: u32,
    rx: Receiver<ServiceCmd>,
    on_event: Box<dyn Fn(UiEvent)>,
    cfg: RefCell<Arc<Config>>,
    /// Modifiers and vk to register; `None` when off, Win+. (Task 13) or invalid.
    combo: Cell<Option<(HOT_KEY_MODIFIERS, u32)>>,
    paused: Cell<bool>,
    registered: Cell<bool>,
    /// Set after a failure balloon so foreground changes do not repeat it.
    fail_notified: Cell<bool>,
    /// Last foreground window of another app, the target for "Open picker" from the tray.
    last_target: Cell<isize>,
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
    // SAFETY: out-of-context hook; the callback runs on this thread inside GetMessageW.
    let hook: HWINEVENTHOOK = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    };

    let svc = Rc::new(Service {
        msg_hwnd,
        tray,
        taskbar_created,
        rx,
        on_event,
        cfg: RefCell::new(cfg.clone()),
        combo: Cell::new(None),
        paused: Cell::new(false),
        registered: Cell::new(false),
        fail_notified: Cell::new(false),
        last_target: Cell::new(0),
    });
    SERVICE.with(|s| *s.borrow_mut() = Some(svc.clone()));
    // SAFETY: no arguments.
    let _ = ready.send(unsafe { GetCurrentThreadId() });

    svc.set_config(cfg);
    // Commands may have been queued before SERVICE was set.
    svc.drain();

    let mut msg = MSG::default();
    // SAFETY: standard message loop on this thread's queue.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        if msg.hwnd.is_invalid() && msg.message == WM_WAKE {
            svc.drain();
            continue;
        }
        // SAFETY: msg was filled by GetMessageW.
        let _ = unsafe { TranslateMessage(&msg) };
        // SAFETY: msg was filled by GetMessageW.
        unsafe { DispatchMessageW(&msg) };
    }

    svc.unregister();
    if !hook.is_invalid() {
        // SAFETY: hook was returned by SetWinEventHook on this thread.
        let _ = unsafe { UnhookWinEvent(hook) };
    }
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
                ServiceCmd::Balloon { title, text } => self.tray.balloon(&title, &text),
                // SAFETY: ends this thread's message loop.
                ServiceCmd::Shutdown => unsafe { PostQuitMessage(0) },
            }
        }
    }

    fn set_config(&self, cfg: Arc<Config>) {
        self.unregister();
        self.fail_notified.set(false);
        let combo = match cfg.hotkey {
            Some(h @ Hotkey::Combo { mods, vk }) => match h.validate() {
                Ok(()) => Some((to_mods(mods), u32::from(vk))),
                Err(e) => {
                    crate::log!("service: hotkey {h} rejected: {e}");
                    self.fail(&format!("Hotkey {h} cannot be used: {e}"));
                    None
                }
            },
            // Win+. is handled by the keyboard hook (Task 13); None means setup is unfinished.
            Some(Hotkey::WinPeriod) | None => None,
        };
        self.combo.set(combo);
        *self.cfg.borrow_mut() = cfg;
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

    /// Registers or releases the hotkey for the new foreground window.
    fn on_foreground(&self, fg: HWND) {
        let Some((mods, vk)) = self.combo.get() else {
            self.unregister();
            return;
        };
        let info = probe(fg);
        let cfg = self.cfg.borrow().clone();
        let allowed = hotkey_allowed(&info, &cfg, self.paused.get());
        crate::log!(
            "service: foreground {:?} fullscreen={} allowed={allowed}",
            info.exe.as_deref().map(exe_file_name),
            info.fullscreen
        );
        if !allowed {
            self.unregister();
            return;
        }
        if self.registered.get() {
            return;
        }
        // SAFETY: msg_hwnd is our window on this thread.
        match unsafe { RegisterHotKey(Some(self.msg_hwnd), HOTKEY_ID, mods, vk) } {
            Ok(()) => {
                self.registered.set(true);
                self.fail_notified.set(false);
            }
            Err(e) => {
                crate::log!("service: RegisterHotKey failed: {e}");
                self.fail("Hotkey is taken by another app");
            }
        }
    }

    fn unregister(&self) {
        if self.registered.replace(false) {
            // SAFETY: the id was registered on msg_hwnd by this thread.
            let _ = unsafe { UnregisterHotKey(Some(self.msg_hwnd), HOTKEY_ID) };
        }
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

    fn open_picker(&self, target: isize) {
        let at = Instant::now();
        let hwnd = HWND(target as *mut _);
        self.emit(UiEvent::OpenPicker {
            target,
            caret: caret_rect(hwnd),
            cursor: cursor(),
            at,
        });
    }

    fn on_command(&self, id: usize) {
        match id {
            CMD_OPEN_PICKER => self.open_picker(self.last_target.get()),
            CMD_SETTINGS => self.emit(UiEvent::OpenSettings),
            CMD_PAUSE => {
                self.paused.set(!self.paused.get());
                self.refresh();
            }
            CMD_ABOUT => self.emit(UiEvent::OpenAbout),
            CMD_QUIT => self.emit(UiEvent::Quit),
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

unsafe extern "system" fn on_foreground(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if event != EVENT_SYSTEM_FOREGROUND
        || id_object != OBJID_WINDOW.0
        || id_child != CHILDID_SELF as i32
    {
        return;
    }
    if let Some(svc) = service() {
        svc.track_target(hwnd);
        svc.on_foreground(hwnd);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(svc) = service() else {
        // SAFETY: default handling for messages before the service is set up.
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    };
    match msg {
        WM_HOTKEY if wp.0 == HOTKEY_ID as usize => {
            // SAFETY: no arguments.
            let fg = unsafe { GetForegroundWindow() };
            svc.open_picker(fg.0 as isize);
        }
        WM_OPEN_SETTINGS => svc.emit(UiEvent::OpenSettings),
        WM_TRAY => match (lp.0 & 0xFFFF) as u32 {
            WM_CONTEXTMENU => {
                svc.tray.show_menu(svc.paused.get());
                // Thread wake messages posted during the modal menu loop are dropped.
                svc.drain();
            }
            NIN_SELECT | NIN_KEYSELECT => svc.open_picker(svc.last_target.get()),
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
