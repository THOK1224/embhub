//! embhub 桌面外壳。
//!
//! 这一层只做「把 core 的能力暴露成 command」。
//! 任何业务逻辑都必须写在 embhub-core 里——本文件里的 if/else 多一行都要警惕。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use serde::Serialize;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

#[derive(Serialize)]
struct ProbeResult {
    bytes: u64,
    hex: String,
}

#[tauri::command]
fn list_ports() -> Result<Vec<embhub_core::PortInfo>, String> {
    embhub_core::list_ports()
}

/// spike 用：阻塞读一小段时间，返回字节数与 HEX 预览。
///
/// 注意：这是**证明数据通路**的最小实现，不是最终形态。
/// Phase 1 会换成常驻读取线程 + 批量事件推送 + 背压（见 docs/PLAN.md 4.3）。
#[tauri::command]
async fn read_probe(port: String, baud: u32, seconds: u64) -> Result<ProbeResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        embhub_core::read_port(&port, baud, seconds, usize::MAX)
            .map(|(bytes, hex)| ProbeResult { bytes, hex })
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

/// 应用的私有数据目录（WebView2 缓存、后续的配置与日志索引都放这里）。
///
/// 为什么不用 Tauri 默认的 `app_local_data_dir()`（即 `%LOCALAPPDATA%\<identifier>`）：
///   1. Tauri 在窗口的 `data_directory` 为空时会强制填该目录，不存在就
///      `create_dir_all` 它（tauri-2.12.4/src/manager/webview.rs:562-576）。
///      在 DSH 沙箱（workspace-write）下这一步被拒绝 → panic：
///      `Failed to setup app: 拒绝访问。 (os error 5)`。
///   2. 本项目要「绿色便携」（docs/SPEC.md A8）：数据放在 exe 旁边，整个目录可拷走，
///      不往系统盘写东西。这条约束与沙箱限制正好同向。
///
/// 解析顺序：环境变量 `EMBHUB_DATA_DIR` → exe 同级的 `embhub-data/`。
fn resolve_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("EMBHUB_DATA_DIR") {
        return PathBuf::from(dir);
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("embhub-data")))
        .unwrap_or_else(|| PathBuf::from("embhub-data"))
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![list_ports, read_probe])
        .setup(|app| {
            let data_dir = resolve_data_dir();
            std::fs::create_dir_all(&data_dir)?;
            println!("[embhub] webview 数据目录 = {}", data_dir.display());

            // 窗口在代码里建（而不是 tauri.conf.json），唯一目的就是能指定 data_directory。
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("embhub — Phase 0 spike")
                .inner_size(960.0, 640.0)
                .data_directory(data_dir)
                .build()?;

            // 让「窗口已创建」这件事在无 GUI 的环境里也可被观测（供自动化验收读取）。
            if let Some(window) = app.get_webview_window("main") {
                println!("[embhub] 窗口已创建: {}", window.label());
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("embhub 启动失败");
}
