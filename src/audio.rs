//! Core Audio 监听：默认播放设备的静音状态与主音量。
//!
//! 所有 COM 对象都在主线程创建与使用（MTA）。音频服务的回调可能运行在任意线程，
//! 因此回调内部**只做一件事**——向隐藏窗口 PostMessage，真正的读取与刷新都在
//! 主线程的消息处理里完成。

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

/// 图标等级。静音优先级最高，其次按音量百分比分档。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VolumeState {
    Muted,
    Low,
    Medium,
    High,
}

impl VolumeState {
    /// 图标资源数组下标。
    pub const fn index(self) -> usize {
        match self {
            Self::Muted => 0,
            Self::Low => 1,
            Self::Medium => 2,
            Self::High => 3,
        }
    }
}

/// 一次读取的完整状态。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Snapshot {
    /// 系统静音标志。与「音量为 0」是两件事，提示文字需要区分。
    pub muted: bool,
    pub percent: u32,
    pub state: VolumeState,
}

/// 唯一的等级判定入口。
///
/// 静音或 0% → `Muted`；1–33 低、34–66 中、67–100 高（分界为项目近似值）。
pub fn evaluate(muted: bool, volume: f32) -> Snapshot {
    let percent = (volume.clamp(0.0, 1.0) * 100.0).round() as u32;
    let state = if muted || percent == 0 {
        VolumeState::Muted
    } else if percent <= 33 {
        VolumeState::Low
    } else if percent <= 66 {
        VolumeState::Medium
    } else {
        VolumeState::High
    };
    Snapshot { muted, percent, state }
}

/// 把「状态可能变了」投递给主线程的隐藏窗口，可在任意线程调用。
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
        Ok(())
    }
}

/// 当前绑定的默认设备：音量接口 + 已注册的回调（注销时必须用同一实例）。
struct Binding {
    volume: IAudioEndpointVolume,
    callback: IAudioEndpointVolumeCallback,
}

/// 默认播放设备的状态监听器。
pub struct AudioMonitor {
    enumerator: IMMDeviceEnumerator,
    client: Option<IMMNotificationClient>,
    binding: Option<Binding>,
    hwnd: isize,
}

impl AudioMonitor {
    /// 创建监听器并注册设备变化通知。设备暂时不可用不算失败。
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
            let _ = monitor.bind();
            Ok(monitor)
        }
    }

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
        }
    }

    /// 读取当前状态。读取失败返回 `None`（调用方保持原图标，不能当作未静音）。
    pub fn read(&self) -> Option<Snapshot> {
        let binding = self.binding.as_ref()?;
        unsafe {
            let muted: BOOL = binding.volume.GetMute().ok()?;
            let volume: f32 = binding.volume.GetMasterVolumeLevelScalar().ok()?;
            Some(evaluate(muted.as_bool(), volume))
        }
    }

    /// 读取状态；读失败或 `force_rebind` 时先解绑再重绑后重试一次。
    ///
    /// `force_rebind` 用于「默认设备已切换」：旧设备可能仍可读，但数值已不属于当前设备。
    pub fn sync(&mut self, force_rebind: bool) -> Option<Snapshot> {
        if !force_rebind {
            if let Some(snapshot) = self.read() {
                return Some(snapshot);
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

    fn state_of(muted: bool, volume: f32) -> VolumeState {
        evaluate(muted, volume).state
    }

    /// 覆盖需求里的分级表与静音优先级。
    #[test]
    fn level_table() {
        assert_eq!(state_of(false, 0.00), VolumeState::Muted);
        assert_eq!(state_of(false, 0.10), VolumeState::Low);
        assert_eq!(state_of(false, 0.30), VolumeState::Low);
        assert_eq!(state_of(false, 0.33), VolumeState::Low);
        assert_eq!(state_of(false, 0.34), VolumeState::Medium);
        assert_eq!(state_of(false, 0.50), VolumeState::Medium);
        assert_eq!(state_of(false, 0.66), VolumeState::Medium);
        assert_eq!(state_of(false, 0.67), VolumeState::High);
        assert_eq!(state_of(false, 0.80), VolumeState::High);
        assert_eq!(state_of(false, 1.00), VolumeState::High);
        // 静音优先于任何音量
        assert_eq!(state_of(true, 0.85), VolumeState::Muted);
        assert_eq!(state_of(true, 1.00), VolumeState::Muted);
    }

    /// 百分比与「静音标志」是两件事，提示文字要能区分。
    #[test]
    fn percent_and_muted_flag() {
        let loud = evaluate(true, 0.65);
        assert_eq!(loud.percent, 65);
        assert!(loud.muted);
        assert_eq!(loud.state, VolumeState::Muted);

        let zero = evaluate(false, 0.0);
        assert_eq!(zero.percent, 0);
        assert!(!zero.muted);
        assert_eq!(zero.state, VolumeState::Muted);
    }
}
