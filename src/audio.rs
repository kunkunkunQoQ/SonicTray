//! Core Audio 监听。
//!
//! 职责：获取「默认播放设备（eRender/eConsole）」的静音状态与主音量，
//! 并通过 Windows Core Audio 的事件通知（`IAudioEndpointVolumeCallback` /
//! `IMMNotificationClient`）感知变化。
//!
//! 线程模型：所有 COM 对象都在主线程创建与使用（MTA）。音频服务的回调可能
//! 运行在任意线程，因此回调内部**只做一件事**——向隐藏窗口 PostMessage，
//! 真正的读取与界面刷新都在主线程的消息处理里完成。

use windows::core::{implement, PCWSTR, Result as WinResult, BOOL, GUID};
use windows::Win32::Foundation::{HWND, LPARAM, PROPERTYKEY, WPARAM};
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    eConsole, eRender, AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE, EDataFlow, ERole,
    IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// 主音量或静音标志发生变化。
pub const WM_AUDIO_CHANGED: u32 = WM_APP + 10;
/// 默认播放设备变化，或设备被添加/移除/状态改变。
pub const WM_DEVICE_CHANGED: u32 = WM_APP + 11;

/// `MMDeviceEnumerator` 的 CLSID（windows crate 未导出该常量）。
const CLSID_MMDEVICE_ENUMERATOR: GUID = GUID::from_u128(0xBCDE0395_E52F_467C_8E3D_C4579291692E);

/// 主音量低于该阈值即视为「音量为 0」。
const SILENCE_THRESHOLD: f32 = 0.0001;

/// 托盘需要展示的两种状态。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AudioState {
    Normal,
    Muted,
}

/// 状态判定：**静音标志为真，或主音量为 0** 时显示静音图标。
///
/// 音量为 0 只改变本程序的显示，绝不去动系统的静音标志。
pub fn decide(muted: bool, volume: f32) -> AudioState {
    if muted || volume <= SILENCE_THRESHOLD {
        AudioState::Muted
    } else {
        AudioState::Normal
    }
}

/// 把「状态可能变了」投递给主线程的隐藏窗口。可以在任意线程调用。
fn wake(hwnd: isize, msg: u32) {
    if hwnd == 0 {
        return;
    }
    unsafe {
        let _ = PostMessageW(
            Some(HWND(hwnd as *mut core::ffi::c_void)),
            msg,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

/// 音量/静音变化回调。运行在音频服务线程，不做任何读取或界面操作。
#[implement(IAudioEndpointVolumeCallback)]
struct VolumeNotify {
    hwnd: isize,
}

impl IAudioEndpointVolumeCallback_Impl for VolumeNotify_Impl {
    fn OnNotify(&self, _data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> WinResult<()> {
        wake(self.hwnd, WM_AUDIO_CHANGED);
        Ok(())
    }
}

/// 设备变化回调。同样只投递消息。
#[implement(IMMNotificationClient)]
struct DeviceNotify {
    hwnd: isize,
}

impl IMMNotificationClient_Impl for DeviceNotify_Impl {
    fn OnDeviceStateChanged(&self, _id: &PCWSTR, _state: DEVICE_STATE) -> WinResult<()> {
        wake(self.hwnd, WM_DEVICE_CHANGED);
        Ok(())
    }

    fn OnDeviceAdded(&self, _id: &PCWSTR) -> WinResult<()> {
        Ok(())
    }

    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> WinResult<()> {
        // 被移除的可能正是当前默认设备，需要重新绑定。
        wake(self.hwnd, WM_DEVICE_CHANGED);
        Ok(())
    }

    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _id: &PCWSTR) -> WinResult<()> {
        // 只关心默认播放设备，默认录音/通信设备变化不得影响图标。
        if flow == eRender && role == eConsole {
            wake(self.hwnd, WM_DEVICE_CHANGED);
        }
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> WinResult<()> {
        // 属性变化（改名、图标等）与静音状态无关。
        Ok(())
    }
}

/// 当前绑定的默认设备及其音量接口 + 已注册的回调（必须成对持有，注销时要用同一实例）。
struct Binding {
    volume: IAudioEndpointVolume,
    callback: IAudioEndpointVolumeCallback,
}

/// 默认播放设备的静音状态监听器。
pub struct AudioMonitor {
    enumerator: IMMDeviceEnumerator,
    client: Option<IMMNotificationClient>,
    binding: Option<Binding>,
    hwnd: isize,
}

impl AudioMonitor {
    /// 创建监听器并注册设备变化通知。设备暂时不可用不会导致失败。
    pub fn new(hwnd: HWND) -> WinResult<Self> {
        let hwnd = hwnd.0 as isize;
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&CLSID_MMDEVICE_ENUMERATOR, None, CLSCTX_ALL)?;
            let notify: IMMNotificationClient = DeviceNotify { hwnd }.into();
            enumerator.RegisterEndpointNotificationCallback(&notify)?;

            let mut monitor = Self {
                enumerator,
                client: Some(notify),
                binding: None,
                hwnd,
            };
            // 音频服务可能尚未就绪，绑定失败交由看门狗重试。
            let _ = monitor.bind();
            Ok(monitor)
        }
    }

    /// 绑定当前默认播放设备，并注册音量变化回调。
    fn bind(&mut self) -> WinResult<()> {
        unsafe {
            let device = self.enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
            let callback: IAudioEndpointVolumeCallback = VolumeNotify { hwnd: self.hwnd }.into();
            volume.RegisterControlChangeNotify(&callback)?;
            self.binding = Some(Binding { volume, callback });
        }
        Ok(())
    }

    /// 解除旧设备的音量回调并释放引用。重复调用安全。
    fn unbind(&mut self) {
        if let Some(binding) = self.binding.take() {
            unsafe {
                let _ = binding
                    .volume
                    .UnregisterControlChangeNotify(&binding.callback);
            }
            // binding 在此处析构：回调与音量接口的引用计数各自减一。
        }
    }

    /// 读取当前状态。读取失败返回 `None`（调用方必须保持原图标，不能当作未静音）。
    pub fn read(&self) -> Option<AudioState> {
        let binding = self.binding.as_ref()?;
        unsafe {
            let muted: BOOL = binding.volume.GetMute().ok()?;
            let volume: f32 = binding.volume.GetMasterVolumeLevelScalar().ok()?;
            Some(decide(muted.as_bool(), volume))
        }
    }

    /// 读取状态；读失败或强制重绑时，先解除旧回调再绑定新设备后重试一次。
    ///
    /// `force_rebind` 用于「默认设备已切换」的场景：旧设备可能仍然可读，
    /// 但读到的已经不是当前默认设备的数值。
    pub fn sync(&mut self, force_rebind: bool) -> Option<AudioState> {
        if !force_rebind {
            if let Some(state) = self.read() {
                return Some(state);
            }
        }
        self.unbind();
        if self.bind().is_err() {
            return None;
        }
        self.read()
    }
}

impl Drop for AudioMonitor {
    fn drop(&mut self) {
        self.unbind();
        if let Some(client) = self.client.take() {
            unsafe {
                let _ = self.enumerator.UnregisterEndpointNotificationCallback(&client);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 覆盖需求 4.1 判定表的每一行。
    #[test]
    fn decision_table_matches_spec() {
        assert_eq!(decide(false, 0.50), AudioState::Normal);
        assert_eq!(decide(false, 1.00), AudioState::Normal);
        assert_eq!(decide(false, 0.00), AudioState::Muted);
        assert_eq!(decide(true, 0.50), AudioState::Muted);
        assert_eq!(decide(true, 1.00), AudioState::Muted);
        // 取消静音且音量大于 0 → 恢复正常
        assert_eq!(decide(false, 0.02), AudioState::Normal);
        // 极小但非零的音量仍按正常显示
        assert_eq!(decide(false, 0.001), AudioState::Normal);
    }
}
