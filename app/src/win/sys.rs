//! Small system queries: memory, theme, autostart, monitors, window styles and the list of
//! running apps.

use std::io;

use emoji_core::geometry::{Point, Rect};
use emoji_core::policy::exe_file_name;
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, HWND, LPARAM, POINT, RECT, WIN32_ERROR};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_DWORD,
};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, EnumWindows, GetWindowLongPtrW, IsWindowVisible, SetForegroundWindow,
    SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use super::foreground::exe_path;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: PCWSTR = w!("emoji-picker");

/// Private bytes (commit charge) of this process.
pub fn private_bytes() -> usize {
    let mut pmc = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: returns a pseudo handle that needs no closing.
    let process = unsafe { GetCurrentProcess() };
    // SAFETY: the EX struct starts with PROCESS_MEMORY_COUNTERS and cb gives its real size.
    let res = unsafe {
        GetProcessMemoryInfo(
            process,
            (&mut pmc as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            pmc.cb,
        )
    };
    res.map(|()| pmc.PrivateUsage).unwrap_or(0)
}

/// True when apps should use the light theme (the default when the value is missing).
pub fn apps_use_light_theme() -> bool {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: value and size are valid out pointers sized for a DWORD.
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    err.is_err() || value != 0
}

fn win32(err: WIN32_ERROR) -> io::Result<()> {
    if err.is_ok() {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(err.0 as i32))
    }
}

/// The HKCU key that holds the Run value. Debug and `diag` builds honour
/// `EMOJI_PICKER_RUN_KEY` (a key path under HKCU) so tests never touch the real Run key.
fn run_key() -> Vec<u16> {
    let mut key = RUN_KEY.to_string();
    if cfg!(any(debug_assertions, feature = "diag")) {
        if let Some(k) = std::env::var("EMOJI_PICKER_RUN_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
        {
            key = k;
        }
    }
    key.encode_utf16().chain(Some(0)).collect()
}

/// Adds or removes the HKCU Run value that starts the current exe at sign-in.
pub fn set_autostart(on: bool) -> io::Result<()> {
    let key = run_key();
    let key = PCWSTR(key.as_ptr());
    if !on {
        // SAFETY: key is a null-terminated buffer that outlives the call; static value name.
        let err = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key, RUN_VALUE) };
        return if err == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            win32(err)
        };
    }
    let exe = std::env::current_exe()?;
    let data: Vec<u16> = format!("\"{}\"", exe.display())
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: key and data are null-terminated UTF-16 buffers that outlive the call, and the
    // size is the length of data in bytes.
    let err = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key,
            RUN_VALUE,
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            (data.len() * 2) as u32,
        )
    };
    win32(err)
}

unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam is the &mut Vec passed by visible_window_exes for the duration of EnumWindows.
    let names = unsafe { &mut *(lparam.0 as *mut Vec<String>) };
    // SAFETY: hwnd comes from EnumWindows.
    if unsafe { IsWindowVisible(hwnd) }.as_bool() {
        if let Some(path) = exe_path(hwnd) {
            let name = exe_file_name(&path);
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    true.into()
}

/// Lowercased, deduplicated exe file names of all visible top-level windows.
pub fn visible_window_exes() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    // SAFETY: the callback only uses lparam while EnumWindows runs, and names outlives the call.
    let _ = unsafe {
        EnumWindows(
            Some(collect_window),
            LPARAM(&mut names as *mut Vec<String> as isize),
        )
    };
    names.sort();
    names
}

/// Work area (physical pixels) and scale factor (DPI / 96) of the monitor that holds `caret`,
/// or `cursor` when there is no caret. Falls back to the nearest monitor.
pub fn monitor_work_area(caret: Option<Rect>, cursor: Point) -> (Rect, f32) {
    let monitor = match caret {
        Some(r) => {
            let rect = RECT {
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: r.bottom,
            };
            // SAFETY: plain value lookup with a valid RECT reference.
            unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST) }
        }
        // SAFETY: plain value lookup.
        None => unsafe {
            MonitorFromPoint(
                POINT {
                    x: cursor.x,
                    y: cursor.y,
                },
                MONITOR_DEFAULTTONEAREST,
            )
        },
    };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: info is a valid out struct with cbSize set; the monitor handle needs no closing.
    let ok = unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool();
    let work = if ok {
        let r = info.rcWork;
        Rect {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    } else {
        Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        }
    };
    let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
    // SAFETY: both out pointers are valid u32s.
    let dpi = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
    let scale = if dpi.is_ok() && dpi_x > 0 {
        dpi_x as f32 / 96.0
    } else {
        1.0
    };
    (work, scale)
}

/// Adds `WS_EX_TOOLWINDOW | WS_EX_TOPMOST` (no Alt+Tab entry, stays on top). Winit rewrites
/// the extended style on every visibility change, so call this after each show.
pub fn set_tool_window(hwnd: isize) {
    let hwnd = HWND(hwnd as *mut _);
    // SAFETY: reading a style of a window owned by this thread.
    let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    let want = ex | (WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0) as isize;
    if want != ex {
        // SAFETY: setting extended style bits on a window owned by this thread.
        unsafe { SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want) };
    }
}

/// Brings `hwnd` to the foreground. Returns false when Windows refused.
pub fn activate(hwnd: isize) -> bool {
    // SAFETY: plain call with a window handle; an invalid handle only makes it fail.
    unsafe { SetForegroundWindow(HWND(hwnd as *mut _)) }.as_bool()
}

/// A never shown top-level window on the calling thread. Windows it owns get no taskbar button
/// and no Alt+Tab entry, whatever winit does to their extended style. 0 on failure.
pub fn create_owner_window() -> isize {
    // SAFETY: the predefined STATIC class needs no registration; all other arguments are
    // plain values and the window is never shown.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("STATIC"),
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        )
    };
    hwnd.map(|h| h.0 as isize).unwrap_or(0)
}
