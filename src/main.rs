//! Sonic Tray —— Windows 系统静音状态托盘指示器。
//!
//! 后台常驻、无主窗口：用 Core Audio 的事件通知跟踪「默认播放设备的静音状态」，
//! 只在自己那个托盘图标上切换「正常 / 静音」两种原生风格图标，
//! 不修改系统音量、不接管系统音量图标、不联网。

#![windows_subsystem = "windows"]

mod audio;
mod icons;
mod single_instance;
mod startup;
mod tray;

use std::cell::RefCell;
use std::thread::sleep;
use std::time::Duration;

use windows::core::{w, Error, Result as WinResult};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, KillTimer,
    PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetTimer, TranslateMessage, MSG,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CONTEXTMENU, WM_ENDSESSION, WM_POWERBROADCAST, WM_RBUTTONUP,
    WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW,
};

use audio::{AudioMonitor, AudioState};

/// 低频看门狗：只用于异常恢复与兜底校验，正常变化全部由系统事件驱动。
const TIMER_WATCHDOG: usize = 1;
const WATCHDOG_INTERVAL_MS: u32 = 5_000;

/// 启动时读取状态的尝试次数（音频服务冷启动可能慢一拍）。
const STARTUP_ATTEMPTS: usize = 3;
const STARTUP_RETRY_DELAY_MS: u64 = 80;

// PBT_APMRESUMESUSPEND / PBT_APMRESUMEAUTOMATIC：睡眠/休眠恢复
const PBT_APMRESUMESUSPEND: usize = 0x0007;
const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn tip_for(muted: bool) -> &'static str {
    if muted {
        "系统声音:静音"
    } else {
        "系统声音:正常"
    }
}

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    // 已有实例则静默退出，不创建第二个托盘图标。
    let _instance = match single_instance::SingleInstance::acquire() {
        Some(guard) => guard,
        None => return,
    };

    let com_initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();

    let _ = run();

    if com_initialized {
        unsafe { CoUninitialize() };
    }
}

fn run() -> WinResult<()> {
    let hwnd = create_hidden_window()?;
    let app = App::start(hwnd)?;

    APP.with(|cell| *cell.borrow_mut() = Some(app));
    message_loop();

    // 让 App 离开作用域：注销 COM 回调、销毁图标句柄。
    let app = APP.with(|cell| cell.borrow_mut().take());
    drop(app);

    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    Ok(())
}

/// 不可见、不带 WS_VISIBLE 的顶层窗口：接收托盘回调、广播消息（TaskbarCreated /
/// WM_SETTINGCHANGE）和定时器。消息窗口（HWND_MESSAGE）收不到广播，所以必须是顶层窗口。
fn create_hidden_window() -> WinResult<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class_name = w!("SonicTray.HiddenWindow");

        let window_class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: HINSTANCE(instance.0),
            lpszClassName: class_name,
            ..Default::default()
        };
        if RegisterClassW(&window_class) == 0 {
            return Err(Error::from_thread());
        }

        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!("Sonic Tray"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(HINSTANCE(instance.0)),
            None,
        )
    }
}

fn message_loop() {
    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // 菜单是模态的（内部会跑消息循环），此时重入本函数会让 RefCell 借用失败；
    // 交给 DefWindowProc，菜单关闭后由下一轮事件补齐状态。
    let handled = APP.with(|cell| match cell.try_borrow_mut() {
        Ok(mut slot) => slot.as_mut().and_then(|app| app.dispatch(message, wparam, lparam)),
        Err(_) => None,
    });

    match handled {
        Some(result) => result,
        None => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

struct App {
    hwnd: HWND,
    monitor: Option<AudioMonitor>,
    icons: icons::Icons,
    tray: tray::Tray,
    light_taskbar: bool,
    /// 当前显示的图标语义；`None` 表示还没成功读到过状态。
    shown_muted: Option<bool>,
    taskbar_created: u32,
    shutting_down: bool,
}

impl App {
    /// 构造并完成「启动同步」：先读到真实状态，再创建托盘图标。
    fn start(hwnd: HWND) -> WinResult<Self> {
        let mut app = Self {
            hwnd,
            monitor: None,
            icons: icons::Icons::load()?,
            tray: tray::Tray::new(hwnd),
            light_taskbar: icons::is_light_taskbar(),
            shown_muted: None,
            taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
            shutting_down: false,
        };

        let mut state = None;
        for attempt in 0..STARTUP_ATTEMPTS {
            state = app.read_state(attempt == 0);
            if state.is_some() {
                break;
            }
            sleep(Duration::from_millis(STARTUP_RETRY_DELAY_MS));
        }

        // 读不到状态时按静音降级显示，避免把「未知」误报成「正常」；
        // 看门狗会在音频服务恢复后立即纠正。
        let muted = !matches!(state, Some(AudioState::Normal));
        app.shown_muted = Some(muted);
        app.tray
            .add(app.icons.get(app.light_taskbar, muted), tip_for(muted));

        unsafe {
            SetTimer(Some(hwnd), TIMER_WATCHDOG, WATCHDOG_INTERVAL_MS, None);
        }
        Ok(app)
    }

    fn dispatch(&mut self, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        if message != 0 && message == self.taskbar_created {
            // Explorer 重启：重新注册图标，音频监听不受影响。
            self.refresh_icon();
            self.sync(true);
        } else {
            match message {
                tray::WM_TRAYICON => self.on_tray_event(lparam),
                audio::WM_AUDIO_CHANGED => self.sync(false),
                audio::WM_DEVICE_CHANGED => self.sync(true),
                WM_SETTINGCHANGE => self.apply_theme(),
                WM_TIMER if wparam.0 == TIMER_WATCHDOG => self.sync(false),
                WM_POWERBROADCAST
                    if wparam.0 == PBT_APMRESUMESUSPEND || wparam.0 == PBT_APMRESUMEAUTOMATIC =>
                {
                    self.sync(true)
                }
                WM_ENDSESSION if wparam.0 != 0 => self.shutdown(),
                _ => return None,
            }
        }
        Some(LRESULT(0))
    }

    fn on_tray_event(&mut self, lparam: LPARAM) {
        // 经典 NOTIFYICON：lParam 低 16 位是鼠标消息（左键不做任何事）。
        let event = (lparam.0 as u32) & 0xFFFF;
        if event != WM_RBUTTONUP && event != WM_CONTEXTMENU {
            return;
        }

        match tray::show_context_menu(self.hwnd, startup::is_enabled()) {
            tray::MENU_AUTOSTART => {
                let _ = startup::set_enabled(!startup::is_enabled());
            }
            // 与当前 EXE 路径无关地清除自启动项（含移动过位置后的残留）。
            tray::MENU_CLEAR_AUTOSTART => {
                let _ = startup::clear_all();
            }
            tray::MENU_QUIT => self.shutdown(),
            _ => {}
        }
    }

    /// 主题变化：重新读取深浅色并刷新图标，无需重启程序。
    fn apply_theme(&mut self) {
        let light = icons::is_light_taskbar();
        if light == self.light_taskbar {
            return;
        }
        let Ok(fresh) = icons::Icons::load() else { return };

        self.light_taskbar = light;
        let previous = std::mem::replace(&mut self.icons, fresh);
        self.repaint();
        // 新图标设置成功之后才销毁旧句柄。
        drop(previous);
    }

    fn read_state(&mut self, force_rebind: bool) -> Option<AudioState> {
        if self.monitor.is_none() {
            self.monitor = AudioMonitor::new(self.hwnd).ok();
        }
        self.monitor.as_mut()?.sync(force_rebind)
    }

    /// 同步一次状态。读取失败时保持当前图标，绝不把「读不到」当成「未静音」。
    fn sync(&mut self, force_rebind: bool) {
        let Some(state) = self.read_state(force_rebind) else {
            return;
        };
        let muted = state == AudioState::Muted;
        if self.shown_muted == Some(muted) {
            return;
        }
        self.shown_muted = Some(muted);
        self.repaint();
    }

    /// 按当前主题与状态刷新托盘图标与提示文字。
    fn repaint(&mut self) {
        let muted = self.shown_muted.unwrap_or(false);
        self.tray
            .update(self.icons.get(self.light_taskbar, muted), tip_for(muted));
    }

    /// Explorer 重启后重新注册托盘图标。
    fn refresh_icon(&mut self) {
        let muted = self.shown_muted.unwrap_or(false);
        self.tray
            .re_add(self.icons.get(self.light_taskbar, muted), tip_for(muted));
    }

    fn shutdown(&mut self) {
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;

        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_WATCHDOG);
        }
        self.tray.remove();
        // 注销音量回调与设备通知，并释放所有 COM 引用。
        self.monitor = None;
        unsafe { PostQuitMessage(0) };
    }
}
