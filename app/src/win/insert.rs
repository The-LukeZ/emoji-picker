//! Puts the picked emoji into the window that was in front before the picker opened.
//!
//! Runs on the UI thread right after the picker hid. Restoring the foreground waits at most
//! [`FOCUS_TIMEOUT`]; opening the clipboard retries for at most about 50 ms. The clipboard
//! restore after a paste runs later on a Slint timer, so nothing here blocks for long.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use emoji_core::config::InsertMode;
use emoji_core::policy::exe_file_name;
use slint::{Timer, TimerMode};
use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcess, GetCurrentThreadId, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC,
    VIRTUAL_KEY, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU,
    VK_RSHIFT, VK_RWIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsWindow, SetForegroundWindow,
};

use super::foreground::exe_path;
use super::ll_hook::OWN_INPUT_MARKER;
use crate::picker::InsertRequest;

/// How long `insert` waits for the target to become the foreground window again.
const FOCUS_TIMEOUT: Duration = Duration::from_millis(150);
/// Time the target app gets to read the clipboard after Ctrl+V before the old text returns.
const RESTORE_DELAY: Duration = Duration::from_millis(300);
/// `CF_UNICODETEXT` from winuser.h (avoids the large `Win32_System_Ole` feature).
const CF_UNICODETEXT: u32 = 13;
/// Unassigned virtual key the app taps before releasing Alt or Win (see `ll_hook`).
const VK_MASK: u16 = 0xE8;
/// The `V` key.
const VK_V: u16 = 0x56;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The target runs elevated and we do not, so UIPI would drop our input.
    Elevated,
    /// The target window is closed or did not come back to the front.
    TargetGone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertResult {
    Typed,
    Pasted,
    /// The text is only on the clipboard; the caller tells the user why.
    CopiedOnly(Reason),
}

impl Reason {
    /// Balloon text for a [`InsertResult::CopiedOnly`] result.
    pub fn balloon_text(self) -> &'static str {
        match self {
            Reason::Elevated => "Can't type into admin windows, emoji copied to clipboard",
            Reason::TargetGone => "Target window is gone, emoji copied to clipboard",
        }
    }
}

/// A clipboard restore that waits for its timer.
struct PendingRestore {
    /// The text that was on the clipboard before the first paste (`None`: there was none).
    saved: Option<String>,
    /// `GetClipboardSequenceNumber` right after we set the emoji. Any other value means the
    /// clipboard changed since, and then it is left alone.
    seq: u32,
}

thread_local! {
    static PENDING: RefCell<Option<PendingRestore>> = const { RefCell::new(None) };
    static RESTORE_TIMER: Timer = Timer::default();
    /// Hidden window that owns the clipboard while we write it.
    static CLIP_OWNER: isize = super::sys::create_owner_window();
}

fn key(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                // Tagged so the Win+. hook ignores our own input.
                dwExtraInfo: OWN_INPUT_MARKER,
            },
        },
    }
}

/// A virtual-key event with its scan code, as a real keyboard would send it.
fn vk_key(vk: u16, up: bool) -> INPUT {
    // SAFETY: plain table lookup.
    let scan = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) } as u16;
    let mut flags = if up {
        KEYEVENTF_KEYUP
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
    if matches!(VIRTUAL_KEY(vk), VK_RCONTROL | VK_RMENU | VK_LWIN | VK_RWIN) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    key(vk, scan, flags)
}

/// One `KEYEVENTF_UNICODE` down and up per UTF-16 code unit of `text`, so surrogate pairs,
/// ZWJ sequences, variation selectors and keycaps arrive exactly as given.
pub fn unicode_inputs(text: &str) -> Vec<INPUT> {
    text.encode_utf16()
        .flat_map(|u| {
            [
                key(0, u, KEYEVENTF_UNICODE),
                key(0, u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
            ]
        })
        .collect()
}

/// Key-ups for every modifier that is down right now (for example Shift still held from a
/// Shift+click), so they cannot turn Ctrl+V into another shortcut or reach the target as
/// part of the typed text. A held Alt or Win gets a mask key tap first, so its release opens
/// neither a menu bar nor the Start menu. Modifiers are not pressed again afterwards: a key
/// the user still holds simply reads as up until it is pressed again, nothing stays stuck.
fn release_modifiers() -> Vec<INPUT> {
    let held: Vec<u16> = [
        VK_LSHIFT,
        VK_RSHIFT,
        VK_LCONTROL,
        VK_RCONTROL,
        VK_LMENU,
        VK_RMENU,
        VK_LWIN,
        VK_RWIN,
    ]
    .into_iter()
    .map(|vk| vk.0)
    .filter(|&vk| {
        // SAFETY: plain key state query.
        let state = unsafe { GetAsyncKeyState(i32::from(vk)) };
        state < 0
    })
    .collect();
    let mut inputs = Vec::new();
    let alt_or_win = held
        .iter()
        .any(|&vk| matches!(VIRTUAL_KEY(vk), VK_LMENU | VK_RMENU | VK_LWIN | VK_RWIN));
    if alt_or_win {
        inputs.push(key(VK_MASK, 0, KEYBD_EVENT_FLAGS(0)));
        inputs.push(key(VK_MASK, 0, KEYEVENTF_KEYUP));
    }
    inputs.extend(held.into_iter().map(|vk| vk_key(vk, true)));
    inputs
}

fn send(inputs: &[INPUT]) -> bool {
    if inputs.is_empty() {
        return true;
    }
    // SAFETY: inputs is a valid slice of initialised INPUT structs of the given size.
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        crate::log!("insert: SendInput sent {sent} of {} events", inputs.len());
    }
    sent as usize == inputs.len()
}

fn foreground_is(target: HWND) -> bool {
    // SAFETY: no arguments.
    let fg = unsafe { GetForegroundWindow() };
    fg == target
}

/// Polls until `target` is the foreground window or `deadline` passes.
fn wait_foreground(target: HWND, deadline: Instant) -> bool {
    loop {
        if foreground_is(target) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Brings `target` back to the front within [`FOCUS_TIMEOUT`]. Our process had the
/// foreground a moment ago, so a plain `SetForegroundWindow` is normally allowed; if it is
/// refused, attach to the current foreground thread's input and try again.
fn restore_foreground(target: HWND) -> bool {
    let started = Instant::now();
    if foreground_is(target) {
        return true;
    }
    // SAFETY: plain call; a stale handle only makes it fail.
    let first = unsafe { SetForegroundWindow(target) }.as_bool();
    if first && wait_foreground(target, started + FOCUS_TIMEOUT / 3) {
        return true;
    }
    // SAFETY: no arguments.
    let fg = unsafe { GetForegroundWindow() };
    let fg_tid = if fg.is_invalid() {
        0
    } else {
        // SAFETY: a stale hwnd gives thread id 0.
        unsafe { GetWindowThreadProcessId(fg, None) }
    };
    // SAFETY: no arguments.
    let me = unsafe { GetCurrentThreadId() };
    let attached = fg_tid != 0 && fg_tid != me && {
        // SAFETY: both ids are live GUI threads; detached again below.
        unsafe { AttachThreadInput(me, fg_tid, true) }.as_bool()
    };
    // SAFETY: plain call; a stale handle only makes it fail.
    let second = unsafe { SetForegroundWindow(target) }.as_bool();
    // SAFETY: plain call; a stale handle only makes it fail.
    let _ = unsafe { BringWindowToTop(target) };
    if attached {
        // SAFETY: undoes the attach above.
        let _ = unsafe { AttachThreadInput(me, fg_tid, false) };
    }
    let ok = wait_foreground(target, started + FOCUS_TIMEOUT);
    crate::log!(
        "insert: foreground first={first} attached={attached} second={second} ok={ok} after {:?}",
        started.elapsed()
    );
    ok
}

/// `TokenElevation` of `process`, `None` if the token cannot be read.
fn token_elevated(process: HANDLE) -> Option<bool> {
    let mut token = HANDLE::default();
    // SAFETY: token is a valid out pointer; the handle is closed below.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.ok()?;
    let mut info = TOKEN_ELEVATION::default();
    let mut len = 0u32;
    // SAFETY: info is a valid buffer of the given size for TokenElevation.
    let res = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&mut info as *mut TOKEN_ELEVATION).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    };
    // SAFETY: token is a handle we own and close exactly once.
    let _ = unsafe { CloseHandle(token) };
    res.ok()?;
    Some(info.TokenIsElevated != 0)
}

/// True when `target` runs elevated while this process does not. A target whose token we
/// cannot even read is treated as elevated: UIPI would drop our input there as well.
fn target_elevated(target: HWND) -> bool {
    // SAFETY: returns a pseudo handle that needs no closing.
    let me = unsafe { GetCurrentProcess() };
    if token_elevated(me).unwrap_or(false) {
        return false;
    }
    let mut pid = 0u32;
    // SAFETY: pid is a valid out pointer; a stale hwnd leaves it at 0.
    unsafe { GetWindowThreadProcessId(target, Some(&mut pid)) };
    if pid == 0 {
        return false;
    }
    // SAFETY: plain query; the handle is closed below.
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return true;
    };
    let elevated = token_elevated(process).unwrap_or(true);
    // SAFETY: process is a handle we own and close exactly once.
    let _ = unsafe { CloseHandle(process) };
    elevated
}

/// Opens the clipboard for our hidden owner window, retrying briefly while another app
/// holds it. Returns false after about 50 ms.
fn open_clipboard() -> bool {
    let owner = CLIP_OWNER.with(|h| *h);
    let owner = (owner != 0).then_some(HWND(owner as *mut _));
    for _ in 0..10 {
        // SAFETY: owner is our own window (or none); closed by the caller.
        if unsafe { OpenClipboard(owner) }.is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    crate::log!("insert: OpenClipboard failed");
    false
}

fn close_clipboard() {
    // SAFETY: only called after a successful open_clipboard.
    let _ = unsafe { CloseClipboard() };
}

/// `CF_UNICODETEXT` on the clipboard, if any. The clipboard must be open.
fn clipboard_text() -> Option<String> {
    // SAFETY: the clipboard is open; the handle stays owned by the clipboard.
    let handle = unsafe { GetClipboardData(CF_UNICODETEXT) }.ok()?;
    let mem = HGLOBAL(handle.0);
    // SAFETY: CF_UNICODETEXT data is a global memory block.
    let size = unsafe { GlobalSize(mem) };
    // SAFETY: locks the block; unlocked below.
    let ptr = unsafe { GlobalLock(mem) } as *const u16;
    if ptr.is_null() {
        return None;
    }
    // SAFETY: the locked block is `size` bytes long and holds u16 units.
    let units = unsafe { std::slice::from_raw_parts(ptr, size / 2) };
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    let text = String::from_utf16_lossy(&units[..end]);
    // SAFETY: matches the GlobalLock above.
    let _ = unsafe { GlobalUnlock(mem) };
    Some(text)
}

/// Copies `bytes` into a new movable global block and hands it to the clipboard as
/// `format`. The clipboard must be open and emptied by us.
fn set_clipboard_bytes(format: u32, bytes: &[u8]) -> bool {
    // SAFETY: plain allocation; freed below unless the clipboard takes it.
    let Ok(mem) = (unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) }) else {
        return false;
    };
    // SAFETY: locks the new block; unlocked below.
    let ptr = unsafe { GlobalLock(mem) } as *mut u8;
    if ptr.is_null() {
        // SAFETY: the block is ours and not on the clipboard.
        let _ = unsafe { GlobalFree(Some(mem)) };
        return false;
    }
    // SAFETY: the block holds at least bytes.len() bytes and does not overlap `bytes`.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len()) };
    // SAFETY: matches the GlobalLock above.
    let _ = unsafe { GlobalUnlock(mem) };
    // SAFETY: the clipboard is open and owned by us; on success it owns the block.
    if unsafe { SetClipboardData(format, Some(HANDLE(mem.0))) }.is_ok() {
        true
    } else {
        // SAFETY: the clipboard did not take the block, so it is still ours.
        let _ = unsafe { GlobalFree(Some(mem)) };
        false
    }
}

/// Replaces the clipboard with `text` (or empties it for `None`). `transient` keeps it out
/// of the Win+V clipboard history and cloud clipboard (used for the paste round trip).
/// Returns the clipboard sequence number afterwards, `None` on failure.
fn put_clipboard(text: Option<&str>, transient: bool) -> Option<u32> {
    if !open_clipboard() {
        return None;
    }
    // SAFETY: the clipboard is open.
    let mut ok = unsafe { EmptyClipboard() }.is_ok();
    if ok {
        if let Some(text) = text {
            let bytes: Vec<u8> = text
                .encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect();
            ok = set_clipboard_bytes(CF_UNICODETEXT, &bytes);
        }
    }
    if ok && transient {
        // SAFETY: registering a named format; the same name always gives the same id.
        let exclude =
            unsafe { RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")) };
        if exclude != 0 {
            set_clipboard_bytes(exclude, &[0]);
        }
        // SAFETY: registering a named format, as above.
        let history = unsafe { RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")) };
        if history != 0 {
            set_clipboard_bytes(history, &0u32.to_le_bytes());
        }
    }
    close_clipboard();
    // SAFETY: no arguments.
    ok.then(|| unsafe { GetClipboardSequenceNumber() })
}

/// Copies `text` for the user to paste by hand. Cancels any pending paste restore.
fn copy_only(text: &str, reason: Reason) -> InsertResult {
    RESTORE_TIMER.with(Timer::stop);
    PENDING.with(|p| p.borrow_mut().take());
    if put_clipboard(Some(text), false).is_none() {
        crate::log!("insert: copying to the clipboard failed");
    }
    InsertResult::CopiedOnly(reason)
}

/// Puts the old clipboard text back, unless the clipboard changed since we set the emoji.
fn restore_clipboard() {
    let Some(pending) = PENDING.with(|p| p.borrow_mut().take()) else {
        return;
    };
    // SAFETY: no arguments.
    let now = unsafe { GetClipboardSequenceNumber() };
    if now != pending.seq {
        crate::log!("insert: clipboard changed since the paste, not restoring");
        return;
    }
    if put_clipboard(pending.saved.as_deref(), true).is_none() {
        crate::log!("insert: restoring the clipboard failed");
    }
}

/// Ctrl+V with the emoji on the clipboard, then the old text back after [`RESTORE_DELAY`].
fn paste(text: &str) -> Option<InsertResult> {
    // A restore still pending holds the user's real text: keep that one, not our last emoji.
    // SAFETY: no arguments.
    let now = unsafe { GetClipboardSequenceNumber() };
    let carried = PENDING.with(|p| {
        p.borrow_mut()
            .take()
            .filter(|pending| pending.seq == now)
            .map(|pending| pending.saved)
    });
    let saved = match carried {
        Some(saved) => saved,
        None => {
            if !open_clipboard() {
                return None;
            }
            let saved = clipboard_text();
            close_clipboard();
            saved
        }
    };
    let seq = put_clipboard(Some(text), true)?;
    let mut inputs = release_modifiers();
    inputs.extend([
        vk_key(VK_CONTROL.0, false),
        vk_key(VK_V, false),
        vk_key(VK_V, true),
        vk_key(VK_CONTROL.0, true),
    ]);
    send(&inputs);
    PENDING.with(|p| *p.borrow_mut() = Some(PendingRestore { saved, seq }));
    RESTORE_TIMER.with(|t| t.start(TimerMode::SingleShot, RESTORE_DELAY, restore_clipboard));
    Some(InsertResult::Pasted)
}

/// True when the target's exe is in `paste_exes` (compared by lowercased file name).
fn wants_paste(target: HWND, paste_exes: &[String]) -> bool {
    if paste_exes.is_empty() {
        return false;
    }
    let Some(exe) = exe_path(target).map(|p| exe_file_name(&p)) else {
        return false;
    };
    paste_exes.iter().any(|e| exe_file_name(e) == exe)
}

/// Inserts `req.text` into `req.target` (spec 7.8). Call on the UI thread after the picker
/// hid. Blocks for at most [`FOCUS_TIMEOUT`] plus a few clipboard retries.
pub fn insert(req: &InsertRequest, mode: InsertMode, paste_exes: &[String]) -> InsertResult {
    let target = HWND(req.target as *mut _);
    // SAFETY: plain query; any value is allowed.
    let alive = req.target != 0 && unsafe { IsWindow(Some(target)) }.as_bool();
    if !alive || !restore_foreground(target) {
        return copy_only(&req.text, Reason::TargetGone);
    }
    if target_elevated(target) {
        return copy_only(&req.text, Reason::Elevated);
    }
    if mode == InsertMode::Paste || wants_paste(target, paste_exes) {
        if let Some(result) = paste(&req.text) {
            return result;
        }
        crate::log!("insert: clipboard busy, typing instead");
    }
    let mut inputs = release_modifiers();
    inputs.extend(unicode_inputs(&req.text));
    send(&inputs);
    InsertResult::Typed
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, KEYEVENTF_UNICODE};

    #[test]
    fn keycap_and_zwj_units() {
        // SAFETY: unicode_inputs only builds keyboard inputs, so `ki` is the active field.
        let units = |s: &str| {
            unicode_inputs(s)
                .iter()
                .map(|i| unsafe { i.Anonymous.ki.wScan })
                .collect::<Vec<_>>()
        };
        let pride = "🏳️‍🌈";
        let expected: Vec<u16> = pride.encode_utf16().flat_map(|u| [u, u]).collect();
        assert_eq!(units(pride), expected);
        assert_eq!(units("#️⃣").len(), 6);
        assert_eq!(units("👍🏽").len(), 8);
    }

    #[test]
    fn up_events_flagged() {
        let inputs = unicode_inputs("👍🏽#️⃣");
        assert!(!inputs.is_empty());
        for (n, input) in inputs.iter().enumerate() {
            // SAFETY: unicode_inputs only builds keyboard inputs, so `ki` is the active field.
            let ki = unsafe { input.Anonymous.ki };
            let want = if n % 2 == 1 {
                KEYEVENTF_KEYUP | KEYEVENTF_UNICODE
            } else {
                KEYEVENTF_UNICODE
            };
            assert_eq!(ki.dwFlags, want, "input {n}");
            assert_eq!(ki.wVk.0, 0, "input {n}");
            assert_eq!(ki.dwExtraInfo, OWN_INPUT_MARKER, "input {n}");
        }
    }
}
