//! 单实例：命名 Mutex。第二个实例拿不到所有权就直接退出，不弹窗、不建第二个图标。

use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Threading::CreateMutexW;

const MUTEX_NAME: windows::core::PCWSTR = w!(r"Local\SonicTray.SingleInstance.v1");

/// 持有即表示本进程是唯一实例；析构时关闭句柄。
pub struct SingleInstance {
    handle: HANDLE,
}

impl SingleInstance {
    /// 拿到命名 Mutex 则返回守卫；已存在同名实例则返回 `None`。
    pub fn acquire() -> Option<Self> {
        unsafe {
            let handle = CreateMutexW(None, false, MUTEX_NAME).ok()?;
            // CreateMutexW 成功后 GetLastError() 仍保留 ERROR_ALREADY_EXISTS，据此判断。
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                None
            } else {
                Some(Self { handle })
            }
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}
