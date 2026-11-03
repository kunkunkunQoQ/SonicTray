//! 系统托盘：图标注册/更新/删除，以及原生右键菜单。

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, NOTIFY_ICON_MESSAGE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, PostMessageW, SetForegroundWindow,
    TrackPopupMenuEx, HICON, MF_CHECKED, MF_SEPARATOR, MF_STRING, TPM_LEFTALIGN, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WM_APP, WM_NULL,
};

/// 托盘图标的回调消息（`lParam` 低 16 位是鼠标消息）。
pub const WM_TRAYICON: u32 = WM_APP + 1;
/// 菜单命令：开机自启（勾选 / 取消）。
pub const MENU_AUTOSTART: usize = 1;
/// 菜单命令：清除开机自启（按值名删除，与 EXE 当前位置无关）。
pub const MENU_CLEAR_AUTOSTART: usize = 2;
/// 菜单命令：退出。
pub const MENU_QUIT: usize = 3;

const ICON_ID: u32 = 1;

pub struct Tray {
    hwnd: HWND,
    added: bool,
}

impl Tray {
    pub fn new(hwnd: HWND) -> Self {
        Self { hwnd, added: false }
    }

    fn data(&self, icon: HICON, tip: &str) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: ICON_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAYICON,
            hIcon: icon,
            ..Default::default()
        };
        // szTip 初始全 0，按上限写入即可保证结尾有 NUL。
        let limit = data.szTip.len() - 1;
        for (slot, unit) in data.szTip.iter_mut().zip(tip.encode_utf16().take(limit)) {
            *slot = unit;
        }
        data
    }

    fn notify(&self, message: NOTIFY_ICON_MESSAGE, data: &NOTIFYICONDATAW) -> bool {
        unsafe { Shell_NotifyIconW(message, data).as_bool() }
    }

    /// 添加托盘图标。
    pub fn add(&mut self, icon: HICON, tip: &str) {
        let ok = self.notify(NIM_ADD, &self.data(icon, tip));
        self.added = ok;
    }

    /// 更新图标与提示文字；图标已丢失（如 Explorer 重启）时重新添加。
    pub fn update(&mut self, icon: HICON, tip: &str) {
        if !self.added || !self.notify(NIM_MODIFY, &self.data(icon, tip)) {
            self.add(icon, tip);
        }
    }

    /// Explorer 重启后重新注册：先删后加，保证不会出现两个图标。
    pub fn re_add(&mut self, icon: HICON, tip: &str) {
        self.remove();
        self.add(icon, tip);
    }

    /// 退出前移除托盘图标。
    pub fn remove(&mut self) {
        if self.added {
            self.notify(NIM_DELETE, &self.data(HICON::default(), ""));
            self.added = false;
        }
    }
}

/// 弹出右键菜单，返回被选中的命令（0 表示取消）。
pub fn show_context_menu(hwnd: HWND, autostart_checked: bool) -> usize {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return 0 };
        let mut flags = MF_STRING;
        if autostart_checked {
            flags |= MF_CHECKED;
        }
        let _ = AppendMenuW(menu, flags, MENU_AUTOSTART, w!("开机自启"));
        let _ = AppendMenuW(menu, MF_STRING, MENU_CLEAR_AUTOSTART, w!("清除开机自启"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("退出"));

        // 必须先置前台，否则点击菜单外部时菜单不会消失。
        let _ = SetForegroundWindow(hwnd);
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);

        let command = TrackPopupMenuEx(
            menu,
            (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN).0,
            point.x,
            point.y,
            hwnd,
            None,
        );

        let _ = DestroyMenu(menu);
        // 文档推荐的收尾动作，避免菜单残影。
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        command.0 as usize
    }
}
