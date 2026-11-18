//! Notification area icon with balloons and the context menu.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, PostMessageW, SetForegroundWindow,
    SetMenuDefaultItem, TrackPopupMenu, HICON, MF_CHECKED, MF_SEPARATOR, MF_STRING,
    TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, WM_APP, WM_NULL,
};

/// Callback message the shell sends to the owner window for icon clicks.
pub const WM_TRAY: u32 = WM_APP + 2;

/// Menu command ids, delivered to the owner window as `WM_COMMAND`.
pub const CMD_OPEN_PICKER: usize = 1;
pub const CMD_SETTINGS: usize = 2;
pub const CMD_PAUSE: usize = 3;
pub const CMD_ABOUT: usize = 4;
pub const CMD_QUIT: usize = 5;

const ICON_ID: u32 = 1;
const TIP: &str = "Emoji picker";

/// Copy `s` into a fixed UTF-16 buffer, truncated and null-terminated.
fn fill<const N: usize>(dst: &mut [u16; N], s: &str) {
    let mut i = 0;
    for unit in s.encode_utf16().take(N - 1) {
        dst[i] = unit;
        i += 1;
    }
    dst[i] = 0;
}

pub struct Tray {
    hwnd: HWND,
    icon: HICON,
}

impl Tray {
    /// Adds the icon. `hwnd` must be a top-level window so it also gets `TaskbarCreated`.
    pub fn new(hwnd: HWND, icon: HICON) -> Tray {
        let tray = Tray { hwnd, icon };
        tray.add();
        tray
    }

    fn data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: ICON_ID,
            ..Default::default()
        }
    }

    /// (Re)adds the icon; called at start and when Explorer restarts. Returns the shell result.
    pub fn add(&self) -> bool {
        let mut nid = self.data();
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = self.icon;
        fill(&mut nid.szTip, TIP);
        // SAFETY: nid is fully initialised with cbSize set.
        let added = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) }.as_bool();
        nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        // SAFETY: as above; NIM_SETVERSION only reads uVersion.
        let _ = unsafe { Shell_NotifyIconW(NIM_SETVERSION, &nid) };
        crate::log!("tray: Shell_NotifyIconW(NIM_ADD) = {added}");
        added
    }

    pub fn balloon(&self, title: &str, text: &str) {
        let mut nid = self.data();
        nid.uFlags = NIF_INFO;
        nid.dwInfoFlags = NIIF_INFO;
        fill(&mut nid.szInfoTitle, title);
        fill(&mut nid.szInfo, text);
        // SAFETY: nid is fully initialised with cbSize set.
        let ok = unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) }.as_bool();
        crate::log!("tray: balloon {title:?} / {text:?} shown = {ok}");
    }

    /// Shows the context menu at the cursor. The choice arrives as `WM_COMMAND` on the owner.
    pub fn show_menu(&self, paused: bool) {
        // SAFETY: the menu is created, used and destroyed within this call.
        let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
            return;
        };
        let pause_flags = if paused {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        let items: [(_, usize, PCWSTR); 6] = [
            (MF_STRING, CMD_OPEN_PICKER, w!("Open picker")),
            (MF_STRING, CMD_SETTINGS, w!("Settings")),
            (pause_flags, CMD_PAUSE, w!("Pause hotkey")),
            (MF_STRING, CMD_ABOUT, w!("About")),
            (MF_SEPARATOR, 0, PCWSTR::null()),
            (MF_STRING, CMD_QUIT, w!("Quit")),
        ];
        for (flags, id, text) in items {
            // SAFETY: menu is valid and text is a static wide string or null for a separator.
            let _ = unsafe { AppendMenuW(menu, flags, id, text) };
        }
        // SAFETY: menu is a valid menu handle with this item.
        let _ = unsafe { SetMenuDefaultItem(menu, CMD_OPEN_PICKER as u32, 0) };
        let mut pt = Default::default();
        // SAFETY: pt is a valid out pointer.
        let _ = unsafe { GetCursorPos(&mut pt) };
        // The foreground and WM_NULL steps make the menu close when the user clicks
        // elsewhere (documented TrackPopupMenu requirement).
        // SAFETY: self.hwnd is our tray window.
        let _ = unsafe { SetForegroundWindow(self.hwnd) };
        // SAFETY: menu and self.hwnd are valid; runs a modal loop on this thread.
        let _ = unsafe {
            TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
                pt.x,
                pt.y,
                None,
                self.hwnd,
                None,
            )
        };
        // SAFETY: plain post to our own window.
        let _ = unsafe { PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0)) };
        // SAFETY: menu was created above and is not used afterwards.
        let _ = unsafe { DestroyMenu(menu) };
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        let nid = self.data();
        // SAFETY: nid identifies our icon by hwnd and uID.
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
    }
}
