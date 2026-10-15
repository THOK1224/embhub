//! embhub-cli —— 同一 core 的无头入口（脚本化 / CI / 人工验收）。
//!
//! 用法：
//!   embhub-cli list
//!   embhub-cli read --port COM3 [--baud 115200] [--seconds 3]

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("list");

    match cmd {
        "list" => match embhub_core::list_ports() {
            Ok(ports) => {
                println!("{} 个串口", ports.len());
                for p in &ports {
                    let mut extra = String::new();
                    if let (Some(vid), Some(pid)) = (p.vid, p.pid) {
                        extra.push_str(&format!(" VID:PID={vid:04X}:{pid:04X}"));
                    }
                    if let Some(sn) = &p.serial_number {
                        extra.push_str(&format!(" SN={sn}"));
                    }
                    if let Some(prod) = &p.product {
                        extra.push_str(&format!(" product={prod}"));
                    }
                    println!("  {} [{}]{}", p.port_name, p.port_type, extra);
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("枚举失败: {e}");
                ExitCode::FAILURE
            }
        },
        "read" => {
            let get = |k: &str| -> Option<String> {
                args.iter()
                    .position(|a| a == k)
                    .and_then(|i| args.get(i + 1))
                    .cloned()
            };
            let port = match get("--port") {
                Some(p) => p,
                None => {
                    eprintln!("用法: embhub-cli read --port COMx [--baud 115200] [--seconds 3]");
                    return ExitCode::FAILURE;
                }
            };
            let baud: u32 = get("--baud")
                .and_then(|v| v.parse().ok())
                .unwrap_or(115_200);
            let seconds: u64 = get("--seconds").and_then(|v| v.parse().ok()).unwrap_or(3);

            println!("打开 {port} @ {baud}，最多读 {seconds}s …");
            match embhub_core::read_port(&port, baud, seconds, usize::MAX) {
                Ok((n, hex)) => {
                    println!("收到字节数 = {n}");
                    if !hex.is_empty() {
                        println!("前 256 字节 HEX: {hex}");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
        other => {
            eprintln!("未知子命令: {other}（可用: list | read）");
            ExitCode::FAILURE
        }
    }
}
