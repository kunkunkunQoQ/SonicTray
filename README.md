# Sonic Tray

Windows 系统静音状态托盘指示器。后台常驻、无主窗口，只在自己那个托盘图标上切换
「正常 / 静音」两种 Windows 原生风格图标，**不修改**任何系统音量或静音设置。

- 语言：Rust（Stable），`x86_64-pc-windows-msvc`
- 界面：Win32 消息循环 + `Shell_NotifyIconW` + 原生弹出菜单（无任何 GUI 框架）
- 音频：Windows Core Audio COM（`IMMDeviceEnumerator` / `IAudioEndpointVolume` /
  `IAudioEndpointVolumeCallback` / `IMMNotificationClient`）
- 依赖：只有 `windows` / `windows-core` crate，运行时零外部依赖
- 交付：单个 `SonicTray.exe`（静态 CRT，不依赖 VC++ 运行库 / .NET / Python）

> 需求文档里的原名称是 `MuteTray`；按后续指令软件名改为 **Sonic Tray**，
> 因此工程目录为 `SonicTray/`、可执行文件为 `SonicTray.exe`。

---

## 1. 运行

直接双击 `publish/SonicTray.exe`（或 `target/release/SonicTray.exe`）。

- 启动后没有窗口、没有控制台，只在通知区域多出一个扬声器图标。
- Windows 11 默认把新图标放进「隐藏的图标」里，可点任务栏的 `^` 找到它，
  或在「任务栏设置 → 其他系统托盘图标」中把它设为常显。
- 重复启动不会产生第二个图标：第二个实例检测到命名互斥体后静默退出。

## 2. 交互

| 操作 | 行为 |
|---|---|
| 左键单击 | 无动作 |
| 右键单击 | 弹出菜单：`开机自启`（可勾选）、`清除开机自启`、`退出` |
| 鼠标悬停 | 提示 `系统声音:正常` / `系统声音:静音` |

## 3. 开机自启

菜单勾选即可，写入的是**当前用户**的启动项，不需要管理员权限、不碰任务计划程序：

```
HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run
    SonicTray = "完整\路径\SonicTray.exe"
```

**勾选状态的判定**：值存在**且**指向当前这个 EXE，才算「已开启」。
所以把 EXE 移动到别的目录后，菜单里会如实显示为未勾选（旧路径已经启动不了新位置），
重新勾选一次即可把路径更新到当前位置。

**清除开机自启**：按值名 `SonicTray` 删除，**与 EXE 现在放在哪里无关**，
哪怕旧的值指向一个已经不存在的目录也能清掉；同时会顺带清理其它以
SonicTray 命名的残留值（例如手工改名或更早版本留下的）。
取消勾选走的是同一套清除逻辑。

## 4. 退出

右键 → `退出`。程序会注销音量回调与设备通知、移除托盘图标、销毁图标句柄并结束进程，
不留下托盘残留或后台子进程。

## 5. 状态判定

```
显示静音图标  <=>  系统静音标志为 true  ||  主音量 <= 0.0001
```

| 系统状态 | 程序显示 |
|---|---|
| 音量 50%，未静音 | 正常扬声器 |
| 音量 100%，未静音 | 正常扬声器 |
| 音量 0%，未开静音标志 | 静音扬声器 |
| 音量 50%/100%，开启静音 | 静音扬声器 |
| 取消静音且音量 > 0 | 正常扬声器 |

音量为 0 只影响本程序的显示，**不会**去写系统静音标志。

## 6. 图标

`assets/` 里是四个多尺寸 ICO（16/20/24/32/40/48/64，每档独立光栅化，非缩放）：

| 文件 | 用途 |
|---|---|
| `volume_light.ico` | 浅色任务栏 → 深色扬声器（带波纹） |
| `mute_light.ico` | 浅色任务栏 → 深色静音扬声器（带 X） |
| `volume_dark.ico` | 深色任务栏 → 浅色扬声器（带波纹） |
| `mute_dark.ico` | 深色任务栏 → 浅色静音扬声器（带 X） |
| `app.ico` | EXE 自身图标（含 256 PNG 帧） |

- 四个 ICO 通过 `include_bytes!` **编译进 EXE**，运行时不读取任何外部文件。
- 运行时按当前 DPI 的小图标尺寸（`SM_CXSMICON`）挑选同尺寸帧创建 `HICON`，
  不缩放、不反复创建。
- 深浅色取自 `HKCU\...\Themes\Personalize\SystemUsesLightTheme`；
  收到 `WM_SETTINGCHANGE` 后重新读取，切换主题立即生效，无需重启。
- 图标可用 `python tools/gen_icons.py` 重新生成（依赖 Pillow），
  同时输出 `tools/icon_preview.png` 供目视检查。

## 7. 事件与线程模型

| 线程 | 做什么 |
|---|---|
| 主线程（STA 消息循环，COM 为 MTA） | 创建窗口/COM 对象、读状态、改托盘图标、弹菜单 |
| 音频服务线程 | 只执行回调里的 `PostMessageW`，不碰任何界面或 COM |

- 音量变化：`IAudioEndpointVolumeCallback::OnNotify` → `WM_AUDIO_CHANGED` → 主线程重读状态。
- 设备切换：`IMMNotificationClient::OnDefaultDeviceChanged`（仅 `eRender` + `eConsole`）
  → `WM_DEVICE_CHANGED` → **先注销旧设备回调再绑定新设备**，避免重复注册和旧回调干扰。
- Explorer 重启：监听 `TaskbarCreated` 广播，先 `NIM_DELETE` 再 `NIM_ADD`，不会出现两个图标。
- 兜底：5 秒一次的低频定时器（`WM_TIMER`）只在读取失败或状态不一致时动作，
  正常运行时 CPU 占用接近 0。这一项是为了满足「设备/音频服务短暂不可用时自动恢复」，
  常规状态变化全部由系统事件驱动，**没有**高频轮询。
- 读不到状态时保持当前图标（绝不把「读不到」当成「未静音」）；启动时若完全读不到，
  按静音图标降级显示，恢复后立即纠正。

## 8. 目录结构

```
SonicTray/
├── Cargo.toml
├── Cargo.lock
├── build.rs                编译 EXE 文件图标 + 版本信息（找不到 rc.exe 自动跳过）
├── .cargo/config.toml      静态 CRT：产物不依赖 vcruntime140.dll
├── src/
│   ├── main.rs             入口、隐藏消息窗口、消息循环、App 状态机
│   ├── audio.rs            Core Audio 监听（COM 接口 + 回调 + 判定表单测）
│   ├── tray.rs             Shell_NotifyIconW 封装与右键菜单
│   ├── icons.rs            ICO 内嵌、按尺寸取帧、HICON 生命周期、主题判定
│   ├── startup.rs          HKCU Run 开机自启
│   └── single_instance.rs  命名互斥体
├── assets/                 四个托盘 ICO + app.ico + app.rc
├── tools/gen_icons.py      图标生成脚本（可选，已生成好资源）
├── publish/SonicTray.exe   已编译产物（单文件）
└── README.md
```

## 9. 构建

需要 Rust Stable + MSVC 链接器（Visual Studio 生成工具）与 Windows SDK。

```bash
cargo build --release          # 产物：target/release/SonicTray.exe（单文件）
cargo test                     # 单元测试：判定表、图标取帧、自启动往返
```

- **单文件**：`.cargo/config.toml` 打开 `+crt-static`，C 运行库静态链入，
  产物只依赖 `kernel32/user32/shell32/ole32/oleaut32/advapi32/combase` 等系统 DLL，
  换一台干净的 Windows 10/11 直接能跑，无需安装 VC++ 运行库。
- **EXE 文件图标**：`build.rs` 会自动在 Windows SDK 里找 `rc.exe`，
  把 `assets/app.rc`（图标 + 版本信息）编译后交给链接器。
  找不到 `rc.exe` 时只打印一条 warning 并跳过，**不会中断构建**，托盘功能不受影响。
- 发布配置：`opt-level="s"`、`lto=true`、`codegen-units=1`、`strip=true`，
  产物约 457 KB。

## 10. 已知限制

- 只在 100%~150% 缩放的 Windows 11 x64 上实测；Windows 10 走的是同一套 API，
  未单独回归。
- 托盘图标是否常显由 Windows 决定，程序不会（也不应该）去改任务栏结构。
