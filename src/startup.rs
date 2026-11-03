//! 开机自启：只用当前用户的 HKCU Run 键，不触碰系统级启动项。
//!
//! 自启动项固定写在值名 `SonicTray` 下。**清除一律按值名删除**，
//! 因此无论 EXE 之后被移动到哪个目录，都能把旧的自启动项清掉。

use std::ffi::c_void;

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteKeyValueW, RegDeleteValueW, RegEnumValueW, RegGetValueW, RegOpenKeyExW,
    RegSetKeyValueW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RRF_RT_REG_SZ,
};

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
/// 固定值名：与本程序当前路径无关。
const VALUE_NAME: PCWSTR = w!("SonicTray");
/// 清理残留时用的名字特征（先去掉空格/连字符/下划线再比较，忽略大小写）。
const NAME_HINT: &str = "sonictray";

/// 当前 EXE 的完整路径。
fn executable_path() -> Option<String> {
    let mut buffer = [0u16; 1024];
    let length = unsafe { GetModuleFileNameW(None, &mut buffer) } as usize;
    if length == 0 || length >= buffer.len() {
        return None;
    }
    Some(String::from_utf16_lossy(&buffer[..length]))
}

/// 写入 Run 键时使用的命令行（带引号，兼容路径中的空格）。
fn current_command() -> Option<String> {
    executable_path().map(|path| format!("\"{path}\""))
}

/// 去掉首尾引号与空白，便于比较路径。
fn strip_quotes(command: &str) -> &str {
    command.trim().trim_matches('"').trim()
}

/// 读取 `SonicTray` 值的内容；不存在则返回 `None`。
fn read_value() -> Option<String> {
    let mut buffer = [0u16; 512];
    let mut size = (buffer.len() * std::mem::size_of::<u16>()) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE_NAME,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let length = (size as usize / std::mem::size_of::<u16>()).min(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..length]);
    Some(text.trim_end_matches('\0').to_string())
}

/// 写入自启动项（内容为带引号的当前 EXE 路径）。
fn write_value(command: &str) -> bool {
    let wide: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE_NAME,
            REG_SZ.0,
            Some(wide.as_ptr() as *const c_void),
            (wide.len() * std::mem::size_of::<u16>()) as u32,
        )
    };
    status.is_ok()
}

/// 是否已开启开机自启：值不仅要存在，还必须指向**当前这个** EXE。
///
/// 这样 EXE 被移动后，菜单里的勾选状态会如实显示为未开启，
/// 用户重新勾选即可把路径更新到新位置。
pub fn is_enabled() -> bool {
    let stored = read_value();
    let current = current_command();
    match (stored, current) {
        (Some(stored), Some(current)) => strip_quotes(&stored).eq_ignore_ascii_case(strip_quotes(&current)),
        _ => false,
    }
}

/// 写入或清除自启动项。返回是否操作成功。
pub fn set_enabled(enabled: bool) -> bool {
    if !enabled {
        return clear_all();
    }
    match current_command() {
        Some(command) => write_value(&command),
        None => false,
    }
}

/// 清除本程序的自启动项：**只按值名删除，与 EXE 当前位置无关**。
///
/// 1. 删除固定值名 `SonicTray`（无论它指向旧路径还是已经不存在的目录）；
/// 2. 顺带清掉其它以 SonicTray 命名的残留值（例如手工改名或更早版本留下的）。
///
/// 返回是否真的删掉了内容。
pub fn clear_all() -> bool {
    let mut removed = false;

    if unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE_NAME) }.is_ok() {
        removed = true;
    }

    let mut key = HKEY::default();
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            None,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            &mut key,
        )
    };
    if opened.is_err() {
        return removed;
    }

    let mut index = 0u32;
    loop {
        let mut name = [0u16; 256];
        let mut name_length = name.len() as u32;
        let status = unsafe {
            RegEnumValueW(
                key,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_length,
                None,
                None,
                None,
                None,
            )
        };
        if status.is_err() {
            break;
        }

        let value_name = String::from_utf16_lossy(&name[..name_length as usize]);
        if matches_our_name(&value_name)
            && unsafe { RegDeleteValueW(key, PCWSTR(name.as_ptr())) }.is_ok()
        {
            removed = true;
            // 删除后后面的值会前移，同一索引需要重新枚举。
            continue;
        }
        index += 1;
    }

    unsafe {
        let _ = RegCloseKey(key);
    }
    removed
}

/// 值名是否属于本程序（忽略大小写、空格、连字符、下划线）。
fn matches_our_name(name: &str) -> bool {
    let normalized = name.to_lowercase().replace([' ', '-', '_', '.'], "");
    normalized.contains(NAME_HINT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 两个测试都会改动同一处注册表，必须串行执行。
    static GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn enable_and_disable_round_trip() {
        let _lock = GUARD.lock().unwrap_or_else(|error| error.into_inner());
        let original = is_enabled();

        assert!(set_enabled(true), "写入自启动项失败");
        assert!(is_enabled(), "写入后应显示为已开启");

        assert!(set_enabled(false), "清除自启动项失败");
        assert!(!is_enabled(), "清除后应为未开启");
        assert!(read_value().is_none(), "清除后注册表里不应还有值");

        if original {
            let _ = set_enabled(true);
        }
    }

    /// EXE 被移动后：值还在但指向旧路径——不该算作已开启，但必须能被清除。
    #[test]
    fn stale_path_is_not_enabled_but_is_clearable() {
        let _lock = GUARD.lock().unwrap_or_else(|error| error.into_inner());

        assert!(write_value("\"D:\\somewhere\\else\\SonicTray.exe\""), "写入测试值失败");
        assert!(!is_enabled(), "指向别处的值不应算作已开启");
        assert!(clear_all(), "应当能清除与当前路径无关的残留项");
        assert!(read_value().is_none(), "清除后注册表里不应还有值");
    }

    #[test]
    fn name_matching() {
        assert!(matches_our_name("SonicTray"));
        assert!(matches_our_name("sonictray.exe"));
        assert!(matches_our_name("Sonic Tray"));
        assert!(matches_our_name("sonic-tray"));
        assert!(!matches_our_name("OneDrive"));
        assert!(!matches_our_name("Steam"));
    }
}
