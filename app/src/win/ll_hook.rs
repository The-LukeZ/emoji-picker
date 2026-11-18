//! Win+. through a `WH_KEYBOARD_LL` hook on its own thread.
//!
//! Windows owns Win+. so `RegisterHotKey` cannot take it. The hook callback only runs the
//! [`WinPeriodMatcher`] and posts: Windows silently removes a low-level hook whose callback
//! takes longer than `LowLevelHooksTimeout`, so no other work happens there.

use std::cell::{Cell, OnceCell, RefCell};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use emoji_core::hook_matcher::{
    HookAction, Physical, WinPeriodMatcher, VK_LWIN, VK_MASK, VK_OEM_PERIOD, VK_RWIN,
};
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PeekMessageW, PostThreadMessageW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT,
    LLKHF_INJECTED, MSG, PM_NOREMOVE, WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN, WM_QUIT, WM_SYSKEYDOWN,
};

const WM_INSTALL: u32 = WM_APP + 10;
const WM_UNINSTALL: u32 = WM_APP + 11;
const WM_RESET: u32 = WM_APP + 12;
/// How long `install` waits for the hook thread to report the result.
const INSTALL_WAIT: Duration = Duration::from_millis(500);

thread_local! {
    // All three live on the hook thread only: the callback runs there inside GetMessageW.
    static MATCHER: RefCell<WinPeriodMatcher> = RefCell::new(WinPeriodMatcher::default());
    static ON_OPEN: OnceCell<Box<dyn Fn()>> = const { OnceCell::new() };
    static HOOK: Cell<Option<HHOOK>> = const { Cell::new(None) };
    /// Test mode (debug and `diag` builds only): see [`TEST_ENV`].
    static TEST_MODE: Cell<bool> = const { Cell::new(false) };
}

/// `dwExtraInfo` of every key this app injects ("EMOJ"), so the matcher always ignores it.
pub const OWN_INPUT_MARKER: usize = 0x454D_4F4A;
/// Debug and `diag` builds: `EMOJI_PICKER_HOOK_TEST=1` makes the hook treat injected keys as
/// physical (except our own, marked with [`OWN_INPUT_MARKER`]), so a desktop test can drive
/// Win+. with `SendInput`. Release builds without `diag` always pass injected keys through.
const TEST_ENV: &str = "EMOJI_PICKER_HOOK_TEST";

fn test_mode_requested() -> bool {
    cfg!(any(debug_assertions, feature = "diag"))
        && std::env::var_os(TEST_ENV).is_some_and(|v| v == "1")
}

/// Handle to the hook thread. Dropping it removes the hook and stops the thread.
pub struct LlHook {
    thread_id: u32,
    lost: Arc<AtomicBool>,
    results: Receiver<bool>,
    join: Option<JoinHandle<()>>,
}

impl LlHook {
    /// Starts the hook thread (no hook installed yet). `on_open` runs inside the hook
    /// callback when Win+. is pressed: it must only post a message and return.
    pub fn spawn(on_open: impl Fn() + Send + 'static) -> LlHook {
        let lost = Arc::new(AtomicBool::new(false));
        let (results_tx, results) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread_lost = lost.clone();
        let join = std::thread::Builder::new()
            .name("ll-hook".into())
            .spawn(move || run(Box::new(on_open), thread_lost, results_tx, ready_tx))
            .expect("spawning the hook thread failed");
        let thread_id = ready_rx.recv().expect("hook thread failed to start");
        LlHook {
            thread_id,
            lost,
            results,
            join: Some(join),
        }
    }

    /// (Re)installs the hook with a fresh matcher. Posts to the hook thread, then waits up to
    /// half a second for the result: `Some(true)` installed, `Some(false)` Windows refused,
    /// `None` no answer in time (the hook thread will still finish the install).
    pub fn install(&self) -> Option<bool> {
        while self.results.try_recv().is_ok() {}
        self.lost.store(false, Ordering::SeqCst);
        if !self.post(WM_INSTALL) {
            self.lost.store(true, Ordering::SeqCst);
            return Some(false);
        }
        let result = self.results.recv_timeout(INSTALL_WAIT).ok();
        if result.is_none() {
            crate::log!("ll-hook: no install result within {INSTALL_WAIT:?}");
        }
        result
    }

    /// Removes the hook. Posts to the hook thread and returns at once.
    pub fn uninstall(&self) {
        self.post(WM_UNINSTALL);
    }

    /// Forgets the Win key state (session lock or switch can hide a Win key-up).
    pub fn reset(&self) {
        self.post(WM_RESET);
    }

    /// True when the last install failed.
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    fn post(&self, msg: u32) -> bool {
        // SAFETY: plain post to a thread whose queue exists (created before spawn returned).
        unsafe { PostThreadMessageW(self.thread_id, msg, WPARAM(0), LPARAM(0)) }.is_ok()
    }
}

impl Drop for LlHook {
    fn drop(&mut self) {
        self.post(WM_QUIT);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn run(
    on_open: Box<dyn Fn()>,
    lost: Arc<AtomicBool>,
    results: Sender<bool>,
    ready: mpsc::SyncSender<u32>,
) {
    let mut msg = MSG::default();
    // SAFETY: creates this thread's message queue so posts sent right after `ready` land.
    let _ = unsafe { PeekMessageW(&mut msg, None, WM_APP, WM_APP, PM_NOREMOVE) };
    if test_mode_requested() {
        crate::log!("ll-hook: test mode, injected keys count as physical");
        TEST_MODE.with(|t| t.set(true));
    }
    ON_OPEN.with(|cell| {
        let _ = cell.set(on_open);
    });
    // SAFETY: no arguments.
    let _ = ready.send(unsafe { GetCurrentThreadId() });

    // SAFETY: standard message loop on this thread's queue.
    // GetMessageW returns 0 on WM_QUIT and -1 on error: stop on both.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        if msg.hwnd.is_invalid() {
            match msg.message {
                WM_INSTALL => {
                    let ok = install_now();
                    lost.store(!ok, Ordering::SeqCst);
                    let _ = results.send(ok);
                    continue;
                }
                WM_UNINSTALL => {
                    uninstall_now();
                    continue;
                }
                WM_RESET => {
                    reset_matcher();
                    continue;
                }
                _ => {}
            }
        }
        // SAFETY: msg was filled by GetMessageW.
        let _ = unsafe { TranslateMessage(&msg) };
        // SAFETY: msg was filled by GetMessageW.
        unsafe { DispatchMessageW(&msg) };
    }
    uninstall_now();
    crate::log!("ll-hook: stopped");
}

fn reset_matcher() {
    MATCHER.with(|m| {
        if let Ok(mut m) = m.try_borrow_mut() {
            m.reset();
        }
    });
}

/// Replaces any current hook with a new one. False when Windows refused.
fn install_now() -> bool {
    uninstall_now();
    reset_matcher();
    // SAFETY: null module name gives this exe's module handle.
    let module: Option<HINSTANCE> = unsafe { GetModuleHandleW(None) }.ok().map(Into::into);
    // SAFETY: global low-level hook; the callback runs on this thread inside GetMessageW.
    match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_proc), module, 0) } {
        Ok(hook) => {
            HOOK.with(|h| h.set(Some(hook)));
            crate::log!("ll-hook: installed");
            true
        }
        Err(e) => {
            crate::log!("ll-hook: SetWindowsHookExW failed: {e}");
            false
        }
    }
}

fn uninstall_now() {
    if let Some(hook) = HOOK.with(|h| h.take()) {
        // SAFETY: hook was returned by SetWindowsHookExW on this thread.
        let _ = unsafe { UnhookWindowsHookEx(hook) };
        crate::log!("ll-hook: uninstalled");
    }
}

/// High bit of `GetAsyncKeyState`: the key is physically down right now.
fn key_down(vk: u16) -> bool {
    // SAFETY: plain key state query.
    let state = unsafe { GetAsyncKeyState(i32::from(vk)) };
    state < 0
}

fn key_input(vk: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: OWN_INPUT_MARKER,
            },
        },
    }
}

/// Injects a down and up of the unassigned `VK_MASK` (0xE8). Before a Win release it keeps
/// the Start menu shut; it also counts as input from this process for foreground rights.
pub fn tap_mask_key() {
    let inputs = [
        key_input(VK_MASK, KEYBD_EVENT_FLAGS(0)),
        key_input(VK_MASK, KEYEVENTF_KEYUP),
    ];
    // SAFETY: inputs is a valid slice of initialised INPUT structs of the given size.
    let _ = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
}

/// Matches one key event. Returns true when the event must be eaten.
fn on_key(kb: &KBDLLHOOKSTRUCT, wp: WPARAM) -> bool {
    let down = matches!(wp.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
    let own = kb.dwExtraInfo == OWN_INPUT_MARKER;
    let injected = own || ((kb.flags & LLKHF_INJECTED).0 != 0 && !TEST_MODE.with(|t| t.get()));
    let vk = u16::try_from(kb.vkCode).unwrap_or(0);
    // Physical state is only needed for a period key-down (the matcher's resync rule).
    let phys = if vk == VK_OEM_PERIOD && down {
        Physical {
            lwin: key_down(VK_LWIN),
            rwin: key_down(VK_RWIN),
            period: key_down(VK_OEM_PERIOD),
        }
    } else {
        Physical::default()
    };
    let action = MATCHER.with(|m| match m.try_borrow_mut() {
        Ok(mut m) => m.on_key_checked(vk, down, injected, phys),
        Err(_) => HookAction::Pass,
    });
    if TEST_MODE.with(|t| t.get()) {
        // Test mode only (debug and `diag` builds): trace every key, so a log shows whether
        // the hook goes deaf (for example while the picker has the foreground).
        crate::log!(
            "ll-hook: vk={vk:#x} down={down} flags={:#x} own={own} injected={injected} action={action:?} lwin_async={}",
            kb.flags.0,
            key_down(VK_LWIN)
        );
    }
    match action {
        HookAction::Pass => false,
        HookAction::Swallow => true,
        HookAction::Open => {
            // The matcher learns Win released only from a key-up it saw; Win+L, the secure
            // desktop or a session switch can hide that. Trust it only if Win is down now.
            if key_down(VK_LWIN) || key_down(VK_RWIN) {
                ON_OPEN.with(|f| {
                    if let Some(f) = f.get() {
                        f();
                    }
                });
                // Mask now, while Win is still held: the Win key-up may never reach this
                // hook (see `WinPeriodMatcher::on_key_checked`), and a mask injected there
                // lands after the key-up.
                tap_mask_key();
                true
            } else {
                reset_matcher();
                false
            }
        }
        HookAction::MaskThenPass => {
            tap_mask_key();
            false
        }
    }
}

unsafe extern "system" fn ll_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lp.0 != 0 {
        // SAFETY: for HC_ACTION, lParam points to a KBDLLHOOKSTRUCT valid during this call.
        let kb = unsafe { *(lp.0 as *const KBDLLHOOKSTRUCT) };
        // A panic must not unwind across this extern "system" boundary: pass the key on.
        if catch_unwind(AssertUnwindSafe(|| on_key(&kb, wp))).unwrap_or(false) {
            return LRESULT(1);
        }
    }
    // SAFETY: forwards the event unchanged to the next hook.
    unsafe { CallNextHookEx(None, code, wp, lp) }
}
