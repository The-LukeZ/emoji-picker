//! What the service thread learns about the foreground window: exe, fullscreen, caret.

use emoji_core::geometry::{is_fullscreen, Point, Rect};
use emoji_core::policy::Foreground;
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetCursorPos, GetGUIThreadInfo, GetShellWindow, GetWindowLongW, GetWindowRect,
    GetWindowThreadProcessId, IsZoomed, GUITHREADINFO, GWL_STYLE, WS_CAPTION,
};

fn to_rect(r: RECT) -> Rect {
    Rect {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

/// Full path of the exe that owns `hwnd`, or `None` if it cannot be queried.
pub fn exe_path(hwnd: HWND) -> Option<String> {
    if hwnd.is_invalid() {
        return None;
    }
    let mut pid = 0u32;
    // SAFETY: hwnd may be stale; the call then returns 0 and leaves pid at 0.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return None;
    }
    // SAFETY: plain query; the handle is closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: buf is valid for len u16 values and len is updated to the written length.
    let res = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    // SAFETY: process is a handle we own and close exactly once.
    let _ = unsafe { CloseHandle(process) };
    res.ok()?;
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    // SAFETY: buf is a valid writable slice.
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

/// True for the desktop and shell windows, which cover the monitor but are not fullscreen apps.
fn is_shell(hwnd: HWND) -> bool {
    // SAFETY: no arguments; returns null when there is no shell.
    let shell = unsafe { GetShellWindow() };
    hwnd == shell || matches!(class_name(hwnd).as_str(), "Progman" | "WorkerW")
}

/// True when `hwnd` covers its whole monitor and is not a shell window. Cheap: no exe lookup.
pub fn is_window_fullscreen(hwnd: HWND) -> bool {
    if hwnd.is_invalid() || is_shell(hwnd) {
        return false;
    }
    let mut window = RECT::default();
    // SAFETY: window is a valid out pointer.
    if unsafe { GetWindowRect(hwnd, &mut window) }.is_err() {
        return false;
    }
    // SAFETY: MONITOR_DEFAULTTONEAREST always returns a monitor.
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: info is a valid out pointer with cbSize set.
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return false;
    }
    // SAFETY: a stale hwnd gives false.
    let maximized = unsafe { IsZoomed(hwnd) }.as_bool();
    // SAFETY: a stale hwnd gives 0.
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
    // WS_CAPTION is two bits (border + dlgframe); a normal title bar has both.
    let has_caption = style & WS_CAPTION.0 == WS_CAPTION.0;
    is_fullscreen(
        to_rect(window),
        to_rect(info.rcMonitor),
        maximized,
        has_caption,
    )
}

/// Exe and fullscreen state of `hwnd` (a null hwnd gives an unknown exe).
pub fn probe(hwnd: HWND) -> Foreground {
    Foreground {
        exe: exe_path(hwnd),
        fullscreen: is_window_fullscreen(hwnd),
    }
}

/// Screen rect of the text caret in the GUI thread that owns `hwnd`, if it shows one.
pub fn caret_rect(hwnd: HWND) -> Option<Rect> {
    if hwnd.is_invalid() {
        return None;
    }
    // SAFETY: a stale hwnd gives thread id 0.
    let tid = unsafe { GetWindowThreadProcessId(hwnd, None) };
    // Thread id 0 would make GetGUIThreadInfo report the foreground thread's caret instead.
    if tid == 0 {
        return None;
    }
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: info is a valid out pointer with cbSize set.
    unsafe { GetGUIThreadInfo(tid, &mut info) }.ok()?;
    let r = info.rcCaret;
    if info.hwndCaret.is_invalid() || (r.right <= r.left && r.bottom <= r.top) {
        return None;
    }
    let mut tl = POINT {
        x: r.left,
        y: r.top,
    };
    let mut br = POINT {
        x: r.right,
        y: r.bottom,
    };
    // SAFETY: tl is a valid in/out pointer.
    let ok_tl = unsafe { ClientToScreen(info.hwndCaret, &mut tl) }.as_bool();
    // SAFETY: br is a valid in/out pointer.
    let ok_br = unsafe { ClientToScreen(info.hwndCaret, &mut br) }.as_bool();
    let ok = ok_tl && ok_br;
    ok.then_some(Rect {
        left: tl.x,
        top: tl.y,
        right: br.x,
        bottom: br.y,
    })
}

/// Current mouse position in screen coordinates.
pub fn cursor() -> Point {
    let mut p = POINT::default();
    // SAFETY: p is a valid out pointer; on failure it stays at the origin.
    let _ = unsafe { GetCursorPos(&mut p) };
    Point { x: p.x, y: p.y }
}
