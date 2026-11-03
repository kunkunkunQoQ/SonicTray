//! 构建脚本：把 `assets/app.rc`（EXE 文件图标 + 版本信息）编译成 .res 并交给链接器。
//!
//! 只影响「EXE 在资源管理器里的图标 / 属性」；托盘图标由 `include_bytes!` 内嵌，
//! 与此无关。找不到 rc.exe 时只打印警告、不中断构建。

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");

    let Some(rc) = find_rc() else {
        println!("cargo:warning=未找到 rc.exe，跳过 EXE 文件图标（托盘图标不受影响）");
        return;
    };

    let out_dir = env::var("OUT_DIR").unwrap_or_else(|_| ".".to_string());
    let res = PathBuf::from(out_dir).join("sonictray.res");

    let status = Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg("app.rc")
        .current_dir("assets")
        .status();

    match status {
        Ok(status) if status.success() => {
            println!("cargo:rustc-link-arg-bin=SonicTray={}", res.display());
        }
        Ok(status) => println!("cargo:warning=rc.exe 退出码 {status}，跳过 EXE 文件图标"),
        Err(error) => println!("cargo:warning=无法执行 rc.exe：{error}"),
    }
}

/// 在 Windows SDK 的 bin 目录里找版本最高的 x64 rc.exe。
fn find_rc() -> Option<PathBuf> {
    const ROOTS: [&str; 2] = [
        r"C:\Program Files (x86)\Windows Kits\10\bin",
        r"C:\Program Files\Windows Kits\10\bin",
    ];

    let mut best: Option<(Vec<u32>, PathBuf)> = None;
    for root in ROOTS {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let version_dir = entry.path();
            let candidate = version_dir.join("x64").join("rc.exe");
            if !candidate.is_file() {
                continue;
            }
            let version: Vec<u32> = version_dir
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.split('.').filter_map(|part| part.parse().ok()).collect())
                .unwrap_or_default();
            if best.as_ref().is_none_or(|(current, _)| version > *current) {
                best = Some((version, candidate));
            }
        }
    }
    best.map(|(_, path)| path)
}
