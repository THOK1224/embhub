//! embhub-core —— 与 UI 无关的核心层。
//!
//! 铁律：本 crate 的依赖里**不允许出现 tauri**（见 docs/PLAN.md 4.1）。
//! 这样 core 能被 `cargo test` 独立验证，也能被 `embhub-cli` 无头调用。

use serde::Serialize;

/// 串口端口的可序列化描述（Phase 1 会补 device_id / 标签等字段）。
#[derive(Debug, Clone, Serialize)]
pub struct PortInfo {
    pub port_name: String,
    pub port_type: String,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub serial_number: Option<String>,
    pub product: Option<String>,
}

/// 枚举当前系统上的串口。
pub fn list_ports() -> Result<Vec<PortInfo>, String> {
    let ports = serialport::available_ports().map_err(|e| e.to_string())?;
    Ok(ports
        .into_iter()
        .map(|p| {
            let (port_type, vid, pid, serial_number, product) = match p.port_type {
                serialport::SerialPortType::UsbPort(u) => (
                    "usb".to_string(),
                    Some(u.vid),
                    Some(u.pid),
                    u.serial_number.clone(),
                    u.product.clone(),
                ),
                serialport::SerialPortType::PciPort => ("pci".to_string(), None, None, None, None),
                serialport::SerialPortType::BluetoothPort => {
                    ("bluetooth".to_string(), None, None, None, None)
                }
                serialport::SerialPortType::Unknown => {
                    ("unknown".to_string(), None, None, None, None)
                }
            };
            PortInfo {
                port_name: p.port_name,
                port_type,
                vid,
                pid,
                serial_number,
                product,
            }
        })
        .collect())
}

/// 打开端口并按给定波特率读取，直到读满 `seconds` 秒或达到 `max_bytes`。
///
/// 返回 `(收到的字节数, 十六进制预览)`。**字节计数是 spike 的验收依据**：
/// 「收到数据」不算通过，「收到的字节数与发送端一致」才算。
pub fn read_port(
    port_name: &str,
    baud: u32,
    seconds: u64,
    max_bytes: usize,
) -> Result<(u64, String), String> {
    use std::io::Read;
    use std::time::{Duration, Instant};

    let mut port = serialport::new(port_name, baud)
        .timeout(Duration::from_millis(200))
        .open()
        .map_err(|e| format!("打开 {port_name} 失败: {e}"))?;

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut buf = [0u8; 4096];
    let mut total: u64 = 0;
    let mut preview: Vec<u8> = Vec::new();

    while Instant::now() < deadline && (total as usize) < max_bytes {
        match port.read(&mut buf) {
            Ok(n) => {
                total += n as u64;
                if preview.len() < 256 {
                    preview.extend_from_slice(&buf[..n.min(256 - preview.len())]);
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(format!("读取失败: {e}")),
        }
    }

    let hex = preview
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    Ok((total, hex))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 不依赖硬件的测试：只断言枚举接口本身可用（CI 上也能跑）。
    #[test]
    fn list_ports_is_callable() {
        let r = list_ports();
        assert!(r.is_ok(), "枚举串口不应报错: {:?}", r.err());
    }

    #[test]
    fn port_info_serializes() {
        let p = PortInfo {
            port_name: "COM_TEST".into(),
            port_type: "unknown".into(),
            vid: None,
            pid: None,
            serial_number: None,
            product: None,
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("COM_TEST"));
    }
}
