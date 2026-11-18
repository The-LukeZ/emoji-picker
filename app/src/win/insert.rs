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
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, GlobalFree, ERROR_SUCCESS, HANDLE, HGLOBAL, HWND,
};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
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
/// Time the target app gets to read the clipboard after Ctrl+V before the old content returns.
const RESTORE_DELAY: Duration = Duration::from_millis(300);
/// Largest clipboard content (all formats together) saved for a paste. Above it the emoji is
/// typed instead, so the user's clipboard is never replaced by something we cannot restore.
const SNAPSHOT_CAP: usize = 64 * 1024 * 1024;

// Standard clipboard formats from winuser.h (avoids the large `Win32_System_Ole` feature).
const CF_BITMAP: u32 = 2;
const CF_METAFILEPICT: u32 = 3;
const CF_PALETTE: u32 = 9;
const CF_UNICODETEXT: u32 = 13;
const CF_ENHMETAFILE: u32 = 14;
const CF_OWNERDISPLAY: u32 = 0x0080;
const CF_DSPBITMAP: u32 = 0x0082;
const CF_DSPMETAFILEPICT: u32 = 0x0083;
const CF_DSPENHMETAFILE: u32 = 0x008E;
/// `CF_PRIVATEFIRST..=CF_GDIOBJLAST`: private handles of any kind and GDI objects.
const CF_PRIVATE_AND_GDIOBJ: std::ops::RangeInclusive<u32> = 0x0200..=0x03FF;

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
    /// No window was in front when the hotkey was pressed.
    NoTarget,
    /// `SendInput` injected none of the keys (input blocked, for example by `BlockInput`
    /// or a secure desktop).
    InputBlocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertResult {
    Typed,
    Pasted,
    /// The text is only on the clipboard; the caller tells the user why.
    CopiedOnly(Reason),
    /// Neither inserted nor copied (the clipboard could not be written either).
    NotInserted,
}

impl Reason {
    /// Balloon text for a [`InsertResult::CopiedOnly`] result.
    pub fn balloon_text(self) -> &'static str {
        match self {
            Reason::Elevated => "Can't type into admin windows, emoji copied to clipboard",
            Reason::TargetGone => "Target window is gone, emoji copied to clipboard",
            Reason::NoTarget => "No target window, emoji copied to clipboard",
            Reason::InputBlocked => "Typing was blocked, emoji copied to clipboard",
        }
    }
}

impl InsertResult {
    /// What the caller tells the user in a balloon, `None` when the emoji arrived.
    pub fn balloon_text(self) -> Option<&'static str> {
        match self {
            InsertResult::Typed | InsertResult::Pasted => None,
            InsertResult::CopiedOnly(reason) => Some(reason.balloon_text()),
            InsertResult::NotInserted => {
                Some("Couldn't insert the emoji or copy it to the clipboard")
            }
        }
    }
}

/// One clipboard format and its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClipItem {
    format: u32,
    data: Vec<u8>,
}

/// A clipboard restore that waits for its timer.
struct PendingRestore {
    /// Every format that was on the clipboard before the first paste (empty: there was none).
    saved: Vec<ClipItem>,
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

/// True for the keys that need `KEYEVENTF_EXTENDEDKEY`.
fn is_extended(vk: u16) -> bool {
    matches!(VIRTUAL_KEY(vk), VK_RCONTROL | VK_RMENU | VK_LWIN | VK_RWIN)
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
    if is_extended(vk) {
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

/// Key-ups for the virtual keys that the first `sent` events of `inputs` pressed and did not
/// release again, newest first. Unicode (`VK_PACKET`) events change no key state and are
/// skipped. Used after a partial `SendInput`, so no key we pressed can stay down.
fn release_after_partial_send(inputs: &[INPUT], sent: usize) -> Vec<INPUT> {
    let mut down: Vec<u16> = Vec::new();
    for input in &inputs[..sent.min(inputs.len())] {
        // SAFETY: every INPUT built in this module is a keyboard input, so `ki` is active.
        let ki = unsafe { input.Anonymous.ki };
        if ki.dwFlags.contains(KEYEVENTF_UNICODE) || ki.wVk.0 == 0 {
            continue;
        }
        let vk = ki.wVk.0;
        down.retain(|&d| d != vk);
        if !ki.dwFlags.contains(KEYEVENTF_KEYUP) {
            down.push(vk);
        }
    }
    down.into_iter().rev().map(|vk| vk_key(vk, true)).collect()
}

/// Sends `inputs` in one batch and returns how many events were injected. After a partial
/// send, every key the batch pressed and did not release yet is released.
fn send(inputs: &[INPUT]) -> usize {
    if inputs.is_empty() {
        return 0;
    }
    // SAFETY: inputs is a valid slice of initialised INPUT structs of the given size.
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) } as usize;
    if sent < inputs.len() {
        crate::log!("insert: SendInput sent {sent} of {} events", inputs.len());
        let ups = release_after_partial_send(inputs, sent);
        if !ups.is_empty() {
            // SAFETY: as above.
            let released = unsafe { SendInput(&ups, size_of::<INPUT>() as i32) };
            crate::log!("insert: released {released} of {} keys", ups.len());
        }
    }
    sent
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

/// The registered formats that keep our temporary clipboard content out of the Win+V
/// history and cloud clipboard (0 where registration failed).
fn history_formats() -> [u32; 2] {
    // SAFETY: registering a named format; the same name always gives the same id.
    let exclude =
        unsafe { RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")) };
    // SAFETY: registering a named format, as above.
    let history = unsafe { RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")) };
    [exclude, history]
}

/// Formats whose clipboard handle is not a global memory block (bitmaps, metafiles,
/// palettes, owner display, private handles, GDI objects), so their bytes cannot be saved.
/// `CF_DIB` and `CF_DIBV5`, which Windows offers for every bitmap, are memory blocks.
fn is_handle_format(format: u32) -> bool {
    matches!(
        format,
        CF_BITMAP
            | CF_METAFILEPICT
            | CF_PALETTE
            | CF_ENHMETAFILE
            | CF_OWNERDISPLAY
            | CF_DSPBITMAP
            | CF_DSPMETAFILEPICT
            | CF_DSPENHMETAFILE
    ) || CF_PRIVATE_AND_GDIOBJ.contains(&format)
}

/// Which of the clipboard's `formats` (in clipboard order) to save before a paste. Handle
/// formats are skipped, and so are `ours` (the history exclusion formats, which every
/// restore sets anyway). `None` when the clipboard holds only handle formats: then nothing
/// of it could be put back, and the caller types instead of pasting.
fn formats_to_save(formats: &[u32], ours: &[u32]) -> Option<Vec<u32>> {
    let mut keep = Vec::new();
    let mut skipped = false;
    for &format in formats {
        if format == 0 || ours.contains(&format) || keep.contains(&format) {
            continue;
        }
        if is_handle_format(format) {
            skipped = true;
        } else {
            keep.push(format);
        }
    }
    if keep.is_empty() && skipped {
        None
    } else {
        Some(keep)
    }
}

/// The bytes of one memory-block format. The clipboard must be open. `None` if the data
/// cannot be read (for example delayed rendering that failed) or `budget` bytes are exceeded.
fn read_format(format: u32, budget: usize) -> Option<Vec<u8>> {
    // SAFETY: the clipboard is open; the handle stays owned by the clipboard.
    let handle = unsafe { GetClipboardData(format) }.ok()?;
    let mem = HGLOBAL(handle.0);
    // SAFETY: formats passed here are global memory blocks; a bad handle gives 0.
    let size = unsafe { GlobalSize(mem) };
    if size == 0 || size > budget {
        return None;
    }
    // SAFETY: locks the block; unlocked below.
    let ptr = unsafe { GlobalLock(mem) } as *const u8;
    if ptr.is_null() {
        return None;
    }
    // SAFETY: the locked block is `size` bytes long.
    let data = unsafe { std::slice::from_raw_parts(ptr, size) }.to_vec();
    // SAFETY: matches the GlobalLock above.
    let _ = unsafe { GlobalUnlock(mem) };
    Some(data)
}

/// Every savable format on the clipboard, in clipboard order. The clipboard must be open.
/// `Err` (with the reason) when part of the content could not be saved: then a paste would
/// lose it, so the caller types instead.
fn read_snapshot() -> Result<Vec<ClipItem>, &'static str> {
    let mut formats = Vec::new();
    let mut format = 0;
    loop {
        // SAFETY: the clipboard is open; 0 starts the enumeration.
        format = unsafe { EnumClipboardFormats(format) };
        if format == 0 {
            break;
        }
        formats.push(format);
    }
    // SAFETY: reads this thread's last error, set by the EnumClipboardFormats call above.
    if unsafe { GetLastError() } != ERROR_SUCCESS {
        return Err("listing the clipboard formats failed");
    }
    let keep = formats_to_save(&formats, &history_formats()).ok_or("only GDI handle formats")?;
    let mut items = Vec::with_capacity(keep.len());
    let mut total = 0usize;
    for format in keep {
        let data = read_format(format, SNAPSHOT_CAP - total)
            .ok_or("a format could not be read or the content is over 64 MB")?;
        total += data.len();
        items.push(ClipItem { format, data });
    }
    Ok(items)
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

/// The emoji as a `CF_UNICODETEXT` item.
fn text_item(text: &str) -> ClipItem {
    ClipItem {
        format: CF_UNICODETEXT,
        data: text
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect(),
    }
}

/// Outcome of [`write_clipboard`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipWrite {
    /// Written; the clipboard sequence number afterwards.
    Done(u32),
    /// The clipboard could not be opened; it is unchanged.
    Busy,
    /// The clipboard changed since the expected sequence number; it is unchanged.
    Changed,
    /// Writing failed. `emptied`: the old content is already gone.
    Failed { emptied: bool },
}

/// Replaces the clipboard with `items` (an empty list empties it). `transient` keeps it out
/// of the Win+V clipboard history and cloud clipboard (used for the paste round trip).
/// `expect_seq`: only write while the clipboard sequence number still is this value,
/// checked with the clipboard open, so no other app can change it in between.
fn write_clipboard(items: &[ClipItem], transient: bool, expect_seq: Option<u32>) -> ClipWrite {
    if !open_clipboard() {
        return ClipWrite::Busy;
    }
    if let Some(expected) = expect_seq {
        // SAFETY: no arguments.
        let now = unsafe { GetClipboardSequenceNumber() };
        if now != expected {
            close_clipboard();
            return ClipWrite::Changed;
        }
    }
    // SAFETY: the clipboard is open.
    if unsafe { EmptyClipboard() }.is_err() {
        close_clipboard();
        return ClipWrite::Failed { emptied: false };
    }
    for item in items {
        if !set_clipboard_bytes(item.format, &item.data) {
            close_clipboard();
            crate::log!("insert: SetClipboardData({}) failed", item.format);
            return ClipWrite::Failed { emptied: true };
        }
    }
    if transient {
        let [exclude, history] = history_formats();
        if exclude != 0 {
            set_clipboard_bytes(exclude, &[0]);
        }
        if history != 0 {
            set_clipboard_bytes(history, &0u32.to_le_bytes());
        }
    }
    close_clipboard();
    // SAFETY: no arguments.
    ClipWrite::Done(unsafe { GetClipboardSequenceNumber() })
}

/// Copies `text` for the user to paste by hand. Cancels any pending paste restore; if the
/// copy fails, that restore runs instead, so the user's own content comes back.
fn copy_only(text: &str, reason: Reason) -> InsertResult {
    RESTORE_TIMER.with(Timer::stop);
    let pending = PENDING.with(|p| p.borrow_mut().take());
    match write_clipboard(&[text_item(text)], false, None) {
        ClipWrite::Done(_) => InsertResult::CopiedOnly(reason),
        failed => {
            crate::log!("insert: copying to the clipboard failed: {failed:?}");
            if let Some(pending) = pending {
                // If we emptied the clipboard, the emoji of the earlier paste is gone too:
                // put the saved content back without the sequence check.
                let expect = match failed {
                    ClipWrite::Failed { emptied: true } => None,
                    _ => Some(pending.seq),
                };
                let back = write_clipboard(&pending.saved, true, expect);
                crate::log!("insert: restoring the clipboard after the failed copy: {back:?}");
            }
            InsertResult::NotInserted
        }
    }
}

/// Puts the saved clipboard content back, unless the clipboard changed since we set the
/// emoji (checked again with the clipboard open).
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
    match write_clipboard(&pending.saved, true, Some(pending.seq)) {
        ClipWrite::Done(_) => {}
        ClipWrite::Changed => {
            crate::log!("insert: clipboard changed since the paste, not restoring")
        }
        failed => crate::log!("insert: restoring the clipboard failed: {failed:?}"),
    }
}

/// Runs a pending clipboard restore now (at shutdown), so the user's content is not left
/// replaced by the last pasted emoji.
pub fn flush_pending_restore() {
    RESTORE_TIMER.with(Timer::stop);
    restore_clipboard();
}

/// Ctrl+V with the emoji on the clipboard, then the old content back after
/// [`RESTORE_DELAY`]. `None`: the clipboard was left as it was, type instead.
fn paste(text: &str) -> Option<InsertResult> {
    // A restore still pending holds the user's real content: keep that, not our last emoji.
    // SAFETY: no arguments.
    let now = unsafe { GetClipboardSequenceNumber() };
    let carried = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        if p.as_ref().is_some_and(|pending| pending.seq == now) {
            p.take()
        } else {
            None
        }
    });
    let carried_seq = carried.as_ref().map(|pending| pending.seq);
    if carried.is_some() {
        RESTORE_TIMER.with(Timer::stop);
    }
    let saved = match carried {
        Some(pending) => pending.saved,
        None => {
            if !open_clipboard() {
                crate::log!("insert: clipboard busy, typing instead");
                return None;
            }
            let snapshot = read_snapshot();
            close_clipboard();
            match snapshot {
                Ok(saved) => saved,
                Err(why) => {
                    crate::log!("insert: clipboard cannot be saved ({why}), typing instead");
                    return None;
                }
            }
        }
    };
    let seq = match write_clipboard(&[text_item(text)], true, None) {
        ClipWrite::Done(seq) => seq,
        ClipWrite::Failed { emptied: true } => {
            let back = write_clipboard(&saved, true, None);
            crate::log!("insert: setting the emoji failed, clipboard put back: {back:?}");
            return None;
        }
        failed => {
            crate::log!("insert: setting the emoji failed: {failed:?}, typing instead");
            // The clipboard is unchanged. A restore that was pending still has to run.
            if let Some(seq) = carried_seq {
                schedule_restore(saved, seq);
            }
            return None;
        }
    };
    let mut inputs = release_modifiers();
    let v_down = inputs.len() + 1;
    inputs.extend([
        vk_key(VK_CONTROL.0, false),
        vk_key(VK_V, false),
        vk_key(VK_V, true),
        vk_key(VK_CONTROL.0, true),
    ]);
    let sent = send(&inputs);
    if sent <= v_down {
        // V never went down: nothing was pasted. The emoji stays on the clipboard for the
        // user to paste by hand, so there is no restore.
        return Some(InsertResult::CopiedOnly(Reason::InputBlocked));
    }
    schedule_restore(saved, seq);
    Some(InsertResult::Pasted)
}

/// Puts `saved` back after [`RESTORE_DELAY`] if the clipboard still has sequence `seq`.
fn schedule_restore(saved: Vec<ClipItem>, seq: u32) {
    PENDING.with(|p| *p.borrow_mut() = Some(PendingRestore { saved, seq }));
    RESTORE_TIMER.with(|t| t.start(TimerMode::SingleShot, RESTORE_DELAY, restore_clipboard));
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
    if req.target == 0 {
        return copy_only(&req.text, Reason::NoTarget);
    }
    let target = HWND(req.target as *mut _);
    // SAFETY: plain query; any value is allowed.
    if !unsafe { IsWindow(Some(target)) }.as_bool() {
        return copy_only(&req.text, Reason::TargetGone);
    }
    if !restore_foreground(target) {
        // An admin window may refuse the focus: say why, not that it is gone.
        let reason = if target_elevated(target) {
            Reason::Elevated
        } else {
            Reason::TargetGone
        };
        return copy_only(&req.text, reason);
    }
    if target_elevated(target) {
        return copy_only(&req.text, Reason::Elevated);
    }
    if mode == InsertMode::Paste || wants_paste(target, paste_exes) {
        if let Some(result) = paste(&req.text) {
            return result;
        }
    }
    let mut inputs = release_modifiers();
    let text_start = inputs.len();
    inputs.extend(unicode_inputs(&req.text));
    if send(&inputs) <= text_start {
        return copy_only(&req.text, Reason::InputBlocked);
    }
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

    /// (vk, flags) of each input, for comparisons.
    fn keys(inputs: &[INPUT]) -> Vec<(u16, KEYBD_EVENT_FLAGS)> {
        inputs
            .iter()
            .map(|i| {
                // SAFETY: only keyboard inputs are built here, so `ki` is the active field.
                let ki = unsafe { i.Anonymous.ki };
                assert_eq!(ki.dwExtraInfo, OWN_INPUT_MARKER);
                (ki.wVk.0, ki.dwFlags)
            })
            .collect()
    }

    #[test]
    fn partial_paste_releases_pressed_keys() {
        let ctrl = VK_CONTROL.0;
        let inputs = [
            key(VK_MASK, 0, KEYBD_EVENT_FLAGS(0)),
            key(VK_MASK, 0, KEYEVENTF_KEYUP),
            vk_key(VK_LMENU.0, true),
            vk_key(ctrl, false),
            vk_key(VK_V, false),
            vk_key(VK_V, true),
            vk_key(ctrl, true),
        ];
        let up = KEYEVENTF_KEYUP;
        // Nothing sent: nothing to release.
        assert!(release_after_partial_send(&inputs, 0).is_empty());
        // Mask down only.
        assert_eq!(
            keys(&release_after_partial_send(&inputs, 1)),
            [(VK_MASK, up)]
        );
        // Up to Ctrl down: release Ctrl (a user key-up such as Alt needs nothing).
        assert_eq!(keys(&release_after_partial_send(&inputs, 4)), [(ctrl, up)]);
        // Ctrl and V down: V first, then Ctrl.
        assert_eq!(
            keys(&release_after_partial_send(&inputs, 5)),
            [(VK_V, up), (ctrl, up)]
        );
        assert_eq!(keys(&release_after_partial_send(&inputs, 6)), [(ctrl, up)]);
        // Everything sent (or a bogus count): nothing left down.
        assert!(release_after_partial_send(&inputs, 7).is_empty());
        assert!(release_after_partial_send(&inputs, 99).is_empty());
    }

    #[test]
    fn partial_typing_releases_only_virtual_keys() {
        let mut inputs = vec![key(VK_MASK, 0, KEYBD_EVENT_FLAGS(0))];
        inputs.extend(unicode_inputs("👍"));
        // Unicode units change no key state; only the mask key is still down.
        assert_eq!(
            keys(&release_after_partial_send(&inputs, 2)),
            [(VK_MASK, KEYEVENTF_KEYUP)]
        );
        assert!(release_after_partial_send(&unicode_inputs("👍"), 1).is_empty());
    }

    #[test]
    fn right_keys_released_as_extended() {
        let inputs = [vk_key(VK_RCONTROL.0, false)];
        assert_eq!(
            keys(&release_after_partial_send(&inputs, 1)),
            [(VK_RCONTROL.0, KEYEVENTF_KEYUP | KEYEVENTF_EXTENDEDKEY)]
        );
    }

    #[test]
    fn clipboard_format_filter() {
        const CF_TEXT: u32 = 1;
        const CF_DIB: u32 = 8;
        const CF_HDROP: u32 = 15;
        const CF_LOCALE: u32 = 16;
        const CF_DIBV5: u32 = 17;
        const HTML: u32 = 0xC0A1;
        const OURS: [u32; 2] = [0xC100, 0xC101];
        // Text, HTML, files and DIBs are kept in clipboard order; GDI handles are skipped.
        assert_eq!(
            formats_to_save(&[HTML, CF_UNICODETEXT, CF_TEXT, CF_LOCALE, CF_HDROP], &OURS),
            Some(vec![HTML, CF_UNICODETEXT, CF_TEXT, CF_LOCALE, CF_HDROP])
        );
        assert_eq!(
            formats_to_save(&[CF_BITMAP, CF_DIB, CF_PALETTE, CF_DIBV5], &OURS),
            Some(vec![CF_DIB, CF_DIBV5])
        );
        // Our own history exclusion formats are not saved (every restore sets them).
        assert_eq!(
            formats_to_save(&[CF_UNICODETEXT, OURS[0], OURS[1]], &OURS),
            Some(vec![CF_UNICODETEXT])
        );
        // Empty clipboard: nothing to save, a paste may go ahead.
        assert_eq!(formats_to_save(&[], &OURS), Some(vec![]));
        assert_eq!(formats_to_save(&OURS, &OURS), Some(vec![]));
        // Only handle formats: cannot be preserved.
        for only in [
            &[CF_ENHMETAFILE, CF_METAFILEPICT][..],
            &[CF_OWNERDISPLAY],
            &[CF_DSPBITMAP, CF_DSPENHMETAFILE, CF_DSPMETAFILEPICT],
            &[0x0200],
            &[0x03FF],
        ] {
            assert_eq!(formats_to_save(only, &OURS), None, "{only:?}");
        }
        // Text next to a metafile: the text is saved.
        assert_eq!(
            formats_to_save(&[CF_ENHMETAFILE, CF_UNICODETEXT], &OURS),
            Some(vec![CF_UNICODETEXT])
        );
        // CF_DSPTEXT (0x81) is a memory block; 0x400 is past the GDI object range.
        assert_eq!(
            formats_to_save(&[0x0081, 0x0400], &OURS),
            Some(vec![0x0081, 0x0400])
        );
    }

    #[test]
    fn text_item_is_nul_terminated_utf16() {
        let item = text_item("👍");
        assert_eq!(item.format, CF_UNICODETEXT);
        assert_eq!(item.data, [0x3D, 0xD8, 0x4D, 0xDC, 0, 0]);
    }

    #[test]
    fn balloons() {
        assert_eq!(InsertResult::Typed.balloon_text(), None);
        assert_eq!(InsertResult::Pasted.balloon_text(), None);
        assert_eq!(
            InsertResult::CopiedOnly(Reason::NoTarget).balloon_text(),
            Some("No target window, emoji copied to clipboard")
        );
        assert!(InsertResult::NotInserted
            .balloon_text()
            .is_some_and(|t| !t.contains("copied to clipboard")));
    }
}
