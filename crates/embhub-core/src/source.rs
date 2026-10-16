//! SPEC 3.1 —— `ByteSource`（读）与 `ByteSink`（写）：字节流的两个方向。
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
//!
//! **发送侧**：SPEC 3.1 只定义了只读的 `ByteSource`，而串口助手必须能发。
//! 因此这里补上 [`ByteSink`]（写 + flush），两个 adapter 各给出一份实现：
//! [`SerialSink`]（`serialport` 的 `try_clone()`，与读句柄**共享同一物理端口**）
//! 与 [`SyntheticSink`]（回环：写进去的字节被同一个 [`SyntheticSource`] 读回，
//! 让「能发」在没有硬件时也能端到端验证）。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

/// 单调时钟读数：微秒，起点是进程内某个不对外承诺的时刻。
/// 只有先后顺序与差值有意义，不是墙上时钟，不是 UI 渲染时间。
pub type MonoMicros = u64;

/// 全项目**唯一**的单调时钟读数入口。
///
/// 所有 `Block::t_mono` 都由它产生；会话创建时刻（[`crate::store::SessionStart`]）
/// 也必须用它取值，否则「由 t_mono 换算墙上时间」会跨基准而失真。
pub fn mono_now() -> MonoMicros {
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

/// 写侧的失败模式：与读侧完全同构（已关闭 / 致命断开），
/// 所以直接复用 [`SourceError`]，不造一个几乎一样的兄弟枚举。
pub type SinkError = SourceError;

/// SPEC 3.1 的**发送侧**：写 + flush。
///
/// 为什么单独一条 trait：发送不是「反过来读」——串口的读句柄与写句柄可以是
/// 两个独立对象（`try_clone()`），失败模式也不同（写失败往往是设备被拔出）。
/// 把它并进 `ByteSource` 会让回放源这种「天然只能读」的实现被迫实现写方法。
pub trait ByteSink {
    /// 把整段字节写出去。与 `std::io::Write::write_all` 同语义：
    /// 要么全部写出去，要么返回错误（不允许「写了一半说成功」）。
    fn write_all(&mut self, buf: &[u8]) -> Result<(), SinkError>;
    /// 把驱动侧缓冲推出去（串口上对应 `SerialPort::flush`）。
    fn flush(&mut self) -> Result<(), SinkError>;
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

/// 回环共享缓冲：发送端塞进来，读取端按顺序取走（FIFO）。
#[derive(Debug, Default)]
struct Loopback {
    /// 待读字节（FIFO）。
    data: VecDeque<u8>,
    /// 写侧累计字节数（tx 计数的底层依据）。
    written: u64,
    /// 读侧累计取走字节数。
    drained: u64,
}

/// 合成源的**发送句柄**：写进去的字节会被同一个回环 [`SyntheticSource`] 读回。
///
/// 它存在的意义是「让发送这件事在没有硬件时也能端到端验证」——
/// 回环路径与串口路径走的是同一个 [`ByteSink`] seam，只有末端实现不同。
#[derive(Debug)]
pub struct SyntheticSink {
    shared: Arc<Mutex<Loopback>>,
}

impl SyntheticSink {
    /// 写侧累计字节数（诊断用；正常路径用 `ByteSink` 即可）。
    pub fn written_bytes(&self) -> u64 {
        self.shared.lock().unwrap().written
    }

    /// 还堆在回环缓冲里、没被读走的字节数。
    pub fn pending_bytes(&self) -> u64 {
        self.shared.lock().unwrap().data.len() as u64
    }
}

impl ByteSink for SyntheticSink {
    fn write_all(&mut self, buf: &[u8]) -> Result<(), SinkError> {
        let mut g = self.shared.lock().unwrap();
        g.data.extend(buf.iter().copied());
        g.written += buf.len() as u64;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), SinkError> {
        // 回环没有驱动缓冲，写进去即「已到达」。
        Ok(())
    }
}

/// 确定性合成数据源：xorshift64* PRNG，稳态读路径零分配。
///
/// 两种模式：
/// - **合成模式**（[`SyntheticSource::open`]）：按 `(seed, offset)` 产出确定字节，
///   用于「注入 == 落盘 == 回放」自证。
/// - **回环模式**（[`SyntheticSource::loopback`]）：不产合成字节，
///   只回放通过 [`SyntheticSource::try_clone_sink`] 写进来的字节；
///   缓冲空时返回 `len == 0`（暂时无数据，不是错误）。
pub struct SyntheticSource {
    state: u64,
    remaining: u64,
    chunk_size: usize,
    closed: bool,
    /// `Some` = 回环模式（此时 `state`/`remaining` 不再使用）。
    loopback: Option<Arc<Mutex<Loopback>>>,
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
            loopback: None,
        })
    }

    /// 回环源（默认块大小 64 KiB）。写进 [`SyntheticSource::try_clone_sink`] 的字节
    /// 会按 FIFO 被本源的 `read` 读回；没数据时返回 `len == 0`。
    pub fn loopback() -> Self {
        Self::loopback_with_chunk(64 * 1024)
    }

    /// 同 [`SyntheticSource::loopback`]，但指定单次 `read` 最多吐多少字节。
    pub fn loopback_with_chunk(chunk_size: usize) -> Self {
        Self {
            state: 0,
            remaining: 0,
            chunk_size: chunk_size.max(1),
            closed: false,
            loopback: Some(Arc::new(Mutex::new(Loopback::default()))),
        }
    }

    /// 取回环写入句柄。**同一个源**的 `read` 会读回写进去的字节。
    /// 非回环源没有写侧，返回带原因的 `Fatal`。
    pub fn try_clone_sink(&self) -> Result<SyntheticSink, SourceError> {
        match &self.loopback {
            Some(shared) => Ok(SyntheticSink {
                shared: Arc::clone(shared),
            }),
            None => Err(SourceError::Fatal(
                "非回环合成源没有写入句柄：请用 SyntheticSource::loopback() 创建".to_string(),
            )),
        }
    }

    /// 是否处于回环模式。
    pub fn is_loopback(&self) -> bool {
        self.loopback.is_some()
    }

    /// 回环缓冲里还没被读走的字节数（非回环源恒为 0）。
    pub fn pending_bytes(&self) -> u64 {
        match &self.loopback {
            Some(shared) => shared.lock().unwrap().data.len() as u64,
            None => 0,
        }
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
        // 回环模式：只回放写进来的字节，顺序即写入顺序。
        if let Some(shared) = &self.loopback {
            let mut g = shared.lock().unwrap();
            let n = buf.len().min(self.chunk_size).min(g.data.len());
            for (slot, byte) in buf.iter_mut().zip(g.data.iter()).take(n) {
                *slot = *byte;
            }
            g.data.drain(..n);
            g.drained += n as u64;
            drop(g);
            return Ok(Block {
                len: n,
                t_mono: mono_now(),
            });
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

// ---------------------------------------------------------------------------
// 串口帧格式参数：数据位 / 校验位 / 停止位 / 流控
// ---------------------------------------------------------------------------

/// 数据位（5/6/7/8）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DataBits {
    Five,
    Six,
    Seven,
    #[default]
    Eight,
}

impl DataBits {
    /// 位数（5..=8）。
    pub fn bits(self) -> u8 {
        match self {
            DataBits::Five => 5,
            DataBits::Six => 6,
            DataBits::Seven => 7,
            DataBits::Eight => 8,
        }
    }

    /// 映射到 serialport 的对应类型。
    pub fn to_serialport(self) -> serialport::DataBits {
        match self {
            DataBits::Five => serialport::DataBits::Five,
            DataBits::Six => serialport::DataBits::Six,
            DataBits::Seven => serialport::DataBits::Seven,
            DataBits::Eight => serialport::DataBits::Eight,
        }
    }

    /// 帧格式摘要里的那一位（如 `8N1` 里的 `8`）。
    pub fn label(self) -> &'static str {
        match self {
            DataBits::Five => "5",
            DataBits::Six => "6",
            DataBits::Seven => "7",
            DataBits::Eight => "8",
        }
    }
}

/// 校验位（None / Odd / Even）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Parity {
    #[default]
    None,
    Odd,
    Even,
}

impl Parity {
    /// 映射到 serialport 的对应类型。
    pub fn to_serialport(self) -> serialport::Parity {
        match self {
            Parity::None => serialport::Parity::None,
            Parity::Odd => serialport::Parity::Odd,
            Parity::Even => serialport::Parity::Even,
        }
    }

    /// 帧格式摘要里的那一位（如 `8N1` 里的 `N`）。
    pub fn label(self) -> &'static str {
        match self {
            Parity::None => "N",
            Parity::Odd => "O",
            Parity::Even => "E",
        }
    }
}

/// 停止位（1 / 1.5 / 2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StopBits {
    #[default]
    One,
    OneAndHalf,
    Two,
}

impl StopBits {
    /// 映射到 serialport 的对应类型。
    ///
    /// **1.5 停止位在本项目用的 serialport 4.x 里没有对应类型**
    /// （`serialport::StopBits` 只有 `One` / `Two`），所以这里明确报错，
    /// 而不是悄悄折算成 1 或 2——静默降级是本项目最不能接受的失败
    /// （docs/PLAN.md 4.3、SPEC 术语表「零丢失」）。
    pub fn to_serialport(self) -> Result<serialport::StopBits, String> {
        match self {
            StopBits::One => Ok(serialport::StopBits::One),
            StopBits::Two => Ok(serialport::StopBits::Two),
            StopBits::OneAndHalf => Err(
                "停止位 1.5 无法应用：底层 serialport 4.x 的 StopBits 只有 1 与 2，\
                 拒绝静默降级（请改用 1 或 2）"
                    .to_string(),
            ),
        }
    }

    /// 帧格式摘要里的那一位（如 `8N1` 里的 `1`）。
    pub fn label(self) -> &'static str {
        match self {
            StopBits::One => "1",
            StopBits::OneAndHalf => "1.5",
            StopBits::Two => "2",
        }
    }
}

/// 流控（None / RTS-CTS / XON-XOFF）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FlowControl {
    #[default]
    None,
    /// RTS/CTS 硬件流控。
    RtsCts,
    /// XON/XOFF 软件流控。
    XonXoff,
}

impl FlowControl {
    /// 映射到 serialport 的对应类型。
    pub fn to_serialport(self) -> serialport::FlowControl {
        match self {
            FlowControl::None => serialport::FlowControl::None,
            FlowControl::RtsCts => serialport::FlowControl::Hardware,
            FlowControl::XonXoff => serialport::FlowControl::Software,
        }
    }

    /// 配置摘要里的名字。
    pub fn label(self) -> &'static str {
        match self {
            FlowControl::None => "无流控",
            FlowControl::RtsCts => "RTS/CTS",
            FlowControl::XonXoff => "XON/XOFF",
        }
    }
}

/// 一次串口会话的**连接配置**：端口 + 波特率 + 读超时。
///
/// 帧格式（数据位/校验/停止位/流控）单独放在 [`SerialFrame`] 里，原因见它的文档。
#[derive(Debug, Clone)]
pub struct SerialConfig {
    pub port_name: String,
    pub baud: u32,
    /// 读超时（毫秒）；超时被视为「暂时无数据」，不是错误。
    pub read_timeout_ms: u64,
}

impl Default for SerialConfig {
    fn default() -> Self {
        Self {
            port_name: String::new(),
            baud: 115_200,
            read_timeout_ms: 200,
        }
    }
}

/// 一帧的格式：数据位 / 校验位 / 停止位 / 流控。默认 `8N1` 无流控。
///
/// 为什么它不是 `SerialConfig` 的四个字段：`SerialConfig` 已经被外部用**完整字面量**
/// 构造（`embhub-cli` 与 `embhub-app` 各一处），而 Rust 没有「字段默认值」
/// （实测 `E0658: default values on fields are experimental`）——往里加字段会让那些
/// 调用方直接编译不过。把帧格式做成**独立且带 `Default`** 的值，
/// 既保住向后兼容，又让四项参数有明确载体：`SerialSource::open_with_frame(cfg, frame)`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SerialFrame {
    /// 数据位（5/6/7/8），默认 8。
    pub data_bits: DataBits,
    /// 校验位（None/Odd/Even），默认 None。
    pub parity: Parity,
    /// 停止位（1/1.5/2），默认 1。注意 1.5 在本项目的 serialport 4.x 下会被明确拒绝。
    pub stop_bits: StopBits,
    /// 流控（None/RTS-CTS/XON-XOFF），默认 None。
    pub flow_control: FlowControl,
}

/// 将真正交给 serialport 的四个参数（纯数据，可单测、可打日志）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SerialFrameSettings {
    pub data_bits: serialport::DataBits,
    pub parity: serialport::Parity,
    pub stop_bits: serialport::StopBits,
    pub flow_control: serialport::FlowControl,
}

impl SerialFrame {
    /// 参数校验。任何非法/无法应用的组合都返回**指出具体冲突**的错误，
    /// 绝不静默降级到 8N1。
    pub fn validate(&self) -> Result<(), String> {
        // ① 平台规则（Win32 DCB 与 16550 UART 的既有约束）先判：这些是「组合冲突」，
        //    错误信息能直接点出是哪两个参数打架。
        //    5 数据位 + 2 停止位、以及 6/7/8 数据位 + 1.5 停止位都是非法组合。
        match (self.data_bits, self.stop_bits) {
            (DataBits::Five, StopBits::Two) => {
                return Err(
                    "非法串口参数组合：5 数据位不支持 2 停止位（Win32 DCB 规则）；\
                     5 数据位只能用 1 或 1.5 停止位"
                        .to_string(),
                )
            }
            (DataBits::Six | DataBits::Seven | DataBits::Eight, StopBits::OneAndHalf) => {
                return Err(format!(
                    "非法串口参数组合：{} 数据位不支持 1.5 停止位（Win32 DCB 规则）；\
                     1.5 停止位只能配 5 数据位",
                    self.data_bits.bits()
                ))
            }
            _ => {}
        }

        // ② 能力缺口：serialport 4.x 没有 1.5 停止位（即使组合本身在硬件上合法）。
        self.stop_bits.to_serialport()?;
        Ok(())
    }

    /// 把四个参数映射成 serialport 类型（纯函数，`open_with_frame` 用的就是它）。
    pub fn to_serialport_settings(&self) -> Result<SerialFrameSettings, String> {
        self.validate()?;
        Ok(SerialFrameSettings {
            data_bits: self.data_bits.to_serialport(),
            parity: self.parity.to_serialport(),
            stop_bits: self.stop_bits.to_serialport()?,
            flow_control: self.flow_control.to_serialport(),
        })
    }

    /// 帧格式摘要，例如 `8N1 无流控`。
    pub fn summary(&self) -> String {
        format!(
            "{}{}{} {}",
            self.data_bits.label(),
            self.parity.label(),
            self.stop_bits.label(),
            self.flow_control.label()
        )
    }
}

/// 人可读的完整配置摘要，例如 `COM7 9600 7E1 RTS/CTS`。
///
/// 失败信息里带上它，用户就能一眼看出「我当时要的是什么参数」。
pub fn describe_serial(cfg: &SerialConfig, frame: &SerialFrame) -> String {
    format!("{} {} {}", cfg.port_name, cfg.baud, frame.summary())
}

impl SerialConfig {
    /// [`describe_serial`] 的方法形式。
    pub fn describe_with(&self, frame: &SerialFrame) -> String {
        describe_serial(self, frame)
    }
}

/// 按连接配置与帧格式组装 serialport builder：**先校验，再把四个参数逐个应用上去**。
///
/// 抽成单一落点是为了让「参数确实被应用」只有一处可审——`SerialSource` 与
/// `SerialSink` 的打开路径共用它，不允许任何一条路径绕过。
fn build_serial_builder(
    cfg: &SerialConfig,
    frame: &SerialFrame,
) -> Result<serialport::SerialPortBuilder, SourceError> {
    let settings = frame.to_serialport_settings().map_err(SourceError::Fatal)?;
    Ok(serialport::new(&cfg.port_name, cfg.baud)
        .timeout(std::time::Duration::from_millis(cfg.read_timeout_ms))
        .data_bits(settings.data_bits)
        .parity(settings.parity)
        .stop_bits(settings.stop_bits)
        .flow_control(settings.flow_control))
}

/// 串口 `ByteSource`。读超时返回 `len == 0`；其余 I/O 错误视为致命断开。
pub struct SerialSource {
    port: Option<Box<dyn serialport::SerialPort>>,
}

impl SerialSource {
    /// 用默认帧格式（8N1 无流控）打开。
    pub fn open(cfg: &SerialConfig) -> Result<Self, SourceError> {
        Self::open_with_frame(cfg, &SerialFrame::default())
    }

    /// 用指定帧格式打开：数据位/校验/停止位/流控都**真正应用**到 serialport builder。
    pub fn open_with_frame(cfg: &SerialConfig, frame: &SerialFrame) -> Result<Self, SourceError> {
        let port = build_serial_builder(cfg, frame)?.open().map_err(|e| {
            SourceError::Fatal(format!("打开 {} 失败: {e}", cfg.describe_with(frame)))
        })?;
        Ok(Self { port: Some(port) })
    }

    /// 取一个**写入句柄**，与读句柄共享同一个物理端口（`serialport::SerialPort::try_clone`）。
    ///
    /// 这正是「串口助手能发」的关键：读线程拿着一个句柄阻塞读，发送方拿另一个句柄写，
    /// 两者指向同一个端口，互不抢所有权。
    pub fn try_clone_sink(&self) -> Result<SerialSink, SourceError> {
        let port = self.port.as_ref().ok_or(SourceError::Closed)?;
        let cloned = port
            .try_clone()
            .map_err(|e| SourceError::Fatal(format!("克隆串口写句柄失败: {e}")))?;
        Ok(SerialSink { port: Some(cloned) })
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

/// 串口**发送句柄**。由 [`SerialSource::try_clone_sink`]（与读句柄共享同一端口）
/// 或 [`SerialSink::open`]（只要写、不需要读）得到。
pub struct SerialSink {
    port: Option<Box<dyn serialport::SerialPort>>,
}

impl SerialSink {
    /// 用默认帧格式（8N1 无流控）打开。
    pub fn open(cfg: &SerialConfig) -> Result<Self, SourceError> {
        Self::open_with_frame(cfg, &SerialFrame::default())
    }

    /// 用指定帧格式打开（与读句柄同一套校验与参数应用）。
    pub fn open_with_frame(cfg: &SerialConfig, frame: &SerialFrame) -> Result<Self, SourceError> {
        let port = build_serial_builder(cfg, frame)?.open().map_err(|e| {
            SourceError::Fatal(format!("打开 {} 失败: {e}", cfg.describe_with(frame)))
        })?;
        Ok(Self { port: Some(port) })
    }

    /// 幂等关闭：之后写返回 `Err(SourceError::Closed)`，不 panic。
    pub fn close(&mut self) {
        self.port = None;
    }
}

impl ByteSink for SerialSink {
    fn write_all(&mut self, buf: &[u8]) -> Result<(), SinkError> {
        use std::io::Write;
        let port = self.port.as_mut().ok_or(SourceError::Closed)?;
        port.write_all(buf)
            .map_err(|e| SourceError::Fatal(format!("写串口失败: {e}")))
    }

    fn flush(&mut self) -> Result<(), SinkError> {
        use std::io::Write;
        let port = self.port.as_mut().ok_or(SourceError::Closed)?;
        port.flush()
            .map_err(|e| SourceError::Fatal(format!("刷串口失败: {e}")))
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

    // ---- 帧格式四件套：映射 / 校验 / 摘要 ----------------------------------

    #[test]
    fn data_bits_map_to_serialport() {
        // 每个合法取值都断言一次（5/6/7/8）。
        assert_eq!(DataBits::Five.to_serialport(), serialport::DataBits::Five);
        assert_eq!(DataBits::Six.to_serialport(), serialport::DataBits::Six);
        assert_eq!(DataBits::Seven.to_serialport(), serialport::DataBits::Seven);
        assert_eq!(DataBits::Eight.to_serialport(), serialport::DataBits::Eight);
        assert_eq!(DataBits::Five.bits(), 5);
        assert_eq!(DataBits::Eight.bits(), 8);
        assert_eq!(DataBits::default(), DataBits::Eight, "默认 8 数据位");
    }

    #[test]
    fn parity_maps_to_serialport() {
        assert_eq!(Parity::None.to_serialport(), serialport::Parity::None);
        assert_eq!(Parity::Odd.to_serialport(), serialport::Parity::Odd);
        assert_eq!(Parity::Even.to_serialport(), serialport::Parity::Even);
        assert_eq!(Parity::default(), Parity::None);
    }

    #[test]
    fn stop_bits_map_to_serialport_and_1_5_is_explicitly_unsupported() {
        assert_eq!(
            StopBits::One.to_serialport().unwrap(),
            serialport::StopBits::One
        );
        assert_eq!(
            StopBits::Two.to_serialport().unwrap(),
            serialport::StopBits::Two
        );
        // serialport 4.x 没有 1.5 停止位：必须明确报错，不许静默折算成 1 或 2。
        let err = StopBits::OneAndHalf.to_serialport().unwrap_err();
        assert!(err.contains("1.5"), "{err}");
        assert!(err.contains("拒绝静默降级"), "{err}");
        assert_eq!(StopBits::default(), StopBits::One);
    }

    #[test]
    fn flow_control_maps_to_serialport() {
        assert_eq!(
            FlowControl::None.to_serialport(),
            serialport::FlowControl::None
        );
        assert_eq!(
            FlowControl::RtsCts.to_serialport(),
            serialport::FlowControl::Hardware,
            "RTS/CTS = 硬件流控"
        );
        assert_eq!(
            FlowControl::XonXoff.to_serialport(),
            serialport::FlowControl::Software,
            "XON/XOFF = 软件流控"
        );
        assert_eq!(FlowControl::default(), FlowControl::None);
    }

    #[test]
    fn default_frame_is_8n1_with_no_flow_control() {
        let frame = SerialFrame::default();
        assert_eq!(frame.data_bits, DataBits::Eight);
        assert_eq!(frame.parity, Parity::None);
        assert_eq!(frame.stop_bits, StopBits::One);
        assert_eq!(frame.flow_control, FlowControl::None);
        assert!(frame.validate().is_ok());
        assert_eq!(frame.summary(), "8N1 无流控");
        // 连接配置的默认值：115200 / 200ms 超时（端口名留空）。
        let cfg = SerialConfig::default();
        assert_eq!(cfg.baud, 115_200);
        assert_eq!(cfg.read_timeout_ms, 200);
        assert_eq!(
            describe_serial(
                &SerialConfig {
                    port_name: "COM_TEST".into(),
                    ..SerialConfig::default()
                },
                &frame
            ),
            "COM_TEST 115200 8N1 无流控"
        );
    }

    #[test]
    fn legal_combinations_are_accepted_and_settings_match() {
        // 8N1 无流控 / 8E1 RTS-CTS / 7O2 XON-XOFF / 5N1（1.5 停止位单独测，见下）
        let cases: Vec<(SerialFrame, SerialFrameSettings)> = vec![
            (
                SerialFrame::default(),
                SerialFrameSettings {
                    data_bits: serialport::DataBits::Eight,
                    parity: serialport::Parity::None,
                    stop_bits: serialport::StopBits::One,
                    flow_control: serialport::FlowControl::None,
                },
            ),
            (
                SerialFrame {
                    data_bits: DataBits::Eight,
                    parity: Parity::Even,
                    stop_bits: StopBits::One,
                    flow_control: FlowControl::RtsCts,
                },
                SerialFrameSettings {
                    data_bits: serialport::DataBits::Eight,
                    parity: serialport::Parity::Even,
                    stop_bits: serialport::StopBits::One,
                    flow_control: serialport::FlowControl::Hardware,
                },
            ),
            (
                SerialFrame {
                    data_bits: DataBits::Seven,
                    parity: Parity::Odd,
                    stop_bits: StopBits::Two,
                    flow_control: FlowControl::XonXoff,
                },
                SerialFrameSettings {
                    data_bits: serialport::DataBits::Seven,
                    parity: serialport::Parity::Odd,
                    stop_bits: serialport::StopBits::Two,
                    flow_control: serialport::FlowControl::Software,
                },
            ),
            (
                SerialFrame {
                    data_bits: DataBits::Five,
                    ..SerialFrame::default()
                },
                SerialFrameSettings {
                    data_bits: serialport::DataBits::Five,
                    parity: serialport::Parity::None,
                    stop_bits: serialport::StopBits::One,
                    flow_control: serialport::FlowControl::None,
                },
            ),
        ];
        for (frame, expected) in cases {
            assert!(frame.validate().is_ok(), "应合法: {}", frame.summary());
            assert_eq!(
                frame.to_serialport_settings().unwrap(),
                expected,
                "映射不一致: {}",
                frame.summary()
            );
        }
    }

    #[test]
    fn illegal_combinations_report_the_specific_conflict() {
        // ① 5 数据位 + 2 停止位（Win32 DCB 规则）
        let five_two = SerialFrame {
            data_bits: DataBits::Five,
            stop_bits: StopBits::Two,
            ..SerialFrame::default()
        };
        let err = five_two.validate().unwrap_err();
        assert!(err.contains("5 数据位不支持 2 停止位"), "{err}");

        // ② 6/7/8 数据位 + 1.5 停止位（Win32 DCB 规则）
        for bits in [DataBits::Six, DataBits::Seven, DataBits::Eight] {
            let frame = SerialFrame {
                data_bits: bits,
                stop_bits: StopBits::OneAndHalf,
                ..SerialFrame::default()
            };
            let err = frame.validate().unwrap_err();
            assert!(err.contains("不支持 1.5 停止位"), "{err}");
        }

        // ③ 5 数据位 + 1.5 停止位在硬件规则上是合法的（16550 就是为它留的），
        //    但 serialport 4.x 表达不了 1.5 → 必须**明确拒绝**而不是折算成 1 或 2。
        let five_one_half = SerialFrame {
            data_bits: DataBits::Five,
            stop_bits: StopBits::OneAndHalf,
            ..SerialFrame::default()
        };
        let err = five_one_half.validate().unwrap_err();
        assert!(err.contains("1.5") && err.contains("拒绝静默降级"), "{err}");
    }

    #[test]
    fn invalid_frame_is_rejected_before_touching_the_port() {
        // 关键：非法参数必须在**碰端口之前**就被拒，并且错误里点名具体冲突，
        // 而不是变成一句「打开 NOPE 失败」（那会让人以为是自己端口写错了）。
        let cfg = SerialConfig {
            port_name: "EMBHUB_NO_SUCH_PORT".to_string(),
            ..SerialConfig::default()
        };
        let bad = SerialFrame {
            data_bits: DataBits::Five,
            stop_bits: StopBits::Two,
            ..SerialFrame::default()
        };
        let err = SerialSource::open_with_frame(&cfg, &bad)
            .err()
            .expect("非法参数必须被拒绝")
            .to_string();
        assert!(err.contains("5 数据位不支持 2 停止位"), "{err}");

        let bad2 = SerialFrame {
            stop_bits: StopBits::OneAndHalf,
            ..SerialFrame::default()
        };
        let err2 = SerialSource::open_with_frame(&cfg, &bad2)
            .err()
            .expect("1.5 停止位必须被拒绝")
            .to_string();
        assert!(err2.contains("1.5"), "{err2}");
        // 发送句柄走同一套校验
        let err3 = SerialSink::open_with_frame(&cfg, &bad2)
            .err()
            .expect("1.5 停止位必须被拒绝（sink）")
            .to_string();
        assert!(err3.contains("1.5"), "{err3}");
        // 默认 open 就是 8N1：参数合法，所以报的应该是「打不开端口」而不是参数错。
        let plain = SerialSource::open(&cfg)
            .err()
            .expect("端口不存在")
            .to_string();
        assert!(plain.contains("8N1"), "错误里要带上帧格式摘要: {plain}");
        assert!(!plain.contains("非法串口参数组合"), "{plain}");
    }

    #[test]
    fn summary_shows_the_frame_format() {
        let frame = SerialFrame {
            data_bits: DataBits::Seven,
            parity: Parity::Even,
            stop_bits: StopBits::One,
            flow_control: FlowControl::RtsCts,
        };
        assert_eq!(frame.summary(), "7E1 RTS/CTS");
        let cfg = SerialConfig {
            port_name: "COM7".into(),
            baud: 9_600,
            ..SerialConfig::default()
        };
        assert_eq!(describe_serial(&cfg, &frame), "COM7 9600 7E1 RTS/CTS");
        assert_eq!(cfg.describe_with(&frame), "COM7 9600 7E1 RTS/CTS");
    }

    #[test]
    fn mono_clock_is_monotonic() {
        let a = mono_now();
        let b = mono_now();
        assert!(b >= a);
    }

    // ---- 发送侧 / 回环 ----------------------------------------------------

    #[test]
    fn loopback_written_bytes_come_back_from_the_same_source() {
        let mut src = SyntheticSource::loopback_with_chunk(3); // 故意用很小的块边界
        let mut sink = src.try_clone_sink().unwrap();
        assert!(src.is_loopback());
        sink.write_all(b"hello embhub").unwrap();
        sink.flush().unwrap();
        assert_eq!(src.pending_bytes(), 12, "\"hello embhub\" 是 12 字节");

        let mut got = Vec::new();
        let mut buf = [0u8; 8];
        loop {
            let b = src.read(&mut buf).unwrap();
            if b.len == 0 {
                break;
            }
            got.extend_from_slice(&buf[..b.len]);
        }
        assert_eq!(got, b"hello embhub", "写进去的字节必须被同一个源读回");
        assert_eq!(src.pending_bytes(), 0);
    }

    #[test]
    fn loopback_preserves_fifo_order_across_writes() {
        let mut src = SyntheticSource::loopback_with_chunk(1024);
        let mut sink = src.try_clone_sink().unwrap();
        sink.write_all(b"first").unwrap();
        sink.write_all(b"-second").unwrap();
        sink.write_all(b"-third").unwrap();
        assert_eq!(sink.written_bytes(), 18);

        let mut buf = [0u8; 64];
        let b = src.read(&mut buf).unwrap();
        assert_eq!(&buf[..b.len], b"first-second-third", "回环必须保持写入顺序");
    }

    #[test]
    fn empty_loopback_returns_zero_len_not_error() {
        let mut src = SyntheticSource::loopback();
        let mut buf = [0u8; 64];
        // 串口没数据时就是这个样子：Ok(len=0)，不是错误、不是 EOF。
        let empty = src.read(&mut buf).unwrap();
        assert_eq!(empty.len, 0, "空回环必须返回 len==0（暂时无数据）");
        let mut sink = src.try_clone_sink().unwrap();
        sink.write_all(b"x").unwrap();
        assert_eq!(src.read(&mut buf).unwrap().len, 1);
        assert_eq!(src.pending_bytes(), 0);
    }

    #[test]
    fn loopback_read_after_close_is_closed_error() {
        let mut src = SyntheticSource::loopback();
        let mut sink = src.try_clone_sink().unwrap();
        sink.write_all(b"data").unwrap();
        src.close();
        src.close(); // 幂等
        let mut buf = [0u8; 8];
        assert_eq!(src.read(&mut buf), Err(SourceError::Closed));
    }

    #[test]
    fn non_loopback_source_has_no_sink() {
        let cfg = SyntheticConfig {
            seed: 1,
            total_bytes: 10,
            chunk_size: 4,
        };
        let src = SyntheticSource::open(&cfg).unwrap();
        assert!(!src.is_loopback());
        let err = src.try_clone_sink().unwrap_err();
        assert!(
            matches!(err, SourceError::Fatal(ref m) if m.contains("loopback")),
            "非回环源必须给出带原因的失败: {err:?}"
        );
    }

    #[test]
    fn serial_sink_open_missing_port_is_error_not_panic() {
        let cfg = SerialConfig {
            port_name: "EMBHUB_NO_SUCH_PORT_\\0".to_string(),
            baud: 115_200,
            read_timeout_ms: 50,
        };
        assert!(SerialSink::open(&cfg).is_err());
        // 关掉的 sink 再写：明确报 Closed，不 panic。
        let mut sink = SerialSink { port: None };
        assert_eq!(sink.write_all(b"x"), Err(SourceError::Closed));
        assert_eq!(sink.flush(), Err(SourceError::Closed));
        sink.close();
    }
}
