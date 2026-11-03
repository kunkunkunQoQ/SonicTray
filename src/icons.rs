//! 图标资源：四个多尺寸 ICO 在编译期通过 `include_bytes!` 嵌入 EXE，
//! 运行时按当前 DPI 的小图标尺寸挑选同尺寸的一帧创建 `HICON`。
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

/// 顺序即 `Icons::load` 里句柄数组的下标顺序。
const SOURCES: [&[u8]; 4] = [
    include_bytes!("../assets/volume_light.ico"),
    include_bytes!("../assets/volume_dark.ico"),
    include_bytes!("../assets/mute_light.ico"),
    include_bytes!("../assets/mute_dark.ico"),
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

/// 一种主题下的一对图标（正常 / 静音）。
struct Pair {
    normal: HICON,
    muted: HICON,
}

impl Drop for Pair {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyIcon(self.normal);
            let _ = DestroyIcon(self.muted);
        }
    }
}

/// 四种状态的全部图标句柄，进程生命周期内一直持有，避免反复创建。
pub struct Icons {
    light: Pair,
    dark: Pair,
}

impl Icons {
    /// 按当前 DPI 加载四个图标；任一失败则释放已创建的句柄并返回错误。
    pub fn load() -> WinResult<Self> {
        let size = small_icon_size();
        let mut handles = vec![HICON::default(); 4];
        for (index, source) in SOURCES.iter().enumerate() {
            match make_icon(source, size) {
                Ok(icon) => handles[index] = icon,
                Err(error) => {
                    for created in handles {
                        unsafe { let _ = DestroyIcon(created); }
                    }
                    return Err(error);
                }
            }
        }

        Ok(Self {
            light: Pair { normal: handles[0], muted: handles[2] },
            dark: Pair { normal: handles[1], muted: handles[3] },
        })
    }

    /// 取出当前需要的图标句柄（句柄由 `Icons` 拥有，调用方不得销毁）。
    pub fn get(&self, light_taskbar: bool, muted: bool) -> HICON {
        let pair = if light_taskbar { &self.light } else { &self.dark };
        if muted {
            pair.muted
        } else {
            pair.normal
        }
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
        return true; // 读不到就按浅色任务栏处理
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
            match frame_for_size(SOURCES[0], target) {
                Some(frame) => assert_eq!(frame_width(frame), target, "尺寸 {target} 选错帧"),
                None => panic!("尺寸 {target} 没找到帧"),
            }
        }
    }

    /// 超出资源范围时退化为最大尺寸，且不会越界。
    #[test]
    fn falls_back_to_nearest_frame() {
        match frame_for_size(SOURCES[3], 512) {
            Some(frame) => assert_eq!(frame_width(frame), 64),
            None => panic!("没有回退帧"),
        }
        assert!(frame_for_size(&[], 16).is_none());
        assert!(frame_for_size(&[0, 0, 1, 0, 8, 0], 16).is_none());
    }

    /// 四种组合必须是四个不同且有效的图标句柄。
    #[test]
    fn four_distinct_icons() {
        let icons = match Icons::load() {
            Ok(icons) => icons,
            Err(_) => panic!("图标资源加载失败"),
        };
        let mut handles = vec![
            icons.get(true, false).0,
            icons.get(true, true).0,
            icons.get(false, false).0,
            icons.get(false, true).0,
        ];
        assert!(handles.iter().all(|handle| !handle.is_null()), "图标句柄为空");
        handles.sort();
        handles.dedup();
        assert_eq!(handles.len(), 4, "四种状态的图标句柄出现重复");
    }
}
