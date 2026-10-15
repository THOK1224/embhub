//! SPEC 3.1 —— `ByteSource`：字节流来源。
//!
//! 契约要点（以 docs/SPEC.md 3.1 为准）：
//! - `open(config) -> Result<Self>`；`read(&mut buf) -> Result<Block>`（阻塞，
//!   `len == 0` 表示当前无数据）；`close()` 幂等。
//! - 每个返回的块自带到达时刻（进程内单调时钟微秒数），调用方不得改写。
//! - 块边界任意，来源不承诺「一次 read 一帧」。
//! - 复用调用方缓冲区；稳态读取路径不分配堆内存。
//! - 「暂时无数据」（`Ok(Block { len: 0, .. })`）与「致命断开」
//!   （`Err(SourceError::Fatal)`）分开；前者不是错误。
//! - `close` 后再读返回 `Err(SourceError::Closed)`，不允许 panic。

use std::sync::OnceLock;
use std::time::Instant;

/// 单调时钟读数：微秒，起点是进程内某个不对外承诺的时刻。
/// 只有先后顺序与差值有意义，不是墙上时钟，不是 UI 渲染时间。
pub type MonoMicros = u64;

fn mono_now() -> MonoMicros {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// 一次 `read` 返回的块：字节写在调用方缓冲区里，这里只有长度与到达时刻。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    /// 本次写入缓冲区的字节数；0 表示当前无数据（不是错误）。
    pub len: usize,
    /// 到达时刻（单调时钟微秒），在数据源线程打出，下游不得改写。
    pub t_mono: MonoMicros,
}

/// 来源错误模式：把「已关闭」与「致命断开」分开；暂时无数据不是错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    /// `close` 之后再读。
    Closed,
    /// 设备拔出、连接重置等不可恢复错误。
    Fatal(String),
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceError::Closed => write!(f, "来源已关闭"),
            SourceError::Fatal(msg) => write!(f, "来源致命错误: {msg}"),
        }
    }
}

impl std::error::Error for SourceError {}

/// SPEC 3.1 的 seam。两个以上实现（SerialSource / SyntheticSource /
/// store::ReplaySource）证明这是真 seam。
pub trait ByteSource {
    /// 阻塞读取，把字节写进 `buf`，返回长度与到达时刻。
    /// `len == 0` 表示暂时无数据，调用方应继续读或稍后重试。
    fn read(&mut self, buf: &mut [u8]) -> Result<Block, SourceError>;
    /// 幂等关闭。关闭后再读返回 `Err(SourceError::Closed)`，不 panic。
    fn close(&mut self);
}

// ---------------------------------------------------------------------------
// SyntheticSource —— 确定性合成数据源（零硬件自证用）
// ---------------------------------------------------------------------------

/// 合成源配置：给定种子与总量，产出**确定**的字节序列。
/// 同一 `(seed, offset)` 永远得到同一字节，因此可以无损重建用于逐字节比对。
#[derive(Debug, Clone)]
pub struct SyntheticConfig {
    /// PRNG 种子；0 会被替换成固定非零值（xorshift 不允许全零状态）。
    pub seed: u64,
    /// 产出总量（字节）。读完后继续 `read` 返回 `len == 0`。
    pub total_bytes: u64,
    /// 单次 `read` 最多产出的字节数（模拟来源的任意块边界）。
    pub chunk_size: usize,
}

/// 确定性合成数据源：xorshift64* PRNG，稳态读路径零分配。
pub struct SyntheticSource {
    state: u64,
    remaining: u64,
    chunk_size: usize,
    closed: bool,
}

impl SyntheticSource {
    pub fn open(cfg: &SyntheticConfig) -> Result<Self, SourceError> {
        let seed = if cfg.seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            cfg.seed
        };
        Ok(Self {
            state: seed,
            remaining: cfg.total_bytes,
            chunk_size: cfg.chunk_size.max(1),
            closed: false,
        })
    }

    #[inline]
    fn next_byte(&mut self) -> u8 {
        // xorshift64*
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8
    }
}

impl ByteSource for SyntheticSource {
    fn read(&mut self, buf: &mut [u8]) -> Result<Block, SourceError> {
        if self.closed {
            return Err(SourceError::Closed);
        }
        let n = self
            .remaining
            .min(buf.len() as u64)
            .min(self.chunk_size as u64) as usize;
        for b in buf.iter_mut().take(n) {
            *b = self.next_byte();
        }
        self.remaining -= n as u64;
        Ok(Block {
            len: n,
            t_mono: mono_now(),
        })
    }

    fn close(&mut self) {
        self.closed = true;
    }
}

// ---------------------------------------------------------------------------
// SerialSource —— 串口来源（serialport-rs）
// ---------------------------------------------------------------------------

/// 串口来源配置。
#[derive(Debug, Clone)]
pub struct SerialConfig {
    pub port_name: String,
    pub baud: u32,
    /// 读超时（毫秒）；超时被视为「暂时无数据」，不是错误。
    pub read_timeout_ms: u64,
}

/// 串口 `ByteSource`。读超时返回 `len == 0`；其余 I/O 错误视为致命断开。
pub struct SerialSource {
    port: Option<Box<dyn serialport::SerialPort>>,
}

impl SerialSource {
    pub fn open(cfg: &SerialConfig) -> Result<Self, SourceError> {
        let port = serialport::new(&cfg.port_name, cfg.baud)
            .timeout(std::time::Duration::from_millis(cfg.read_timeout_ms))
            .open()
            .map_err(|e| SourceError::Fatal(format!("打开 {} 失败: {e}", cfg.port_name)))?;
        Ok(Self { port: Some(port) })
    }
}

impl ByteSource for SerialSource {
    fn read(&mut self, buf: &mut [u8]) -> Result<Block, SourceError> {
        use std::io::Read;
        let port = match self.port.as_mut() {
            Some(p) => p,
            None => return Err(SourceError::Closed),
        };
        match port.read(buf) {
            Ok(n) => Ok(Block {
                len: n,
                t_mono: mono_now(),
            }),
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(Block {
                len: 0,
                t_mono: mono_now(),
            }),
            Err(e) => Err(SourceError::Fatal(format!("读取失败: {e}"))),
        }
    }

    fn close(&mut self) {
        // 幂等：重复 close 只是空操作。drop 即释放串口句柄。
        self.port = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_is_deterministic_across_instances() {
        let cfg = SyntheticConfig {
            seed: 42,
            total_bytes: 10_000,
            chunk_size: 777,
        };
        let mut a = SyntheticSource::open(&cfg).unwrap();
        let mut b = SyntheticSource::open(&cfg).unwrap();
        let (mut va, mut vb) = (Vec::new(), Vec::new());
        let mut buf = [0u8; 1024];
        loop {
            let ba = a.read(&mut buf).unwrap();
            if ba.len == 0 {
                break;
            }
            va.extend_from_slice(&buf[..ba.len]);
        }
        loop {
            let bb = b.read(&mut buf).unwrap();
            if bb.len == 0 {
                break;
            }
            vb.extend_from_slice(&buf[..bb.len]);
        }
        assert_eq!(va.len(), 10_000);
        assert_eq!(va, vb, "同一种子必须产出同一字节序列");
    }

    #[test]
    fn synthetic_chunk_boundary_is_respected() {
        let cfg = SyntheticConfig {
            seed: 7,
            total_bytes: 100,
            chunk_size: 30,
        };
        let mut s = SyntheticSource::open(&cfg).unwrap();
        let mut buf = [0u8; 4096];
        let b1 = s.read(&mut buf).unwrap();
        assert_eq!(b1.len, 30, "块边界任意但不超过 chunk_size");
        // 到达时刻由来源打出，且随读取推进单调不减。
        let b2 = s.read(&mut buf).unwrap();
        assert!(b2.t_mono >= b1.t_mono, "到达时刻必须单调不减");
    }

    #[test]
    fn synthetic_exhaustion_returns_zero_len_not_error() {
        let cfg = SyntheticConfig {
            seed: 1,
            total_bytes: 5,
            chunk_size: 64,
        };
        let mut s = SyntheticSource::open(&cfg).unwrap();
        let mut buf = [0u8; 64];
        assert_eq!(s.read(&mut buf).unwrap().len, 5);
        assert_eq!(s.read(&mut buf).unwrap().len, 0, "耗尽后返回 0，不是错误");
    }

    #[test]
    fn close_is_idempotent_and_read_after_close_errors() {
        let cfg = SyntheticConfig {
            seed: 1,
            total_bytes: 5,
            chunk_size: 64,
        };
        let mut s = SyntheticSource::open(&cfg).unwrap();
        s.close();
        s.close(); // 幂等
        let mut buf = [0u8; 8];
        assert_eq!(s.read(&mut buf), Err(SourceError::Closed));
    }

    #[test]
    fn serial_open_missing_port_is_error_not_panic() {
        // 不依赖硬件：打开一个几乎不可能存在的端口，必须返回 Err 而不是 panic。
        let cfg = SerialConfig {
            port_name: "EMBHUB_NO_SUCH_PORT_\\0".to_string(),
            baud: 115_200,
            read_timeout_ms: 50,
        };
        assert!(SerialSource::open(&cfg).is_err());
    }

    #[test]
    fn mono_clock_is_monotonic() {
        let a = mono_now();
        let b = mono_now();
        assert!(b >= a);
    }
}
