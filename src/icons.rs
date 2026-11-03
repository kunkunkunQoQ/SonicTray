//! 图标资源：8 个多尺寸 ICO（4 个音量等级 × 2 种主题）在编译期通过
//! `include_bytes!` 嵌入 EXE，启动时一次性创建全部 `HICON`，运行期间只切换句柄。
//!
//! 命名约定：`*_light` 用于**浅色任务栏**（图形是深色的），
//! `*_dark` 用于**深色任务栏**（图形是浅色的）。

use std::ffi::c_void;

use windows::core::{w, Error, Result as WinResult};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconFromResourceEx, DestroyIcon, GetSystemMetrics, HICON, IMAGE_FLAGS, SM_CXSMICON,
};

use crate::audio::VolumeState;

/// 顺序即 `VolumeState::index()`：静音 / 低 / 中 / 高。
const SOURCES: [[&[u8]; 4]; 2] = [
    [
        include_bytes!("../assets/volume_muted_light.ico"),
        include_bytes!("../assets/volume_low_light.ico"),
        include_bytes!("../assets/volume_medium_light.ico"),
        include_bytes!("../assets/volume_high_light.ico"),
    ],
    [
        include_bytes!("../assets/volume_muted_dark.ico"),
        include_bytes!("../assets/volume_low_dark.ico"),
        include_bytes!("../assets/volume_medium_dark.ico"),
        include_bytes!("../assets/volume_high_dark.ico"),
    ],
];

/// 从 ICO 目录里挑选尺寸最接近 `target` 的一帧，返回该帧的原始字节。
fn frame_for_size(ico: &[u8], target: u32) -> Option<&[u8]> {
    let count = u16::from_le_bytes([*ico.get(4)?, *ico.get(5)?]) as usize;
    let mut best: Option<(u32, &[u8])> = None;

    for index in 0..count {
        let entry = 6 + index * 16;
        let header = ico.get(entry..entry + 16)?;
        let width = if header[0] == 0 { 256 } else { header[0] as u32 };
        let length = u32::from_le_bytes([header[8], header[9], header[10], header[11]]) as usize;
        let offset = u32::from_le_bytes([header[12], header[13], header[14], header[15]]) as usize;
        let frame = ico.get(offset..offset.checked_add(length)?)?;
        let distance = width.abs_diff(target);
        if best.is_none_or(|(best_distance, _)| distance < best_distance) {
            best = Some((distance, frame));
        }
    }
    best.map(|(_, frame)| frame)
}

/// 由 ICO 帧数据创建 HICON（0x0003_0000 = 图标资源版本 3）。
fn make_icon(ico: &[u8], size: u32) -> WinResult<HICON> {
    let frame = frame_for_size(ico, size).ok_or_else(|| Error::from_hresult(E_FAIL))?;
    unsafe {
        CreateIconFromResourceEx(frame, true, 0x0003_0000, size as i32, size as i32, IMAGE_FLAGS(0))
    }
}

/// 一种主题下的四个等级图标。
struct Theme {
    handles: [HICON; 4],
}

impl Drop for Theme {
    fn drop(&mut self) {
        for icon in self.handles {
            unsafe {
                let _ = DestroyIcon(icon);
            }
        }
    }
}

/// 全部图标句柄，进程生命周期内一直持有。
pub struct Icons {
    light: Theme,
    dark: Theme,
}

impl Icons {
    /// 按当前 DPI 一次性加载 8 个图标；任一失败则释放已创建的句柄并返回错误。
    pub fn load() -> WinResult<Self> {
        let size = small_icon_size();
        let mut handles = [[HICON::default(); 4]; 2];

        for (theme, sources) in SOURCES.iter().enumerate() {
            for (level, source) in sources.iter().enumerate() {
                match make_icon(source, size) {
                    Ok(icon) => handles[theme][level] = icon,
                    Err(error) => {
                        for row in handles {
                            for icon in row {
                                unsafe { let _ = DestroyIcon(icon); }
                            }
                        }
                        return Err(error);
                    }
                }
            }
        }

        let [light, dark] = handles;
        Ok(Self { light: Theme { handles: light }, dark: Theme { handles: dark } })
    }

    /// 取出当前需要的图标句柄（由 `Icons` 拥有，调用方不得销毁）。
    pub fn get(&self, light_taskbar: bool, state: VolumeState) -> HICON {
        let theme = if light_taskbar { &self.light } else { &self.dark };
        theme.handles[state.index()]
    }
}

/// 托盘图标的推荐像素尺寸（随 DPI 变化：100% → 16，150% → 24）。
fn small_icon_size() -> u32 {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) };
    if size >= 8 {
        size as u32
    } else {
        16
    }
}

/// 任务栏是浅色主题吗？（`SystemUsesLightTheme` = 1，读不到按浅色处理）
pub fn is_light_taskbar() -> bool {
    let mut value: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return true;
    }
    value != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_width(frame: &[u8]) -> u32 {
        // ICO 里的 BMP 帧以 BITMAPINFOHEADER 开头：[0..4)=biSize，[4..8)=biWidth。
        assert_eq!(u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]), 40);
        u32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]])
    }

    /// 每个尺寸都应命中同尺寸的帧，而不是被拉伸。
    #[test]
    fn picks_exact_frame_per_size() {
        for target in [16u32, 20, 24, 32, 40, 48, 64] {
            match frame_for_size(SOURCES[0][3], target) {
                Some(frame) => assert_eq!(frame_width(frame), target, "尺寸 {target} 选错帧"),
                None => panic!("尺寸 {target} 没找到帧"),
            }
        }
    }

    /// 8 套图标必须互不相同且句柄有效。
    #[test]
    fn eight_distinct_icons() {
        let icons = match Icons::load() {
            Ok(icons) => icons,
            Err(_) => panic!("图标资源加载失败"),
        };
        let mut handles: Vec<*mut c_void> = Vec::new();
        for light in [true, false] {
            for state in [
                VolumeState::Muted,
                VolumeState::Low,
                VolumeState::Medium,
                VolumeState::High,
            ] {
                handles.push(icons.get(light, state).0);
            }
        }
        assert!(handles.iter().all(|handle| !handle.is_null()), "图标句柄为空");
        handles.sort();
        handles.dedup();
        assert_eq!(handles.len(), 8, "8 个图标句柄出现重复");
    }
}
