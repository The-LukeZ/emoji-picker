//! Small system queries: memory, theme, autostart and the list of running apps.

use std::io;

use emoji_core::policy::exe_file_name;
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, HWND, LPARAM, WIN32_ERROR};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_DWORD,
};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, IsWindowVisible};

use super::foreground::exe_path;

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
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
#[allow(dead_code)] // used by the picker and settings windows (Tasks 12 and 14)
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

/// Adds or removes the HKCU Run value that starts the current exe at sign-in.
#[allow(dead_code)] // used by settings (Task 14)
pub fn set_autostart(on: bool) -> io::Result<()> {
    if !on {
        // SAFETY: static key and value names.
        let err = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE) };
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
    // SAFETY: data is a null-terminated UTF-16 buffer and the size is its length in bytes.
    let err = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
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
#[allow(dead_code)] // used by the blocklist picker in settings (Task 14)
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
