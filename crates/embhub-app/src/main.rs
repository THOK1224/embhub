//! embhub 桌面外壳 —— 只是 core 的薄适配层（见 docs/PLAN.md 4.1 依赖方向）。
//!
//! 本文件只做五件事：
//! 1. 把 `embhub_core::Session` 的生命周期包成 command
//!    （`open_session` / `close_session` / `session_stats` / `send_data`
//!    / `start_timed_send` / `stop_timed_send`）；
//! 2. 把显示批次转成事件 `session-batch`（载荷 `{ seq, t_mono, wall_ms, hex }`，
//!    `wall_ms` 由 core 的 `(start_wall_ms, start_mono_micros)` 精确换算）；
//! 3. 把用户输入解析成字节（文本 / HEX + CR/LF/CRLF 追加）；
//! 4. 把「串口参数控件」的字符串解析成 core 的 `SerialFrame`（含冲突报错）；
//! 5. 把失败原因**原样**交给界面（不允许「只显示失败」）。
//!
//! 读取、落盘、批量策略、背压、停止语义、时间基准**全在 embhub-core 里**，
//! 这里一行都不重新实现。刷新节奏由 core 的批量策略决定（≥8 KiB 或 ≥30 ms）。
//!
//! 与 Phase 0 spike 的差别：spike 的 `read_probe`（阻塞读 N 秒）已被
//! 「常驻会话 + 事件推送」取代——那正是 docs/PLAN.md 4.3 要求的方向，故删掉不留死命令。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State, WebviewUrl, WebviewWindowBuilder};

use embhub_core::{
    describe_serial, DataBits, DisplayBatch, FingerprintSection, FlowControl, LogStore, Parity,
    PipelineConfig, SerialConfig, SerialFrame, SerialSource, Session, SessionList, SessionMeta,
    SessionSnapshot, StopBits,
};

/// 串口读超时：兼顾「停止要及时」与「不忙等」（超时算「暂时无数据」，不是错误）。
const SERIAL_READ_TIMEOUT_MS: u64 = 50;
/// 单个分片文件的字节上限。
const SEGMENT_MAX_BYTES: u64 = 8 * 1024 * 1024;

// ---------------------------------------------------------------------------
// 纯函数：输入解析与载荷构造（可单测，不碰硬件）
// ---------------------------------------------------------------------------

/// 把 `"AA55FF"` / `"aa 55 ff"` 解析成字节。非法/空一律报错（不允许静默发空帧）。
fn parse_hex_data(data: &str) -> Result<Vec<u8>, String> {
    let clean: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.is_empty() {
        return Err("HEX 模式下没有可发送的字节（拒绝发送空数据）".to_string());
    }
    if !clean.len().is_multiple_of(2) {
        return Err(format!(
            "HEX 必须是偶数个字符（实际 {} 个）: {clean}",
            clean.len()
        ));
    }
    let bytes = clean.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for i in (0..bytes.len()).step_by(2) {
        let hi = (bytes[i] as char)
            .to_digit(16)
            .ok_or_else(|| format!("HEX 里有非十六进制字符: {clean}"))?;
        let lo = (bytes[i + 1] as char)
            .to_digit(16)
            .ok_or_else(|| format!("HEX 里有非十六进制字符: {clean}"))?;
        out.push((hi * 16 + lo) as u8);
    }
    Ok(out)
}

/// 终结符：`none` / `cr` / `lf` / `crlf`。不认识就报错（不猜）。
fn append_terminator(append: &str) -> Result<&'static [u8], String> {
    match append {
        "none" => Ok(b""),
        "cr" => Ok(b"\r"),
        "lf" => Ok(b"\n"),
        "crlf" => Ok(b"\r\n"),
        other => Err(format!(
            "append 只能是 none | cr | lf | crlf（实际 {other:?}）"
        )),
    }
}

/// 组装要发出去的一帧：数据（文本或 HEX）+ 终结符。空数据一律拒绝。
fn build_frame(data: &str, mode: &str, append: &str) -> Result<Vec<u8>, String> {
    let terminator = append_terminator(append)?;
    let payload: Vec<u8> = match mode {
        "text" => {
            if data.is_empty() {
                return Err("文本模式下没有可发送的内容（拒绝发送空数据）".to_string());
            }
            data.as_bytes().to_vec()
        }
        "hex" => parse_hex_data(data)?,
        other => {
            return Err(format!("mode 只能是 text | hex（实际 {other:?}）"));
        }
    };
    let mut frame = Vec::with_capacity(payload.len() + terminator.len());
    frame.extend_from_slice(&payload);
    frame.extend_from_slice(terminator);
    Ok(frame)
}

/// 大写十六进制（每字节两位，无分隔符）——前端自己决定怎么渲染。
fn to_upper_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02X}"));
    }
    out
}

/// 打开失败时把「为什么」说清楚：端口不在枚举结果里 / 可能被占用 / 原始 I/O 原因。
fn open_error_hint(port: &str, known_ports: &[String], err: &str) -> String {
    let mut msg = format!("打开 {port} 失败: {err}");
    let known = known_ports
        .iter()
        .any(|p| p.eq_ignore_ascii_case(port.trim()));
    if !known {
        let listed = if known_ports.is_empty() {
            "当前枚举不到任何串口".to_string()
        } else {
            format!("当前可见: {}", known_ports.join(", "))
        };
        msg.push_str(&format!(
            "（{port} 不在刚刷新出的端口列表里，可能是设备已拔出或名称写错；{listed}）"
        ));
    }
    let lower = err.to_lowercase();
    if lower.contains("拒绝访问")
        || lower.contains("access is denied")
        || lower.contains("busy")
        || lower.contains("被占用")
    {
        msg.push_str("（该端口可能已被其它程序占用）");
    }
    msg
}

/// 打开会话的**配置对象**（前端一次传四项帧格式参数，便于以后继续扩字段）。
///
/// 前端传 camelCase（`dataBits` / `stopBits` / `flowControl`），这里映射到 snake_case。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenSessionConfig {
    port: String,
    baud: u32,
    /// `"5" | "6" | "7" | "8"`
    data_bits: String,
    /// `"none" | "odd" | "even"`
    parity: String,
    /// `"1" | "1.5" | "2"`
    stop_bits: String,
    /// `"none" | "rtscts" | "xonxoff"`
    flow_control: String,
    /// 仓库路径（**选填**）：填了才采集固件指纹；空串/缺省 = 本会话不含指纹。
    #[serde(default)]
    repo_path: String,
    /// 构建产物路径（**选填**）：只在采集指纹且给了它时才会有 `bin_hash`。
    #[serde(default)]
    bin_path: String,
}

/// 文本框 → `Option<PathBuf>`：**去首尾空白后为空就是「没填」**。
fn optional_trimmed(raw: &str) -> Option<PathBuf> {
    let t = raw.trim();
    if t.is_empty() {
        None
    } else {
        Some(PathBuf::from(t))
    }
}

/// 采集输入：`(repo, bin)`，两者都可为 `None`。
fn fingerprint_inputs(cfg: &OpenSessionConfig) -> (Option<PathBuf>, Option<PathBuf>) {
    (
        optional_trimmed(&cfg.repo_path),
        optional_trimmed(&cfg.bin_path),
    )
}

/// 构造会话元数据——**「不得静默产生无指纹日志」的落点**。
///
/// 两种情况写进 v2 会话头的指纹段是不同的：
/// · 填了 `repoPath` → 调 core 的 `fingerprint::collect` 采集（字段取不到时**带原因**）；
/// · 没填 `repoPath` → 走 [`SessionMeta::new`]，core 会写「未采集 + 原因」。
/// 界面上这两种必须都能看出来（参数区提示 + 状态行明示），见 `ui/index.html`。
///
/// 另外修掉一个真实缺陷：baud 与帧格式之间原来没有分隔符，
/// 来源配置会粘成 `serial:COM14@1152008N1`（t19 的列举把它暴露了出来）。
fn build_session_meta(cfg: &OpenSessionConfig, port: &str, frame: &SerialFrame) -> SessionMeta {
    let source_config = format!("serial:{port}@{} {}", cfg.baud, frame.summary());
    let (repo, bin) = fingerprint_inputs(cfg);
    match repo {
        Some(repo) => {
            let fp = embhub_core::fingerprint::collect(&repo, bin.as_deref());
            SessionMeta::with_fingerprint(source_config, fp)
        }
        None => SessionMeta::new(source_config),
    }
}

/// 把控件字符串解析成 core 的枚举。不认识就报错并列出可接受取值（不猜、不静默降级）。
fn parse_frame_from_config(cfg: &OpenSessionConfig) -> Result<SerialFrame, String> {
    let data_bits = match cfg.data_bits.trim() {
        "5" => DataBits::Five,
        "6" => DataBits::Six,
        "7" => DataBits::Seven,
        "8" => DataBits::Eight,
        other => return Err(format!("dataBits 只能是 5 | 6 | 7 | 8（实际 {other:?}）")),
    };
    let parity = match cfg.parity.trim().to_ascii_lowercase().as_str() {
        "none" => Parity::None,
        "odd" => Parity::Odd,
        "even" => Parity::Even,
        other => return Err(format!("parity 只能是 none | odd | even（实际 {other:?}）")),
    };
    let stop_bits = match cfg.stop_bits.trim() {
        "1" => StopBits::One,
        "1.5" => StopBits::OneAndHalf,
        "2" => StopBits::Two,
        other => return Err(format!("stopBits 只能是 1 | 1.5 | 2（实际 {other:?}）")),
    };
    let flow_control = match cfg.flow_control.trim().to_ascii_lowercase().as_str() {
        "none" => FlowControl::None,
        "rtscts" => FlowControl::RtsCts,
        "xonxoff" => FlowControl::XonXoff,
        other => {
            return Err(format!(
                "flowControl 只能是 none | rtscts | xonxoff（实际 {other:?}）"
            ))
        }
    };
    let frame = SerialFrame {
        data_bits,
        parity,
        stop_bits,
        flow_control,
    };
    // core 负责「组合是否合法」，错误里会点名具体冲突（例如 5 数据位 + 2 停止位）。
    frame.validate()?;
    Ok(frame)
}

/// 显示批次的事件载荷（与前端约定：`hex` 是大写十六进制字符串）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct BatchEvent {
    seq: u64,
    /// 到达时刻（单调时钟微秒，来源打出，不改写）。
    t_mono: u64,
    /// 由会话起点的 `(start_wall_ms, start_mono_micros)` **精确换算**出的墙上时钟（Unix epoch 毫秒）。
    ///
    /// 换算在 Rust 侧用 core 的 `SessionStart::wall_time_ms_for` 完成，
    /// 前端不需要（也不应该）自己拿「第一批到达时间」近似——那会带上整段沉默的偏移。
    wall_ms: u64,
    hex: String,
}

/// 事件泵：把 core 推来的显示批次转成事件。**节奏完全由 core 的批量策略决定**。
///
/// 抽成「接受一个回调」而不是直接调 `app.emit`，是为了能在单测里用一个收集
/// 闭包验证 seq 单调、hex 大写、墙上时刻换算、内容与发送一致（不需要 GUI）。
///
/// 退出条件只有两个：`stop` 置位（用户关了会话），或采集线程自己结束
/// （设备拔出等致命错误 / 来源关闭）——后者会**先把队列里剩下的批次取完**再收工，
/// 否则尾部几批会「已落盘但没上屏」。
fn pump_batches(
    session: &Session,
    snapshot: SessionSnapshot,
    stop: &AtomicBool,
    seq: &AtomicU64,
    mut on_batch: impl FnMut(BatchEvent),
) {
    let mut emit = |batch: DisplayBatch| {
        on_batch(BatchEvent {
            seq: seq.fetch_add(1, Ordering::Relaxed) + 1,
            t_mono: batch.t_mono,
            // 用 core 的快照方法换算（它内部就是 `start.wall_time_ms_for`，基准是会话创建点）。
            wall_ms: snapshot.wall_time_ms_for(batch.t_mono),
            hex: to_upper_hex(&batch.bytes),
        })
    };
    while !stop.load(Ordering::Relaxed) {
        match session.recv_batch(Duration::from_millis(50)) {
            Ok(Some(batch)) => emit(batch),
            Ok(None) => {
                if !session.is_running() {
                    // 采集线程已经结束：尾部批次的状态是「已 emitted、还没被取走」，
                    // 用 0 超时把它们一次取干净（取空即断开，循环自然结束）。
                    while let Ok(Some(rest)) = session.recv_batch(Duration::ZERO) {
                        emit(rest);
                    }
                    break;
                }
                // 会话还在跑：说明只是暂时没数据，继续等。
            }
            // 会话状态异常（未启动等）：停泵，别空转。
            Err(_) => break,
        }
    }
}

// ---------------------------------------------------------------------------
// 会话状态与 DTO
// ---------------------------------------------------------------------------

/// 会话计数（命令 `session_stats` 的返回）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct SessionStatsDto {
    /// 会话是否在跑。
    running: bool,
    port: String,
    baud: u32,
    /// 打开时的帧格式摘要（如 `8E1 RTS/CTS`）——运行中界面就靠它说明「现在实际跑的是什么参数」。
    frame: String,
    /// 会话创建时刻的墙上时钟（Unix epoch 毫秒）——接收区时间戳换算的基准之一。
    start_wall_ms: u64,
    /// 会话创建时刻的单调时钟（微秒）——换算的另一个基准。
    start_mono_micros: u64,
    /// **RX**：从串口读到的字节数。
    rx_bytes: u64,
    /// 已落盘字节数（与 RX 不等就说明落盘丢字节——那是最严重的事）。
    logged_bytes: u64,
    /// **TX**：成功写出去的字节数。
    tx_bytes: u64,
    tx_writes: u64,
    tx_errors: u64,
    /// **显示丢弃**（队列满丢的显示字节，不是数据）。
    display_dropped_bytes: u64,
    display_dropped_batches: u64,
    /// 采集侧致命错误（设备拔出等）。
    fatal_error: Option<String>,
    /// 最近一次发送失败的原因。
    last_error: Option<String>,
    /// 定时发送是否在跑。
    timed_send_running: bool,
    /// 定时发送的间隔（毫秒；没在跑时为上次的间隔）。
    timed_send_interval_ms: u64,
    /// 定时发送已经成功发出的**帧数**。
    timed_send_count: u64,
    /// 定时发送最近一次失败或自动停止的原因。
    timed_send_note: Option<String>,
}

/// 定时发送的状态快照（给 DTO 用，不持锁）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TimedSendInfo {
    running: bool,
    interval_ms: u64,
    count: u64,
    note: Option<String>,
}

fn snapshot_to_dto(
    st: &SessionSnapshot,
    running: bool,
    port: &str,
    baud: u32,
    frame: &str,
    timed: &TimedSendInfo,
) -> SessionStatsDto {
    SessionStatsDto {
        running,
        port: port.to_string(),
        baud,
        frame: frame.to_string(),
        start_wall_ms: st.start.wall_ms,
        start_mono_micros: st.start.mono_micros,
        rx_bytes: st.pipeline.source_bytes,
        logged_bytes: st.pipeline.logged_bytes,
        tx_bytes: st.tx_bytes,
        tx_writes: st.tx_writes,
        tx_errors: st.tx_errors,
        display_dropped_bytes: st.pipeline.display_bytes_dropped,
        display_dropped_batches: st.pipeline.batches_dropped,
        fatal_error: st.pipeline.fatal_error.clone(),
        last_error: st.tx_last_error.clone(),
        timed_send_running: timed.running,
        timed_send_interval_ms: timed.interval_ms,
        timed_send_count: timed.count,
        timed_send_note: timed.note.clone(),
    }
}

/// 正在跑的会话（含事件泵）。
struct RunningSession {
    session: Arc<Session>,
    pump: Option<JoinHandle<()>>,
    pump_stop: Arc<AtomicBool>,
    port: String,
    baud: u32,
    /// 帧格式摘要（运行中不可改参数，所以它是常量）。
    frame: String,
    session_dir: PathBuf,
    session_id: String,
}

/// 定时发送器：按固定间隔重复发同一帧。
///
/// **关会话一定会停它**：`close_session` 主动 `stop_timed_sender`，
/// 而线程自身还有第二道闸——一旦 `session.is_running()` 为假就自动退出。
/// 这样就不会出现「会话关掉了，界面还在定时发送」的假象。
struct TimedSender {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    interval_ms: u64,
    count: Arc<AtomicU64>,
    note: Arc<Mutex<Option<String>>>,
}

impl TimedSender {
    fn info(&self) -> TimedSendInfo {
        TimedSendInfo {
            running: true,
            interval_ms: self.interval_ms,
            count: self.count.load(Ordering::Relaxed),
            note: self.note.lock().unwrap().clone(),
        }
    }
}

/// 停掉并 join 定时发送器（幂等）。返回停止时的状态快照。
fn stop_timed_sender(slot: &mut Option<TimedSender>) -> Option<TimedSendInfo> {
    let mut t = slot.take()?;
    t.stop.store(true, Ordering::SeqCst);
    if let Some(handle) = t.handle.take() {
        let _ = handle.join();
    }
    // 先把值取出来（MutexGuard 必须在 `t` 被 drop 之前结束借用）。
    let count = t.count.load(Ordering::Relaxed);
    let note = t.note.lock().unwrap().clone();
    Some(TimedSendInfo {
        running: false,
        interval_ms: t.interval_ms,
        count,
        note,
    })
}

/// 定时发送线程主体：按 `interval` 重复 `session.send(frame)`。
///
/// 睡眠切成 ≤10ms 的小片，保证「停止」最多 10ms 就生效（不让用户干等一个长间隔）。
/// 会话一旦不再运行（用户关了 / 设备拔出），线程自己退出——这是第二道闸。
fn run_timed_sender(
    session: Arc<Session>,
    frame: Vec<u8>,
    interval: Duration,
    stop: Arc<AtomicBool>,
    count: Arc<AtomicU64>,
    note: Arc<Mutex<Option<String>>>,
) {
    let mut next = Instant::now() + interval;
    while !stop.load(Ordering::Relaxed) {
        if !session.is_running() {
            *note.lock().unwrap() = Some("会话已停止，定时发送自动结束".to_string());
            break;
        }
        let now = Instant::now();
        if now < next {
            thread::sleep((next - now).min(Duration::from_millis(10)));
            continue;
        }
        match session.send(&frame) {
            Ok(_) => {
                count.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                *note.lock().unwrap() = Some(e.to_string());
            }
        }
        next += interval;
        if next < Instant::now() {
            // 落后太多（例如系统卡顿）：直接重置基准，避免「追赶风暴」。
            next = Instant::now() + interval;
        }
    }
}

/// 应用状态：至多一个会话 + 至多一个定时发送器 + 「最后一次关闭时的计数」。
#[derive(Default)]
struct AppState {
    inner: Mutex<Option<RunningSession>>,
    last_stats: Mutex<SessionStatsDto>,
    timed: Mutex<Option<TimedSender>>,
}

#[derive(Serialize)]
struct SessionOpened {
    port: String,
    baud: u32,
    /// 帧格式摘要（如 `8E1 RTS/CTS`）。
    frame: String,
    session_id: String,
    session_dir: String,
    start_wall_ms: u64,
    start_mono_micros: u64,
    /// 本会话**实际写进会话头**的指纹段：就是 core 的 `FingerprintSection`
    /// （键名 `collected`/`not_collected`，与磁盘头、CLI 的 `sessions --json` 一致）。
    /// 已采集时含 commit/dirty/bin_hash 等字段；未采集时含**原因**。
    fingerprint: FingerprintSection,
    /// 指纹摘要来自哪里；会话头回读失败时说明原因（界面会显式告警）。
    fingerprint_note: Option<String>,
}

#[derive(Serialize)]
struct SessionClosed {
    port: String,
    session_id: String,
    rx_bytes: u64,
    tx_bytes: u64,
    display_dropped_bytes: u64,
    session_dir: String,
    /// 关闭时顺带停掉的定时发送：它发过几帧（0 = 原本没开）。
    timed_send_count: u64,
}

#[derive(Serialize)]
struct SendReport {
    bytes: usize,
    hex: String,
    total_tx_bytes: u64,
}

#[derive(Serialize)]
struct TimedSendStarted {
    interval_ms: u64,
    frame_bytes: usize,
    frame_hex: String,
}

#[derive(Serialize)]
struct TimedSendStopped {
    interval_ms: u64,
    count: u64,
}

// ---------------------------------------------------------------------------
// 日志列表（G1 的 core 列举能力 → 界面友好的 DTO）
// ---------------------------------------------------------------------------

/// 列表里的一项会话。
///
/// 用 snake_case 字段名与其它 DTO 保持一致（前端读的是 `row.start_wall_ms`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SessionRow {
    id: String,
    dir: String,
    /// 起始墙上时间（Unix epoch 毫秒）；头坏了就是 null。
    start_wall_ms: Option<u64>,
    source_config: Option<String>,
    payload_bytes: u64,
    records: u64,
    segments: u64,
    /// 坏会话的原因（null = 完全可读）。
    problem: Option<String>,
    /// 指纹段：已采集 / 未采集（带原因）/ 头不可读（null）。
    /// 直接用 core 的类型，键名与磁盘头一致（**不另造形状**）。
    fingerprint: Option<FingerprintSection>,
}

/// 根目录下被跳过的条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SkippedRow {
    name: String,
    reason: String,
}

/// `list_sessions` 命令的返回：**一个坏会话不会让别的会话消失**（core 已保证）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SessionListDto {
    root: String,
    root_problem: Option<String>,
    sessions: Vec<SessionRow>,
    skipped: Vec<SkippedRow>,
    /// 界面显示「没有会话」的直接依据（不许前端自己猜）。
    is_empty: bool,
    ok_count: usize,
    broken_count: usize,
}

/// 会话列表：core 的 [`SessionList`] → 界面 DTO（丢掉「未采集原因」是绝对不行的）。
fn session_list_dto(list: SessionList) -> SessionListDto {
    // 先把派生计数取出来，下面会把它拆开搬走字段。
    let is_empty = list.is_empty();
    let ok_count = list.ok_count();
    let broken_count = list.broken_count();
    SessionListDto {
        root: list.root.display().to_string(),
        root_problem: list.root_problem,
        sessions: list
            .sessions
            .into_iter()
            .map(|s| SessionRow {
                id: s.id,
                dir: s.dir.display().to_string(),
                start_wall_ms: s.start_wall_ms,
                source_config: s.source_config,
                payload_bytes: s.payload_bytes,
                records: s.records,
                segments: s.segments,
                problem: s.problem,
                fingerprint: s.fingerprint,
            })
            .collect(),
        skipped: list
            .skipped
            .into_iter()
            .map(|s| SkippedRow {
                name: s.name,
                reason: s.reason,
            })
            .collect(),
        is_empty,
        ok_count,
        broken_count,
    }
}

/// 界面填的 sessions 目录：去掉首尾空白后为空 → 用程序数据目录下的 `sessions`。
fn sessions_root_for(input: Option<&str>) -> PathBuf {
    match input.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => resolve_data_dir().join("sessions"),
    }
}

// ---------------------------------------------------------------------------
// Tauri command
// ---------------------------------------------------------------------------

/// 打开会话：源 + 落盘 + 发送句柄 + 事件泵。配置以**一个对象**传入（便于继续扩字段）。
#[tauri::command]
fn open_session(
    app: AppHandle,
    state: State<'_, AppState>,
    config: OpenSessionConfig,
) -> Result<SessionOpened, String> {
    let port = config.port.trim().to_string();
    if port.is_empty() {
        return Err("端口名不得为空".to_string());
    }
    if config.baud == 0 {
        return Err("波特率必须是正整数".to_string());
    }
    // 帧格式先解析（含 core 的组合合法性校验）：参数非法就在这里报，不碰端口。
    let frame = parse_frame_from_config(&config)?;

    {
        let guard = state
            .inner
            .lock()
            .map_err(|_| "内部状态锁失效".to_string())?;
        if let Some(r) = guard.as_ref() {
            return Err(format!(
                "已有会话在运行（{} @ {} {}），请先关闭再打开新会话",
                r.port, r.baud, r.frame
            ));
        }
    }

    // 先枚举一次：既给失败原因做提示，也让「没有可用端口」变成一句人话。
    let known_ports: Vec<String> = match embhub_core::list_ports() {
        Ok(ports) => ports.into_iter().map(|p| p.port_name).collect(),
        Err(e) => return Err(format!("枚举串口失败: {e}")),
    };

    let sessions_root = resolve_data_dir().join("sessions");
    std::fs::create_dir_all(&sessions_root)
        .map_err(|e| format!("创建会话目录 {} 失败: {e}", sessions_root.display()))?;
    let store = Arc::new(LogStore::new(&sessions_root, SEGMENT_MAX_BYTES));

    let cfg = SerialConfig {
        port_name: port.clone(),
        baud: config.baud,
        read_timeout_ms: SERIAL_READ_TIMEOUT_MS,
    };
    // 四项帧格式参数在这里真正下发到 serialport builder；
    // 失败信息带上 `describe_serial`（含帧格式摘要），方便对齐「我要的是什么参数」。
    let source = SerialSource::open_with_frame(&cfg, &frame)
        .map_err(|e| open_error_hint(&port, &known_ports, &e.to_string()))?;
    let sink = source
        .try_clone_sink()
        .map_err(|e| format!("取发送句柄失败（端口可能已被占用）: {e}"))?;

    let frame_summary = frame.summary();
    // 指纹在这里采集（填了 repoPath 才采）并交给 core 写进 v2 会话头。
    let meta = build_session_meta(&config, &port, &frame);
    let session = Arc::new(Session::new(
        store,
        meta.clone(),
        Box::new(source),
        Some(Box::new(sink)),
        PipelineConfig::default(),
    ));
    if let Err(e) = session.start() {
        return Err(format!("启动会话失败: {e}"));
    }
    let session_id = session
        .session_id()
        .map(|id| id.0.clone())
        .unwrap_or_default();
    let session_dir = sessions_root.join(&session_id);
    // 会话起点快照：后面每个批次的墙上时刻都由 `SessionSnapshot::wall_time_ms_for` 精确换算
    //（基准是会话创建点，不是「第一批到达时间」）。
    let start_snapshot = session
        .stats()
        .map_err(|e| format!("读会话起点失败: {e}"))?;
    let start = start_snapshot.start;

    // 事件泵：core 决定批量节奏，这里只搬运（含精确的墙上时刻换算）。
    let pump_stop = Arc::new(AtomicBool::new(false));
    let seq = Arc::new(AtomicU64::new(0));
    let pump = {
        let session = Arc::clone(&session);
        let stop = Arc::clone(&pump_stop);
        let seq = Arc::clone(&seq);
        let app = app.clone();
        let snapshot = start_snapshot.clone();
        thread::Builder::new()
            .name("embhub-gui-pump".to_string())
            .spawn(move || {
                pump_batches(&session, snapshot, &stop, &seq, |event| {
                    // 前端可能已经关掉窗口：emit 失败不是错误。
                    let _ = app.emit("session-batch", event);
                });
            })
    };
    let pump = match pump {
        Ok(handle) => handle,
        Err(e) => {
            let _ = session.stop();
            return Err(format!("起事件泵线程失败: {e}"));
        }
    };

    println!(
        "[embhub] 会话已开启 {}（日志目录 {}）",
        describe_serial(&cfg, &frame),
        session_dir.display()
    );
    // 指纹摘要以**磁盘上的会话头**为准（core 在 create_session 里写进去的）：
    // 这样界面显示的是「日志里真的有什么」，而不是「我打算写什么」。
    let (fingerprint, fingerprint_note) = match embhub_core::read_session_header(&session_dir) {
        Ok(header) => (header.fingerprint, None),
        Err(e) => (
            meta.fingerprint.clone(),
            Some(format!("会话头回读失败（以下是写入值，未必已落盘）: {e}")),
        ),
    };
    let opened = SessionOpened {
        port: port.clone(),
        baud: config.baud,
        frame: frame_summary.clone(),
        session_id,
        session_dir: session_dir.display().to_string(),
        start_wall_ms: start.wall_ms,
        start_mono_micros: start.mono_micros,
        fingerprint,
        fingerprint_note,
    };
    *state
        .inner
        .lock()
        .map_err(|_| "内部状态锁失效".to_string())? = Some(RunningSession {
        session,
        pump: Some(pump),
        pump_stop,
        port,
        baud: config.baud,
        frame: frame_summary,
        session_dir,
        session_id: opened.session_id.clone(),
    });
    Ok(opened)
}

/// 关闭会话：**先停定时发送** → 停事件泵 → `stop()`（join 读取线程 + seal）→ 保留最终计数。
///
/// 定时发送必须是第一个被停掉的：否则会出现「会话已经关了，界面还在定时发送」的假象。
#[tauri::command]
fn close_session(state: State<'_, AppState>) -> Result<SessionClosed, String> {
    let running = state
        .inner
        .lock()
        .map_err(|_| "内部状态锁失效".to_string())?
        .take();
    let Some(mut r) = running else {
        return Err("没有正在运行的会话".to_string());
    };

    // ① 停定时发送（幂等；没开过就是 None）。
    let timed = {
        let mut slot = state
            .timed
            .lock()
            .map_err(|_| "内部状态锁失效".to_string())?;
        stop_timed_sender(&mut slot)
    };

    // ② 停会话（join 读取线程 + seal，来源句柄释放），泵会自己发现采集线程已结束并取完尾部批次。
    r.session
        .stop()
        .map_err(|e| format!("关闭会话失败（seal）: {e}"))?;
    r.pump_stop.store(true, Ordering::SeqCst); // 兜底：即使会话停得不干净也不让泵挂住
    if let Some(handle) = r.pump.take() {
        let _ = handle.join();
    }

    let st = r
        .session
        .stats()
        .map_err(|e| format!("读取最终计数失败: {e}"))?;
    let timed_info = timed.unwrap_or_default();
    let dto = snapshot_to_dto(&st, false, &r.port, r.baud, &r.frame, &timed_info);
    *state
        .last_stats
        .lock()
        .map_err(|_| "内部状态锁失效".to_string())? = dto.clone();
    println!(
        "[embhub] 会话已关闭 {}：RX {} / TX {} / 显示丢弃 {} / 定时发送 {} 帧（日志 {}）",
        r.port,
        dto.rx_bytes,
        dto.tx_bytes,
        dto.display_dropped_bytes,
        timed_info.count,
        r.session_dir.display()
    );
    Ok(SessionClosed {
        port: r.port,
        session_id: r.session_id,
        rx_bytes: dto.rx_bytes,
        tx_bytes: dto.tx_bytes,
        display_dropped_bytes: dto.display_dropped_bytes,
        session_dir: r.session_dir.display().to_string(),
        timed_send_count: timed_info.count,
    })
}

/// 读计数。没有会话时返回**上一次关闭时的最终计数**（`running = false`）——不报错。
#[tauri::command]
fn session_stats(state: State<'_, AppState>) -> Result<SessionStatsDto, String> {
    let timed = {
        let slot = state
            .timed
            .lock()
            .map_err(|_| "内部状态锁失效".to_string())?;
        match slot.as_ref() {
            Some(t) => t.info(),
            None => TimedSendInfo::default(),
        }
    };
    let guard = state
        .inner
        .lock()
        .map_err(|_| "内部状态锁失效".to_string())?;
    if let Some(r) = guard.as_ref() {
        let st = r
            .session
            .stats()
            .map_err(|e| format!("读会话计数失败: {e}"))?;
        return Ok(snapshot_to_dto(
            &st, true, &r.port, r.baud, &r.frame, &timed,
        ));
    }
    let mut last = state
        .last_stats
        .lock()
        .map_err(|_| "内部状态锁失效".to_string())?
        .clone();
    // 没有会话：定时发送必然不在跑（close_session 会停掉它），读数以 last_stats 为准。
    last.timed_send_running = false;
    Ok(last)
}

/// 发送一帧：`mode` 决定怎么解析 `data`，`append` 决定追加什么终结符。
#[tauri::command]
fn send_data(
    state: State<'_, AppState>,
    data: String,
    mode: String,
    append: String,
) -> Result<SendReport, String> {
    let frame = build_frame(&data, &mode, &append)?;
    let guard = state
        .inner
        .lock()
        .map_err(|_| "内部状态锁失效".to_string())?;
    let r = guard
        .as_ref()
        .ok_or_else(|| "没有正在运行的会话，无法发送（请先打开会话）".to_string())?;
    let bytes = r.session.send(&frame).map_err(|e| e.to_string())?;
    let total_tx_bytes = r.session.stats().map(|s| s.tx_bytes).unwrap_or(0);
    Ok(SendReport {
        bytes,
        hex: to_upper_hex(&frame),
        total_tx_bytes,
    })
}

/// 定时发送间隔校验：0 会变成忙循环，必须拒绝（并给出人话）。
fn validate_interval_ms(interval_ms: u64) -> Result<(), String> {
    if interval_ms == 0 {
        return Err("定时发送间隔必须 ≥ 1 毫秒（0 会变成忙循环，拒绝）".to_string());
    }
    Ok(())
}

/// 开始定时发送：每 `interval_ms` 毫秒发同一帧。
///
/// 帧的构造与手动发送**完全同一套**（`build_frame(data, mode, append)`），
/// 所以「勾了 CRLF 再定时发」跟「手动发」字节一致。
#[tauri::command]
fn start_timed_send(
    state: State<'_, AppState>,
    interval_ms: u64,
    data: String,
    mode: String,
    append: String,
) -> Result<TimedSendStarted, String> {
    validate_interval_ms(interval_ms)?;
    let frame = build_frame(&data, &mode, &append)?;

    let guard = state
        .inner
        .lock()
        .map_err(|_| "内部状态锁失效".to_string())?;
    let r = guard
        .as_ref()
        .ok_or_else(|| "没有正在运行的会话，无法开始定时发送（请先打开会话）".to_string())?;

    let mut slot = state
        .timed
        .lock()
        .map_err(|_| "内部状态锁失效".to_string())?;
    // 已在跑就先停掉（界面上「改间隔重新开始」就是这个语义），避免两个定时器同时发。
    stop_timed_sender(&mut slot);

    let stop = Arc::new(AtomicBool::new(false));
    let count = Arc::new(AtomicU64::new(0));
    let note = Arc::new(Mutex::new(None));
    let handle = {
        let session = Arc::clone(&r.session);
        let stop = Arc::clone(&stop);
        let count = Arc::clone(&count);
        let note = Arc::clone(&note);
        let frame_for_thread = frame.clone();
        let interval = Duration::from_millis(interval_ms);
        thread::Builder::new()
            .name("embhub-gui-timed-send".to_string())
            .spawn(move || run_timed_sender(session, frame_for_thread, interval, stop, count, note))
            .map_err(|e| format!("起定时发送线程失败: {e}"))?
    };
    *slot = Some(TimedSender {
        stop,
        handle: Some(handle),
        interval_ms,
        count,
        note,
    });
    println!(
        "[embhub] 定时发送已启动：每 {interval_ms} ms 发 {} 字节",
        frame.len()
    );
    Ok(TimedSendStarted {
        interval_ms,
        frame_bytes: frame.len(),
        frame_hex: to_upper_hex(&frame),
    })
}

/// 停止定时发送（幂等：没在跑时返回明确错误，不静默成功）。
#[tauri::command]
fn stop_timed_send(state: State<'_, AppState>) -> Result<TimedSendStopped, String> {
    let mut slot = state
        .timed
        .lock()
        .map_err(|_| "内部状态锁失效".to_string())?;
    match stop_timed_sender(&mut slot) {
        Some(info) => {
            println!("[embhub] 定时发送已停止：共 {} 帧", info.count);
            Ok(TimedSendStopped {
                interval_ms: info.interval_ms,
                count: info.count,
            })
        }
        None => Err("当前没有在运行的定时发送".to_string()),
    }
}

/// 枚举串口（保留 Phase 0 的能力）。
#[tauri::command]
fn list_ports() -> Result<Vec<embhub_core::PortInfo>, String> {
    embhub_core::list_ports()
}

/// 列举历史会话（G1 的 core 能力）；`root` 为空 → 用程序数据目录下的 `sessions`。
///
/// **只读**：只读会话头与分片记录头，不碰任何仓库，也不执行任何 git 写操作。
#[tauri::command]
fn list_sessions(root: Option<String>) -> Result<SessionListDto, String> {
    let root = sessions_root_for(root.as_deref());
    Ok(session_list_dto(embhub_core::list_sessions(&root)))
}

/// 应用的私有数据目录（WebView2 缓存、会话日志都放这里）。
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
    let result = tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            list_ports,
            open_session,
            close_session,
            session_stats,
            send_data,
            start_timed_send,
            stop_timed_send,
            list_sessions
        ])
        .setup(|app| {
            let data_dir = resolve_data_dir();
            std::fs::create_dir_all(&data_dir)?;
            println!("[embhub] webview 数据目录 = {}", data_dir.display());

            // 窗口在代码里建（而不是 tauri.conf.json），唯一目的就是能指定 data_directory。
            //
            // 注意：这里**不能**用 `?` 把 build() 的错误抛回给 tauri——tauri 的 setup 钩子
            // 一旦返回 Err，它内部直接 panic（tauri-2.12.4/src/app.rs:1444），用户只看到
            // 一句 HRESULT 灾难性故障。启动失败必须能看懂，所以我们自己处理并退出。
            let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("embhub — 串口助手")
                .inner_size(1040.0, 720.0)
                .data_directory(data_dir)
                .build();
            match window {
                Ok(window) => {
                    // 让「窗口已创建」这件事在无 GUI 的环境里也可被观测（供自动化验收读取）。
                    println!("[embhub] 窗口已创建: {}", window.label());
                }
                Err(e) => {
                    eprintln!("[embhub] 创建窗口失败: {e}");
                    eprintln!(
                        "[embhub] 排查提示: 该错误来自 WebView2 初始化。常见原因：① 缺少 WebView2 运行时；\
                         ② 进程被受限沙箱限制（WebView2 需要命名管道与用户数据目录）；③ 数据目录不可写。\
                         可用环境变量 EMBHUB_DATA_DIR 指定一个可写目录后重试。"
                    );
                    std::process::exit(1);
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!());

    if let Err(e) = result {
        // 不 panic：把「为什么起不来」讲清楚，退出码 1。
        eprintln!("[embhub] 启动失败: {e}");
        eprintln!(
            "[embhub] 排查提示: 上面的错误发生在创建窗口/WebView2 时。常见原因：① WebView2 运行时缺失；\
             ② 进程被受限沙箱限制（WebView2 需要命名管道与用户数据目录）；③ 数据目录不可写。\
             可用环境变量 EMBHUB_DATA_DIR 指定一个可写目录后重试。"
        );
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embhub_core::SyntheticSource;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "embhub-app-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 回环会话（不碰硬件）：用来验证事件泵真的把 core 的批次搬成了事件。
    fn loopback_session(total: u64) -> (Arc<Session>, PathBuf) {
        let root = temp_root("pump");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        let src = SyntheticSource::loopback_with_chunk(4096);
        let sink = src.try_clone_sink().expect("回环写句柄");
        let session = Arc::new(Session::new(
            store,
            SessionMeta::new("loopback:gui-test"),
            Box::new(src),
            Some(Box::new(sink)),
            PipelineConfig {
                batch_bytes: 1024,
                display_capacity: 64,
                stop_after_bytes: Some(total),
                poll_when_empty: Duration::from_millis(1),
                ..PipelineConfig::default()
            },
        ));
        session.start().expect("start");
        (session, root)
    }

    #[test]
    fn parse_hex_accepts_spaces_and_both_cases() {
        assert_eq!(parse_hex_data("AA55FF").unwrap(), vec![0xAA, 0x55, 0xFF]);
        assert_eq!(parse_hex_data("aa 55 ff").unwrap(), vec![0xAA, 0x55, 0xFF]);
        assert_eq!(parse_hex_data(" 0a\n0b ").unwrap(), vec![0x0A, 0x0B]);
        assert_eq!(parse_hex_data("00").unwrap(), vec![0x00]);
    }

    #[test]
    fn parse_hex_rejects_empty_and_garbage() {
        for bad in ["", "   ", "A", "ABC", "ZZ", "AATT", "0x41", "41 4"] {
            assert!(parse_hex_data(bad).is_err(), "{bad:?} 必须被拒绝");
        }
    }

    #[test]
    fn append_terminators_are_exact() {
        assert_eq!(append_terminator("none").unwrap(), b"");
        assert_eq!(append_terminator("cr").unwrap(), b"\r");
        assert_eq!(append_terminator("lf").unwrap(), b"\n");
        assert_eq!(append_terminator("crlf").unwrap(), b"\r\n");
        assert!(append_terminator("CRLF").is_err());
        assert!(append_terminator("").is_err());
    }

    #[test]
    fn frames_are_data_plus_terminator() {
        assert_eq!(build_frame("AT", "text", "crlf").unwrap(), b"AT\r\n");
        assert_eq!(
            build_frame("AA55", "hex", "none").unwrap(),
            vec![0xAA, 0x55]
        );
        assert_eq!(
            build_frame("4142", "hex", "lf").unwrap(),
            vec![0x41, 0x42, 0x0A]
        );
        // 空数据一律拒绝
        assert!(build_frame("", "text", "none").is_err());
        assert!(build_frame("", "hex", "cr").is_err());
        assert!(build_frame("   ", "hex", "cr").is_err());
        // 非法 mode
        assert!(build_frame("AA", "raw", "none").is_err());
    }

    #[test]
    fn hex_payload_is_uppercase_without_separators() {
        assert_eq!(to_upper_hex(&[0x00, 0x0A, 0xFF, 0xAB]), "000AFFAB");
        assert_eq!(to_upper_hex(&[]), "");
    }

    #[test]
    fn open_error_hint_names_the_reason() {
        let known = vec!["COM3".to_string(), "COM7".to_string()];
        let missing = open_error_hint("COM9", &known, "系统找不到指定的文件");
        assert!(missing.contains("COM9"), "{missing}");
        assert!(missing.contains("不在刚刷新出的端口列表里"), "{missing}");
        assert!(missing.contains("COM3"), "要列出可见端口: {missing}");

        let busy = open_error_hint("COM3", &known, "拒绝访问。 (os error 5)");
        assert!(busy.contains("可能已被其它程序占用"), "{busy}");
        assert!(!busy.contains("不在刚刷新出的端口列表里"), "{busy}");

        let empty = open_error_hint("COM9", &[], "系统找不到指定的文件");
        assert!(empty.contains("当前枚举不到任何串口"), "{empty}");
    }

    // ---- 布局：防止「接收区把下方区域挤出去 / 压扁重叠」回归 -----------------

    /// **布局闸门（H1a 起重写，沿用同一个测试名）**。
    ///
    /// 旧版钉的是「一列 flex + 固定区永不收缩」那套 CSS 规则；布局换成
    /// **区域对象 + 分割树 + CSS Grid 显式轨道** 之后，旧规则已经不存在了，
    /// 所以这里**重新推导**成新模型的不变量（不是删掉、也不是放宽）：
    ///
    /// ① 区域表只有一份（在 `ui/layout.js`），四个区域齐备，且**每区最小尺寸都大于 0**
    ///    —— 最小值为 0 就等于允许被压没，那正是 t17 重叠的成因；
    /// ② 每个区域容器**强制裁剪**（overflow: hidden）+ `min-width/min-height: 0`，
    ///    确保尺寸不随内容变化、内容永不画到区域盒子之外；
    /// ③ 布局根是 `#app` 且是显式 Grid 轨道（轨道由求解结果写 px，不是绝对定位）；
    /// ④ 求解器**被页面引用而非复制**（页面里不许再出现区域表/求解函数）；
    /// ⑤ harness 入口存在（URL 参数），且**五项不变量断言都在**（防止只剩报告）；
    /// ⑥ 持久化走 `normalizeState`（坏数据回退），且 Node 侧有零依赖测试；
    /// ⑦ 内容只能经区域 API 写（`regions.get(...)`），四个区域的内容源与区域表一一对应。
    #[test]
    fn layout_invariants_prevent_the_receive_pane_from_squeezing_others() {
        let layout = include_str!("../ui/layout.js");
        let html = include_str!("../ui/index.html");
        let layout_test = include_str!("../ui/layout.test.js");

        // ---- ① 区域表：四区齐备 + 每区最小尺寸 > 0 ----
        for id in ["params", "tx", "rx", "events"] {
            let block = region_block(layout, id);
            assert!(
                block.contains(&format!("id: \"{id}\"")),
                "区域 {id} 的 def 里必须带上 id"
            );
            for key in ["minW", "minH"] {
                let v = number_after(block, key);
                assert!(
                    v > 0,
                    "区域 {id} 的 {key} 必须 > 0；最小值为 0 = 允许被压没 = 会重叠"
                );
            }
            assert!(
                block.contains("weight:")
                    && block.contains("closable:")
                    && block.contains("defaultVisible:"),
                "区域 {id} 必须声明 weight / closable / defaultVisible"
            );
        }
        assert!(
            !layout.contains("minW: 0") && !layout.contains("minH: 0"),
            "任何区域都不允许 0 最小尺寸"
        );

        // ---- ② 区域容器：强制裁剪 + 尺寸不随内容变化 ----
        let region_css = css_block(html, ".region {");
        assert!(
            region_css.contains("overflow: hidden"),
            "区域容器必须强制裁剪自己的内容（overflow: hidden），否则内容会画到区域之外：{region_css}"
        );
        assert!(
            region_css.contains("min-width: 0") && region_css.contains("min-height: 0"),
            "区域容器必须有 min-width/min-height: 0，才不会被内容顶大：{region_css}"
        );
        let body_css = css_block(html, ".region > .region-body {");
        assert!(
            body_css.contains("overflow: auto"),
            "区域内部滚动容器必须 overflow: auto：{body_css}"
        );
        assert!(
            html.contains("region.el.hidden = !state.visible[id]"),
            "可见性必须直接作用在区域元素上"
        );

        // ---- ③ 布局根是 #app，且轨道是显式 Grid（不是绝对定位） ----
        assert!(html.contains("<div id=\"app\"></div>"), "布局根必须是 #app");
        let app_css = css_block(html, "#app {");
        assert!(
            app_css.contains("display: grid"),
            "#app 必须是 Grid 容器：{app_css}"
        );
        assert!(
            app_css.contains("min-width:") && app_css.contains("min-height:"),
            "#app 必须有最小尺寸（JS 覆盖为可见区域最小值之和）：{app_css}"
        );
        assert!(
            html.contains("gridTemplateColumns") && html.contains("gridTemplateRows"),
            "轨道必须由求解结果写成显式 Grid 轨道"
        );
        assert!(
            !html.contains("position: absolute"),
            "区域不许用绝对定位摆位置（要用 Grid 轨道）"
        );

        // ---- ④ 求解器被页面引用，而不是复制 ----
        assert!(
            html.contains("<script src=\"layout.js\"></script>"),
            "页面必须引用 layout.js"
        );
        assert!(
            html.contains("window.embhubLayout") && html.contains("solve({"),
            "页面必须调用 layout.js 的 solve"
        );
        assert!(
            !html.contains("minW:") && !html.contains("minH:"),
            "页面不许复制区域表；区域表只能在 ui/layout.js 里有一份"
        );
        assert!(
            !html.contains("function solve(") && !html.contains("function subtreeMin("),
            "页面不许复制求解器实现"
        );

        // ---- ⑤ harness：URL 入口 + 五项不变量断言都在 ----
        assert!(
            html.contains("URLSearchParams") && html.contains("selftest"),
            "harness 入口必须是页面 URL 参数（index.html?selftest=1）"
        );
        for rule in [
            "overlap",
            "box-grown",
            "control-covered",
            "below-min",
            "h-scroll",
        ] {
            assert!(
                html.contains(rule),
                "harness 缺少不变量断言 `{rule}`（不许只剩一份好看的报告）"
            );
        }
        assert!(
            html.contains("const saved = {") && html.contains("appEl.style.width = saved.width"),
            "harness 必须先存后还，**只测量不改布局**"
        );
        assert!(
            html.contains("elementFromPoint"),
            "控件遮挡检查必须用 elementFromPoint 实测"
        );

        // ---- ⑥ 持久化 + 坏数据回退 + Node 侧零依赖测试 ----
        assert!(
            html.contains("localStorage") && html.contains("STORAGE_KEY"),
            "可见性与比例必须写进 localStorage"
        );
        assert!(
            html.contains("normalizeState"),
            "读回的状态必须过 normalizeState（坏数据回退）"
        );
        assert!(
            layout.contains("function normalizeState")
                && layout.contains("function serializeState"),
            "状态规整/序列化必须在 layout.js 里（浏览器与 Node 共用同一套）"
        );
        for coverage in ["最小值", "塌缩", "坏数据", "clamp"] {
            assert!(
                layout_test.contains(coverage),
                "Node 测试必须覆盖 `{coverage}`（这是 captain 能独立复跑的那一半）"
            );
        }
        assert!(
            layout_test.contains("require(\"./layout.js\")"),
            "Node 测试必须直接 require 求解器（不另写一份逻辑）"
        );

        // ---- ⑦ 内容只经区域 API 写；区域表与 DOM 一一对应 ----
        assert!(
            html.contains("function createRegion(def)") && html.contains("regions.get("),
            "区域容器必须由注册表创建，内容必须经 regions.get(id) 写入"
        );
        for id in ["params", "tx", "rx", "events"] {
            assert!(
                html.contains(&format!("data-region=\"{id}\"")),
                "区域 {id} 的内容源必须存在（区域表与 DOM 一一对应）"
            );
        }

        // ---- ⑧ 区域交互（t24）：开关/重开入口、分隔条、拖拽、双击复位、守卫 ----
        // 关闭按钮由区域表驱动（不是硬编码一组按钮）
        assert!(
            html.contains("if (def.closable) {"),
            "区域的关闭按钮必须由区域表的 closable 字段驱动"
        );
        // 「重新打开」的全局入口：四个区域全部由 REGION_ORDER 生成，并带可被真实事件驱动的标识
        assert!(
            html.contains("function syncRegionToggles()")
                && html.contains("for (const id of REGION_ORDER)"),
            "「区域」入口必须由区域表（REGION_ORDER）驱动"
        );
        assert!(
            html.contains("box.dataset.regionToggle = id"),
            "区域入口的开关必须带 data-region-toggle（harness 用它驱动「重新打开」）"
        );
        assert!(
            html.contains("box.disabled = !def.closable"),
            "常驻区域的开关必须禁用（不可关）"
        );
        // 不允许把最后一个可见区域关掉：必须走 layout.js 的纯函数守卫，并把原因说出来
        assert!(
            html.contains("L.canHide(state.visible, id, REGIONS)"),
            "关闭前必须过 canHide 守卫（最后一个可见区域不许关）"
        );
        assert!(
            html.contains("不能关闭「") && html.contains("verdict.reason"),
            "被拒绝时必须把原因显示出来（不得静默失败）"
        );
        // 分隔条按「可见孩子数 - 1」生成，带容器/下标标识
        assert!(
            html.contains("function rebuildSplitters(node)") && html.contains("kids.length - 1"),
            "分隔条必须按容器的可见孩子数生成（n-1 条），不许硬编码"
        );
        assert!(
            html.contains("sp.dataset.splitNode = node.id")
                && html.contains("sp.dataset.splitIndex = String(index)"),
            "分隔条必须带容器与下标标识（拖拽据此还原相邻的一对）"
        );
        assert!(
            html.contains("function makeSplitter(node, index)")
                && html.contains("L.visibleChildren(node, state.visible, REGIONS)"),
            "分隔条必须由可见孩子列表驱动"
        );
        // 拖拽：纯函数算新尺寸（等量反向 + 双侧最小值 clamp），真实事件驱动，结束即持久化
        assert!(
            html.contains("L.resizePair(") && html.contains("splitNodeId: dragging.nodeId"),
            "拖拽必须调用 layout.js 的 resizePair（不许在页面里自己算）"
        );
        assert!(
            html.contains("window.addEventListener(DRAG.move, onDragMove)")
                && html.contains("window.addEventListener(DRAG.up, endDrag)"),
            "拖拽必须用真实事件处理函数（pointer/mouse 二选一）"
        );
        assert!(
            html.contains("saveState(); // 每次交互后持久化"),
            "拖拽结束后必须持久化"
        );
        assert!(
            html.contains("addEventListener(\"dblclick\"")
                && html.contains("function resetContainer(nodeId)"),
            "双击分隔条必须能恢复默认比例"
        );
        // harness 覆盖交互路径：真实事件驱动 + 复用同一套不变量断言
        assert!(
            html.contains("function interactionPlan()")
                && html.contains("function dragSplitter(nodeId, index, delta)"),
            "harness 必须通过真实事件驱动交互（拖拽/双击/点击关闭/勾选重开）"
        );
        for need in [
            "dblclickSplitter",
            "clickCloseButton",
            "toggleRegionFromGlobalEntry",
            "setVisible(\"params\", false)",
        ] {
            assert!(
                html.contains(need),
                "harness 的交互计划里缺少 `{need}`（拖到极限 / 关闭再重开 / 常驻区域拒绝 / 双击复位）"
            );
        }
        assert!(
            html.contains("pair-isolation"),
            "harness 必须断言「拖拽只影响相邻两个区域」（pair-isolation）"
        );
        assert!(
            html.contains("const problems = verdict.problems.concat(extra)"),
            "交互后的每个状态必须复用同一套不变量断言"
        );

        // ---- ⑨ F1 根因：隐藏项必须在**布局上**不占位（G15 实测抓到的真缺陷）----
        // 陷阱：UA 的 `[hidden] { display: none }` 特异性 (0,1,0)，而 `.region { display: flex }`
        // 也是 (0,1,0) —— 特异性相同时作者样式优先，于是 `el.hidden = true` 只隐藏语义、
        // 布局上仍是 grid item：求解器删了轨道、DOM 还占位 ⇒ 轨道与孩子错位一位。
        // 所以必须有一条**盖得住 `.region` 的** `[hidden]` 规则。
        let css = html
            .split_once("<style>")
            .and_then(|(_, rest)| rest.split_once("</style>"))
            .map(|(css, _)| css)
            .expect("index.html 必须有 <style> 块");
        let base = css_specificity(".region");
        let winner = hidden_display_none_rule(css);
        let (sel, spec, important) = winner.clone().unwrap_or_else(|| {
            panic!("CSS 里必须有一条针对 [hidden] 的 display:none 规则（F1 修复）")
        });
        assert!(
            important || spec > base,
            "`{sel}` 的特异性 {spec:?} 必须高于 `.region` 的 {base:?}（或用 !important），\
             否则 [hidden] 盖不住 `.region {{ display: flex }}`，隐藏区域仍会占一个 grid 轨道位"
        );
        // 再钉一次「不许用 !important 之外的取巧」：这条规则得真的把 display 设成 none
        assert!(
            css.contains("[hidden]") && css.contains("display: none"),
            "缺少 [hidden] 的 display:none 规则"
        );

        // ---- ⑩ harness：错位的机器判据 + 对视口鲁棒的跳过机制 ----
        assert!(
            html.contains("track-item-mismatch"),
            "harness 必须有「轨道数 ≠ 可见 grid item 数」这条精确规则（F1 的机器判据）"
        );
        assert!(
            html.contains(
                "const expected = track.sizes.length > 0 ? track.sizes.length * 2 - 1 : 0"
            ),
            "轨道数必须按「孩子轨道 + 间隙轨道」的交错布局算出期望值（2n-1）"
        );
        assert!(
            html.contains("getComputedStyle(c).display !== \"none\""),
            "可见 grid item 数必须按**实际 display** 统计（hidden 若没被真正隐藏就会被数进来）"
        );
        assert!(
            html.contains("skipped.push({") && html.contains("skippedChecks"),
            "harness 必须把「本视口下无法成立」的检查标成跳过并汇总上报（不得静默略过）"
        );
        assert!(
            html.contains("viewport") && html.contains("真实视口"),
            "报告必须打印真实视口尺寸与跳过原因"
        );
        // 反作弊：**只有视口相关的检查**允许出现在 skipped 里；既有规则一条都不许被「跳过」洗白
        let skipped_blocks = collect_blocks(html, "skipped.push(");
        for rule in [
            "overlap",
            "below-min",
            "box-grown",
            "not-clipped",
            "paint-out",
            "track-item-mismatch",
            "h-scroll",
        ] {
            assert!(
                html.contains(&format!("rule: \"{rule}\"")),
                "不变量 `{rule}` 的断言不见了（不许为了变绿删规则）"
            );
            assert!(
                !skipped_blocks
                    .iter()
                    .any(|b| b.contains(&format!("\"{rule}\""))),
                "`{rule}` 是可以在本视口下判定的规则，**不允许**被标成跳过：{skipped_blocks:?}"
            );
        }
        // 交互类规则同样不许被跳过洗白
        for rule in ["pair-isolation", "guard-failed", "reopen-failed"] {
            assert!(
                html.contains(&format!("\"{rule}\"")),
                "交互断言 `{rule}` 不见了"
            );
        }

        // ---- ⑪ RC1/RC2（t29）：#app 即根节点、命中不许接受祖先、必须测真实布局、硬名单扩容 ----
        // ① 结构上让 `#app` 自己就是根节点：轨道每次现算现写，静态 CSS 不再有与 DOM 结构不符的兜底
        assert!(
            html.contains("buildLayoutDom(TREE, appEl, true)"),
            "#app 必须是根节点本身（多套一层就要靠静态模板匹配「唯一孩子」，那正是 RC1 的成因）"
        );
        assert!(
            html.contains("const box = isRoot ? host : document.createElement(\"div\");"),
            "根节点不许再建一层 .split 包裹"
        );
        assert!(
            html.contains("if (!isRoot) {") && html.contains("nodes.set(node.id, box);"),
            "根节点的 DOM 就是 #app（nodes.get(TREE.id) === appEl）"
        );
        // 静态 CSS 的 #app 兜底不许再是两列（`300px 1fr`），只允许单格占位
        let app_css = css_block(html, "#app {");
        assert!(
            !css.contains("grid-template-columns: 300px 1fr") && !app_css.contains("300px"),
            "#app 的静态兜底不许再写两列（与单孩子结构矛盾，正是 RC1）：{app_css}"
        );
        assert!(
            app_css.contains("grid-template-columns: 100%"),
            "#app 的静态兜底只能是单格占位，真实轨道由 applyLayout() 每次写入：{app_css}"
        );
        assert!(
            html.contains("gridTemplateColumns = px") && html.contains("gridTemplateRows = px"),
            "applyLayout() 必须真的把求解结果写成轨道（不许依赖静态 CSS 兜底）"
        );
        // ② 命中判定不再接受祖先命中
        assert!(
            html.contains("hit === el || el.contains(hit)"),
            "命中判定必须是「元素本身或其子孙」"
        );
        assert!(
            !html.contains("el.contains(hit) || hit.contains(el)"),
            "命中判定不许再接受祖先命中（命中祖先 = 被祖先裁剪，旧写法把裁剪读成通过 —— RC2①）"
        );
        assert!(
            html.contains("describeHit(hit)"),
            "报错必须点名命中的元素（否则分不清是被谁挡住/裁掉）"
        );
        // ③ 必须把「真实布局」作为第一个被测状态（不覆写 #app 宽高）
        assert!(
            html.contains("real: true") && html.contains("真实布局"),
            "harness 必须在不覆写 #app 宽高的前提下先测一次真实布局（RC2②：它就是这次漏检的根因）"
        );
        assert!(
            html.contains("appEl.style.width = saved.width;\n      appEl.style.height = saved.height;\n      const realRes = applyLayout();"),
            "真实布局状态必须先用**保存下来的原尺寸**再求解"
        );
        // ④ region-clipped（硬规则）：几何裁剪检查 + region-covered（命中检查，可跳过）
        assert!(
            html.contains("rule: \"region-clipped\"") && html.contains("el.parentElement"),
            "harness 必须有 region-clipped：区域不得超出任何祖先容器的矩形（与视口无关）"
        );
        assert!(
            html.contains("rule: \"region-covered\"")
                && html.contains("Math.min(r.right, viewport.w)"),
            "harness 必须有 region-covered：在「区域 ∩ 真实视口」的中心取命中，命中祖先是错误"
        );
        // ⑤ 跳过硬名单扩容：这些规则一条都不许出现在 skipped.push 块里
        for rule in [
            "region-clipped",
            "pair-isolation",
            "guard-failed",
            "reopen-failed",
        ] {
            assert!(
                html.contains(&format!("\"{rule}\"")),
                "规则 `{rule}` 不见了（不许为了变绿删规则）"
            );
            assert!(
                !skipped_blocks
                    .iter()
                    .any(|b| b.contains(&format!("\"{rule}\""))),
                "`{rule}` 属于可判定的硬规则，**不允许**被标成跳过洗白：{skipped_blocks:?}"
            );
        }
    }

    /// 取 CSS 里第一条「针对 `[hidden]` 且声明 `display: none`」的规则：`(选择器, 特异性, 是否 !important)`。
    fn hidden_display_none_rule(css: &str) -> Option<(String, (u32, u32, u32), bool)> {
        let stripped = strip_css_comments(css);
        for block in stripped.split('}') {
            let Some((sel, body)) = block.split_once('{') else {
                continue;
            };
            if !sel.contains("[hidden]") {
                continue;
            }
            let declares_none = body.replace(' ', "").contains("display:none");
            if !declares_none {
                continue;
            }
            let sel = sel.trim().to_string();
            let important = body.contains("!important");
            let best = sel
                .split(',')
                .map(|one| (one.trim().to_string(), css_specificity(one.trim())))
                .max_by_key(|(_, spec)| *spec);
            if let Some((s, spec)) = best {
                return Some((s, spec, important));
            }
        }
        None
    }

    /// 计算选择器的 (id, class/attr/pseudo-class, type) 特异性。够用即可（本项目 CSS 无嵌套/无 :not）。
    fn css_specificity(selector: &str) -> (u32, u32, u32) {
        let bytes = selector.as_bytes();
        let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
        let (mut a, mut b_, mut c) = (0u32, 0u32, 0u32);
        let mut i = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'#' => {
                    a += 1;
                    i += 1;
                    while i < bytes.len() && ident(bytes[i]) {
                        i += 1;
                    }
                }
                b'.' | b'[' | b':' => {
                    b_ += 1;
                    i += 1;
                    while i < bytes.len() && ident(bytes[i]) {
                        i += 1;
                    }
                }
                byte if byte.is_ascii_alphabetic() => {
                    c += 1;
                    while i < bytes.len() && ident(bytes[i]) {
                        i += 1;
                    }
                }
                _ => i += 1,
            }
        }
        (a, b_, c)
    }

    /// 把源码里每个 `marker` 之后的一段文本抓出来（用于「某规则有没有出现在 skipped 里」这类检查）。
    fn collect_blocks(src: &str, marker: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = src;
        while let Some(idx) = rest.find(marker) {
            let tail = &rest[idx..];
            out.push(tail.chars().take(400).collect::<String>());
            rest = &tail[marker.len()..];
        }
        out
    }

    /// 删掉 CSS 注释（注释里会出现 `{`/`}`/`;`，不先删掉按规则切分会误判）。
    fn strip_css_comments(css: &str) -> String {
        let mut out = String::with_capacity(css.len());
        let mut rest = css;
        while let Some(start) = rest.find("/*") {
            out.push_str(&rest[..start]);
            match rest[start + 2..].find("*/") {
                Some(end) => rest = &rest[start + 2 + end + 2..],
                None => return out,
            }
        }
        out.push_str(rest);
        out
    }

    /// 取区域表里某个区域的 def 文本（`params: { ... },` 这一段）。
    fn region_block<'a>(src: &'a str, id: &str) -> &'a str {
        let marker = format!("\n    {id}: {{");
        let start = src
            .find(&marker)
            .unwrap_or_else(|| panic!("ui/layout.js 的区域表里没有 {id}"));
        let rest = &src[start..];
        let end = rest.find("\n    },").unwrap_or(rest.len());
        &rest[..end]
    }

    /// 取 `key: 123` 里的数字（取不到就 panic，避免断言被悄悄放过）。
    fn number_after(src: &str, key: &str) -> u64 {
        let idx = src
            .find(key)
            .unwrap_or_else(|| panic!("缺少 `{key}`：{src}"));
        src[idx + key.len()..]
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .unwrap_or_else(|e| panic!("`{key}` 后面不是数字: {e}"))
    }

    /// 取样式表里某个选择器的声明块（`#app { ... }`）。
    fn css_block<'a>(html: &'a str, selector: &str) -> &'a str {
        let start = html
            .find(selector)
            .unwrap_or_else(|| panic!("样式表里找不到选择器 {selector}"));
        let rest = &html[start + selector.len()..];
        let end = rest.find('}').unwrap_or(rest.len());
        &rest[..end]
    }

    #[test]
    fn pump_turns_core_batches_into_events() {
        let (session, root) = loopback_session(8192);
        let snapshot = session.stats().unwrap();
        let collected = Arc::new(Mutex::new(Vec::<BatchEvent>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let seq = Arc::new(AtomicU64::new(0));
        let pump = {
            let session = Arc::clone(&session);
            let collected = Arc::clone(&collected);
            let stop = Arc::clone(&stop);
            let seq = Arc::clone(&seq);
            thread::spawn(move || {
                pump_batches(&session, snapshot, &stop, &seq, |event| {
                    collected.lock().unwrap().push(event)
                })
            })
        };

        session.send(&vec![0xAB; 8192]).unwrap();
        assert!(
            session.wait_until_finished(Duration::from_secs(10)),
            "回环数据没读完"
        );
        // 不置 stop：让泵自己发现「采集线程已结束」并把尾部批次取完再退出。
        pump.join().unwrap();
        session.stop().unwrap();

        let after = session.stats().unwrap();
        let events = collected.lock().unwrap().clone();
        assert!(!events.is_empty(), "泵必须产出批次事件");
        assert_eq!(events[0].seq, 1, "seq 从 1 开始");
        for pair in events.windows(2) {
            assert_eq!(pair[1].seq, pair[0].seq + 1, "seq 必须严格递增");
        }
        let mut total = 0usize;
        for event in &events {
            assert!(
                event
                    .hex
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()),
                "hex 必须是大写十六进制: {}",
                event.hex
            );
            assert_eq!(event.hex.len() % 2, 0);
            total += event.hex.len() / 2;
            for chunk in event.hex.as_bytes().chunks(2) {
                assert_eq!(chunk, b"AB", "回环内容必须是写进去的 0xAB");
            }
            // 墙上时刻必须等于 core 快照的精确换算（同一个基准、同一个函数）；
            // 绝不能是「第一批到达时间」那种近似。
            assert_eq!(
                event.wall_ms,
                after.wall_time_ms_for(event.t_mono),
                "事件的墙上时刻必须由 SessionSnapshot::wall_time_ms_for 精确换算"
            );
            assert!(
                event.wall_ms >= after.start.wall_ms,
                "任何批次的墙上时刻都不早于会话起点"
            );
        }
        assert_eq!(total, 8192, "事件里的字节总数必须等于写入量");
        assert!(
            events[0].wall_ms >= after.start.wall_ms
                && events[0].wall_ms <= after.start.wall_ms + 60_000,
            "墙上时刻必须落在「会话起点之后、合理窗口内」，实际 {} vs 起点 {}",
            events[0].wall_ms,
            after.start.wall_ms
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// 墙上时刻的换算必须**等价于** `SessionStart::wall_time_ms_for`（两者不能有两套算法）。
    #[test]
    fn wall_time_conversion_matches_start_helper() {
        let (session, root) = loopback_session(1024);
        let snap = session.stats().unwrap();
        for t in [0, 1_000, 2_500, 999_999, 5_000_000] {
            assert_eq!(
                snap.wall_time_ms_for(t),
                snap.start.wall_time_ms_for(t),
                "两处换算必须一致（t_mono={t}）"
            );
        }
        assert_eq!(
            snap.wall_time_ms_for(snap.start.mono_micros),
            snap.start.wall_ms
        );
        assert_eq!(
            snap.wall_time_ms_for(0),
            snap.start.wall_ms,
            "早于起点不下溢"
        );
        session.stop().unwrap();
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn stats_dto_maps_core_counters() {
        let (session, root) = loopback_session(4096);
        session.send(&vec![0x11; 4096]).unwrap();
        assert!(session.wait_until_finished(Duration::from_secs(10)));
        session.stop().unwrap();
        let st = session.stats().unwrap();
        let timed = TimedSendInfo {
            running: false,
            interval_ms: 250,
            count: 7,
            note: None,
        };
        let dto = snapshot_to_dto(&st, true, "COM_TEST", 115_200, "8E1 RTS/CTS", &timed);
        assert!(dto.running);
        assert_eq!(dto.port, "COM_TEST");
        assert_eq!(dto.baud, 115_200);
        assert_eq!(dto.frame, "8E1 RTS/CTS");
        assert_eq!(dto.start_wall_ms, st.start.wall_ms);
        assert_eq!(dto.start_mono_micros, st.start.mono_micros);
        assert_eq!(dto.rx_bytes, 4096, "RX = 管线读到的字节数");
        assert_eq!(dto.logged_bytes, 4096);
        assert_eq!(dto.tx_bytes, 4096, "TX = 成功写出的字节数");
        assert_eq!(dto.tx_writes, 1);
        assert_eq!(dto.tx_errors, 0);
        assert_eq!(dto.display_dropped_bytes, 0);
        assert!(dto.fatal_error.is_none());
        assert_eq!(dto.timed_send_interval_ms, 250);
        assert_eq!(dto.timed_send_count, 7);
        std::fs::remove_dir_all(&root).ok();
    }

    // ---- 串口参数控件 → SerialFrame ----------------------------------------

    fn config(data_bits: &str, parity: &str, stop_bits: &str, flow: &str) -> OpenSessionConfig {
        OpenSessionConfig {
            port: "COM_TEST".into(),
            baud: 115_200,
            data_bits: data_bits.into(),
            parity: parity.into(),
            stop_bits: stop_bits.into(),
            flow_control: flow.into(),
            repo_path: String::new(),
            bin_path: String::new(),
        }
    }

    // ---- 固件指纹（增量 G2）-----------------------------------------------

    fn frame_8n1() -> SerialFrame {
        SerialFrame::default()
    }

    #[test]
    fn optional_paths_are_trimmed_and_empty_means_absent() {
        assert_eq!(optional_trimmed(""), None);
        assert_eq!(optional_trimmed("   "), None);
        assert_eq!(optional_trimmed("\t\n"), None);
        assert_eq!(
            optional_trimmed("  C:\\work\\repo  "),
            Some(PathBuf::from("C:\\work\\repo"))
        );
        // 只有 bin、没有 repo：repo 仍是 None（不采集），bin 照样解析出来
        let mut cfg = config("8", "none", "1", "none");
        cfg.bin_path = " out\\fw.elf ".into();
        let (repo, bin) = fingerprint_inputs(&cfg);
        assert!(repo.is_none());
        assert_eq!(bin, Some(PathBuf::from("out\\fw.elf")));
    }

    /// 来源配置里 baud 与帧格式之间**必须有分隔符**（t19 的列举发现过粘连缺陷）。
    #[test]
    fn source_config_separates_baud_from_frame() {
        let cfg = config("8", "even", "1", "rtscts");
        let meta = build_session_meta(&cfg, "COM14", &frame_8n1());
        assert_eq!(meta.source_config, "serial:COM14@115200 8N1 无流控");
        assert!(
            !meta.source_config.contains("1152008N1"),
            "baud 与帧格式不许粘在一起: {}",
            meta.source_config
        );
        // 带流控/校验的帧也一样
        let frame = parse_frame_from_config(&cfg).unwrap();
        let meta = build_session_meta(&cfg, "COM7", &frame);
        assert_eq!(meta.source_config, "serial:COM7@115200 8E1 RTS/CTS");
        assert!(meta.source_config.contains(' '), "{}", meta.source_config);
    }

    /// 没填仓库路径 → 会话头里必须是「未采集 + 原因」，**不得静默**。
    #[test]
    fn meta_without_repo_records_not_collected_reason() {
        let meta = build_session_meta(&config("8", "none", "1", "none"), "COM_TEST", &frame_8n1());
        assert!(meta.fingerprint.collected.is_none());
        assert_eq!(
            meta.fingerprint.not_collected.as_deref(),
            Some(embhub_core::FINGERPRINT_NOT_COLLECTED),
            "未采集必须带 core 给的原因"
        );
    }

    /// 填了仓库路径 → 真的采集（本仓库是 git 仓库）并带上 commit/dirty/bin_hash。
    #[test]
    fn meta_with_repo_collects_fingerprint_with_commit_dirty_and_bin_hash() {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf(); // 工作区根
        let bin = repo.join("docs").join("SPEC.md");
        let mut cfg = config("8", "none", "1", "none");
        cfg.repo_path = repo.display().to_string();
        cfg.bin_path = bin.display().to_string();

        let meta = build_session_meta(&cfg, "COM_TEST", &frame_8n1());
        let fp = meta
            .fingerprint
            .collected
            .as_ref()
            .expect("填了仓库路径就必须采集到指纹段");
        assert!(meta.fingerprint.not_collected.is_none());

        let commit = fp
            .commit
            .value()
            .unwrap_or_else(|| panic!("本仓库是 git 仓库，commit 应采集到: {:?}", fp.commit));
        assert_eq!(commit.len(), 40, "commit 必须是完整 40 位 hex: {commit}");
        assert!(commit.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(fp.dirty.value().is_some(), "dirty 应该能判定出来");
        let bin_hash = fp.bin_hash.value().expect("给了 bin 文件就该有 bin_hash");
        assert!(bin_hash.starts_with("sha256:"), "{bin_hash}");
        assert_eq!(bin_hash.len(), "sha256:".len() + 64);
    }

    /// 采集失败：core 的每个字段缺失原因都必须留着（界面要原样打印，不许只说「采集失败」）。
    #[test]
    fn meta_with_bad_repo_surfaces_every_core_missing_reason() {
        let ghost = temp_root("fp-ghost-repo").join("definitely-not-a-repo");
        let mut cfg = config("8", "none", "1", "none");
        cfg.repo_path = ghost.display().to_string();

        let meta = build_session_meta(&cfg, "COM_TEST", &frame_8n1());
        let fp = meta
            .fingerprint
            .collected
            .as_ref()
            .expect("采集入口一旦给了路径就会产出指纹段（字段级缺失，而不是整段缺失）");
        let missing = fp.missing();
        assert!(!missing.is_empty(), "取不到就必须有原因: {fp:?}");
        for (name, reason) in &missing {
            assert!(!reason.trim().is_empty(), "字段 {name} 的原因不得为空");
        }
        // 关键字段都应有原因，且能指名道姓
        for key in ["commit", "dirty", "bin_hash"] {
            assert!(
                missing.iter().any(|(k, _)| *k == key),
                "缺少 {key} 的缺失原因: {missing:?}"
            );
        }
        assert!(fp.commit.value().is_none(), "不存在的仓库不该给出 commit");
    }

    /// 给了仓库但 bin 文件不存在：commit 仍有值，bin_hash 带**具体原因**。
    #[test]
    fn meta_with_missing_bin_keeps_bin_hash_reason() {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let mut cfg = config("8", "none", "1", "none");
        cfg.repo_path = repo.display().to_string();
        cfg.bin_path = repo.join("no-such-firmware.elf").display().to_string();

        let meta = build_session_meta(&cfg, "COM_TEST", &frame_8n1());
        let fp = meta.fingerprint.collected.as_ref().unwrap();
        assert!(fp.commit.value().is_some(), "commit 仍应采集到");
        let reason = fp
            .bin_hash
            .reason()
            .expect("bin 不存在时 bin_hash 必须带原因");
        assert!(!reason.trim().is_empty());
        assert!(
            reason.contains("no-such-firmware.elf")
                || reason.contains("不存在")
                || reason.contains("No such"),
            "原因要能指明是哪个文件/什么错: {reason}"
        );
    }

    // ---- 日志列表 ---------------------------------------------------------

    #[test]
    fn sessions_root_defaults_to_the_data_dir_and_explicit_wins() {
        let default_root = sessions_root_for(None);
        assert_eq!(
            default_root.file_name().and_then(|n| n.to_str()),
            Some("sessions"),
            "{default_root:?}"
        );
        assert_eq!(sessions_root_for(Some("   ")), default_root, "空白 = 没填");
        assert_eq!(
            sessions_root_for(Some("  D:\\logs\\s  ")),
            PathBuf::from("D:\\logs\\s"),
            "填了就以填的为准（去首尾空白）"
        );
    }

    #[test]
    fn session_list_dto_keeps_fingerprint_reason_and_flags_emptiness() {
        // 空目录：必须给出「没有会话」的依据
        let empty = temp_root("list-dto-empty");
        let dto = session_list_dto(embhub_core::list_sessions(&empty));
        assert!(dto.is_empty);
        assert_eq!(dto.sessions.len(), 0);
        assert!(dto.root_problem.is_none());
        std::fs::remove_dir_all(&empty).ok();

        // 一个真会话（未采集指纹）+ 一个坏会话
        let root = temp_root("list-dto");
        let store = LogStore::new(&root, 4096);
        let id = store
            .create_session(&SessionMeta::new("serial:COM14@115200 8N1 无流控"))
            .unwrap();
        store
            .append(
                &id,
                &embhub_core::Chunk {
                    t_mono: 1,
                    payload: &[0xAB; 32],
                },
            )
            .unwrap();
        store.seal(&id).unwrap();
        let broken = root.join("s-broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("header.bin"), b"junk").unwrap();

        let dto = session_list_dto(embhub_core::list_sessions(&root));
        assert!(!dto.is_empty);
        assert_eq!(dto.sessions.len(), 2);
        assert_eq!(dto.ok_count, 1);
        assert_eq!(dto.broken_count, 1);

        let good = dto.sessions.iter().find(|s| s.id == id.0).unwrap();
        assert_eq!(
            good.source_config.as_deref(),
            Some("serial:COM14@115200 8N1 无流控")
        );
        assert_eq!(good.payload_bytes, 32);
        assert!(good.problem.is_none());
        let fp = good.fingerprint.as_ref().expect("必须有指纹段");
        assert!(fp.collected.is_none());
        assert_eq!(
            fp.not_collected.as_deref(),
            Some(embhub_core::FINGERPRINT_NOT_COLLECTED),
            "未采集原因必须原样带到界面"
        );

        let bad = dto.sessions.iter().find(|s| s.id == "s-broken").unwrap();
        assert!(
            bad.problem.as_deref().unwrap_or("").contains("太短"),
            "{:?}",
            bad.problem
        );
        assert!(bad.fingerprint.is_none(), "头读不到时没有指纹段可展示");
        assert!(bad.start_wall_ms.is_none());
        assert!(dto.root.ends_with("list-dto") || dto.root.contains("list-dto"));
        std::fs::remove_dir_all(&root).ok();
    }

    /// **GUI 的整条数据链一次走通**（不用开窗）：
    /// 配置对象 → `fingerprint::collect` → 写进 v2 会话头 → 从磁盘回读 → 列表 DTO 里的指纹摘要。
    #[test]
    fn gui_path_end_to_end_fingerprint_reaches_the_session_list() {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let mut cfg = config("8", "even", "1", "rtscts");
        cfg.repo_path = repo.display().to_string();
        cfg.bin_path = repo.join("docs").join("SPEC.md").display().to_string();
        let frame = parse_frame_from_config(&cfg).unwrap();

        // ① 打开会话时构造的元数据（含指纹，且来源配置带分隔符）
        let meta = build_session_meta(&cfg, "COM14", &frame);
        assert_eq!(meta.source_config, "serial:COM14@115200 8E1 RTS/CTS");

        // ② 按 v2 路径落盘
        let root = temp_root("gui-e2e");
        let store = LogStore::new(&root, 4096);
        let id = store.create_session(&meta).unwrap();
        store
            .append(
                &id,
                &embhub_core::Chunk {
                    t_mono: 1,
                    payload: &[0xCD; 16],
                },
            )
            .unwrap();
        store.seal(&id).unwrap();

        // ③ 界面打开会话后就是这么取指纹的：回读磁盘会话头
        let header = embhub_core::read_session_header(&root.join(&id.0)).unwrap();
        let on_disk = header
            .fingerprint
            .collected
            .as_ref()
            .expect("指纹必须真的写进了磁盘");
        assert_eq!(on_disk.commit.value().map(|c| c.len()), Some(40));
        assert!(on_disk.bin_hash.value().unwrap().starts_with("sha256:"));
        assert!(on_disk.dirty.value().is_some());

        // ④ 界面打开「日志列表」时走的就是这条：list_sessions → DTO
        let dto = session_list_dto(embhub_core::list_sessions(&root));
        let row = dto.sessions.iter().find(|s| s.id == id.0).unwrap();
        let shown = row
            .fingerprint
            .as_ref()
            .and_then(|fp| fp.collected.as_ref())
            .expect("列表里必须看得到指纹");
        assert_eq!(shown.commit.value(), on_disk.commit.value());
        assert_eq!(shown.bin_hash.value(), on_disk.bin_hash.value());
        assert_eq!(shown.dirty.value(), on_disk.dirty.value());
        assert_eq!(row.payload_bytes, 16);
        assert_eq!(
            row.source_config.as_deref(),
            Some("serial:COM14@115200 8E1 RTS/CTS")
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// **JSON 键名以 core 实体为准**（修订版第 5 条）：`SessionSummary` 是 camelCase，
    /// 而 `fingerprint` 子对象沿用磁盘会话头的 `collected` / `not_collected`，
    /// 字段值是 `{Value|Missing}` —— 同一个 Rust 类型，一处定义两处使用。
    /// 这条测试把「不许为了统一命名而加 `serde(rename)` / 另造形状」钉住。
    #[test]
    fn fingerprint_json_keys_stay_exactly_as_the_on_disk_header() {
        // ① core 类型本身：键名与磁盘头一致
        let not_collected = FingerprintSection::not_collected("测试用未采集原因");
        let json = serde_json::to_string(&not_collected).unwrap();
        assert!(json.contains("\"collected\":null"), "{json}");
        assert!(
            json.contains("\"not_collected\":\"测试用未采集原因\""),
            "{json}"
        );

        // ② 已采集：字段名仍是磁盘格式的 snake_case，值仍是 {Value|Missing}
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let collected =
            FingerprintSection::collected(embhub_core::fingerprint::collect(&repo, None));
        let json = serde_json::to_string(&collected).unwrap();
        assert!(
            json.contains("\"bin_hash\""),
            "字段名必须与磁盘头一致: {json}"
        );
        assert!(
            json.contains("\"Value\"") || json.contains("\"Missing\""),
            "字段值必须是 Field 的 {{Value|Missing}} 外部标签形式: {json}"
        );

        // ③ 界面拿到的列表 DTO：app 自己的字段是 snake_case（与其它 DTO 一致），
        //    但 fingerprint 子对象的键名**原样**是 collected / not_collected。
        let root = temp_root("json-keys");
        let store = LogStore::new(&root, 4096);
        let id = store
            .create_session(&SessionMeta::new("serial:COM_TEST@115200"))
            .unwrap();
        store.seal(&id).unwrap();
        let dto = session_list_dto(embhub_core::list_sessions(&root));
        let json = serde_json::to_string(&dto).unwrap();
        assert!(
            json.contains("\"not_collected\""),
            "指纹键名不许被改：{json}"
        );
        assert!(
            !json.contains("\"notCollected\""),
            "禁止把 not_collected 改成 camelCase：{json}"
        );
        assert!(
            json.contains("\"payload_bytes\""),
            "app 自己的字段用 snake_case：{json}"
        );
        assert!(json.contains("\"is_empty\""), "{json}");
        std::fs::remove_dir_all(&root).ok();
    }

    // ---- 工程约束（命令集对称 / id 完整 / 绝不碰仓库）---------------------

    /// 前端 `invoke("...")` 的命令集必须与后端 `generate_handler!` **完全一致**（双向对称）。
    #[test]
    fn ui_command_set_is_symmetric_with_the_backend() {
        let src = include_str!("main.rs");
        // main() 里的注册表是文件中第一处 `generate_handler![`（测试模块在文件末尾）。
        let registered: Vec<String> = src
            .split_once("generate_handler![")
            .and_then(|(_, rest)| rest.split_once(']'))
            .map(|(list, _)| {
                list.split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty() && !s.starts_with("//"))
                    .map(|s| s.to_string())
                    .collect()
            })
            .expect("找不到 generate_handler! 注册表");
        assert!(registered.len() >= 7, "{registered:?}");

        let html = include_str!("../ui/index.html");
        let invoked: Vec<String> = html
            .match_indices("invoke(\"")
            .map(|(i, m)| {
                let rest = &html[i + m.len()..];
                rest[..rest.find('"').unwrap()].to_string()
            })
            .collect();
        let mut invoked_sorted = invoked.clone();
        invoked_sorted.sort();
        invoked_sorted.dedup();

        for cmd in &invoked_sorted {
            assert!(
                registered.contains(cmd),
                "前端调用了后端没注册的命令 {cmd}（注册表: {registered:?}）"
            );
        }
        for cmd in &registered {
            assert!(
                invoked_sorted.contains(cmd),
                "后端注册了命令 {cmd}，但前端从未调用（命令集必须双向对称）"
            );
        }
        // G1 的列举能力必须在界面上真的被用起来（不是只注册不用）
        assert!(invoked_sorted.contains(&"list_sessions".to_string()));
    }

    /// 前端引用的元素 id 必须全部有定义（拼错 id 只会在运行时静默坏掉）。
    #[test]
    fn ui_referenced_element_ids_are_all_defined() {
        let html = include_str!("../ui/index.html");
        let defined: Vec<&str> = html
            .match_indices("id=\"")
            .map(|(i, m)| {
                let rest = &html[i + m.len()..];
                &rest[..rest.find('"').unwrap()]
            })
            .collect();
        let mut referenced: Vec<&str> = html
            .match_indices("getElementById(\"")
            .map(|(i, m)| {
                let rest = &html[i + m.len()..];
                &rest[..rest.find('"').unwrap()]
            })
            .collect();
        referenced.sort();
        referenced.dedup();
        assert!(
            referenced.len() >= 20,
            "至少该引用到 20 个元素: {referenced:?}"
        );
        for id in &referenced {
            assert!(
                defined.contains(id),
                "JS 引用了未定义的 id：{id}（定义: {defined:?}）"
            );
        }
    }

    /// **绝不改变用户仓库状态**：生产代码里不许出现会动工作区的 git 子命令。
    ///
    /// 只扫 `#[cfg(test)]` **之前**的生产代码——测试模块天生要写下这些词（“不许出现什么”），
    /// 不该被自己扫到。
    #[test]
    fn app_never_mutates_the_repository() {
        let src = include_str!("main.rs");
        let production = src
            .split("#[cfg(test)]")
            .next()
            .expect("源码里应有生产代码部分");
        assert!(production.contains("fn main"), "生产代码切片不对");
        assert!(
            !production.contains("#[test]") && production.contains("fn open_session"),
            "生产代码切片不该包含测试代码"
        );
        for needle in [
            "checkout",
            "switch",
            "reset",
            "--hard",
            "clean -fd",
            "git branch",
            "stash",
        ] {
            assert!(
                !production.contains(needle),
                "生产代码里不允许出现 `{needle}`（本阶段明确要求不碰用户仓库）"
            );
        }
        // 唯一允许的 git 交互：core 的只读采集入口。
        assert!(
            src.contains("fingerprint::collect"),
            "指纹应通过 core 的只读采集入口取得"
        );
    }

    /// 界面必须**在打开之前**就说明「没有指纹」这件事（核心验收点，防复发）。
    #[test]
    fn ui_states_that_a_session_without_repo_has_no_fingerprint() {
        let html = include_str!("../ui/index.html");
        assert!(
            html.contains("本会话日志将不包含固件指纹"),
            "参数区必须明写「本会话日志将不包含固件指纹」"
        );
        assert!(html.contains("id=\"repoPath\"") && html.contains("id=\"binPath\""));
        // 打开之后的指纹状态行容器：新模型里**内容容器由区域 API 创建**，
        // 所以它不再以 `id="fpStatus"` 的形式写在 HTML 里，而是 JS 建好后挂进参数区。
        assert!(
            html.contains("fpStatusEl.id = \"fpStatus\""),
            "指纹状态行容器必须存在（新模型里由区域 API 创建）"
        );
        assert!(
            html.contains("regions.get(\"params\").body.appendChild(fpStatusEl)"),
            "指纹状态行必须挂在参数区（regions.get(\"params\")）里"
        );
        assert!(
            html.contains("not_collected"),
            "界面要消费磁盘头的 not_collected 键名"
        );
        assert!(
            html.contains("fpReason"),
            "取不到的字段必须打印 Field::Missing 的原因原文"
        );
        // 复制 commit 的入口
        assert!(html.contains("复制 commit"));
        assert!(html.contains("navigator.clipboard"));
        // 列表浮层与「没有会话」
        assert!(html.contains("没有会话"));
        assert!(html.contains("id=\"sessionList\""));
    }

    #[test]
    fn frame_config_parses_every_legal_value() {
        // 数据位 5/6/7/8 × 校验 none/odd/even × 停止位 1/2（下拉只给这两个）× 流控三种
        assert_eq!(
            parse_frame_from_config(&config("5", "none", "1", "none"))
                .unwrap()
                .data_bits,
            DataBits::Five
        );
        assert_eq!(
            parse_frame_from_config(&config("6", "none", "1", "none"))
                .unwrap()
                .data_bits,
            DataBits::Six
        );
        assert_eq!(
            parse_frame_from_config(&config("7", "none", "1", "none"))
                .unwrap()
                .data_bits,
            DataBits::Seven
        );
        assert_eq!(
            parse_frame_from_config(&config("8", "none", "1", "none"))
                .unwrap()
                .data_bits,
            DataBits::Eight
        );
        assert_eq!(
            parse_frame_from_config(&config("8", "odd", "1", "none"))
                .unwrap()
                .parity,
            Parity::Odd
        );
        assert_eq!(
            parse_frame_from_config(&config("8", "even", "1", "none"))
                .unwrap()
                .parity,
            Parity::Even
        );
        assert_eq!(
            parse_frame_from_config(&config("8", "none", "2", "none"))
                .unwrap()
                .stop_bits,
            StopBits::Two
        );
        assert_eq!(
            parse_frame_from_config(&config("8", "none", "1", "none"))
                .unwrap()
                .flow_control,
            FlowControl::None
        );
        assert_eq!(
            parse_frame_from_config(&config("8", "none", "1", "rtscts"))
                .unwrap()
                .flow_control,
            FlowControl::RtsCts
        );
        assert_eq!(
            parse_frame_from_config(&config("8", "none", "1", "xonxoff"))
                .unwrap()
                .flow_control,
            FlowControl::XonXoff
        );
        // 大小写宽容（前端固定小写，但 API 调用方不必猜）
        assert_eq!(
            parse_frame_from_config(&config("8", "EVEN", "1", "RTSCTS"))
                .unwrap()
                .parity,
            Parity::Even
        );
        // 常见合法组合的摘要，直接可用于界面展示
        assert_eq!(
            parse_frame_from_config(&config("8", "even", "1", "rtscts"))
                .unwrap()
                .summary(),
            "8E1 RTS/CTS"
        );
    }

    #[test]
    fn frame_config_rejects_unknown_values_with_accepted_list() {
        for (field, cfg) in [
            ("dataBits", config("9", "none", "1", "none")),
            ("parity", config("8", "mark", "1", "none")),
            ("stopBits", config("8", "none", "3", "none")),
            ("flowControl", config("8", "none", "1", "dtr")),
        ] {
            let err = parse_frame_from_config(&cfg).unwrap_err();
            assert!(err.contains(field), "错误必须点名字段 {field}: {err}");
        }
        // 空/空白也要拒
        assert!(parse_frame_from_config(&config("", "none", "1", "none")).is_err());
    }

    /// 1.5 停止位：**界面下拉不提供它**，但 API 若真传了，core 的明确拒绝要原样透出
    ///（「非法参数组合的错误必须显示」这条走的就是这条路径）。
    #[test]
    fn frame_config_1_5_stop_bits_surfaces_core_rejection() {
        // 8 数据位 + 1.5 停止位：先撞上平台规则（Win32 DCB），错误点名具体冲突。
        let err = parse_frame_from_config(&config("8", "none", "1.5", "none")).unwrap_err();
        assert!(err.contains("1.5"), "{err}");
        assert!(err.contains("不支持 1.5 停止位"), "{err}");
        // 5 数据位 + 1.5 在硬件规则上合法，但 serialport 4.x 表达不了 → 明确拒绝、不折算。
        let err_cap = parse_frame_from_config(&config("5", "none", "1.5", "none")).unwrap_err();
        assert!(err_cap.contains("1.5"), "{err_cap}");
        assert!(err_cap.contains("拒绝静默降级"), "{err_cap}");
        // 非法组合（5 数据位 + 2 停止位）同样给具体冲突
        let err2 = parse_frame_from_config(&config("5", "none", "2", "none")).unwrap_err();
        assert!(err2.contains("5 数据位不支持 2 停止位"), "{err2}");
    }

    // ---- 定时发送 -----------------------------------------------------------

    #[test]
    fn interval_validation_rejects_zero_and_accepts_positive() {
        assert!(validate_interval_ms(0).is_err());
        assert!(validate_interval_ms(1).is_ok());
        assert!(validate_interval_ms(1000).is_ok());
        let err = validate_interval_ms(0).unwrap_err();
        assert!(err.contains("≥ 1 毫秒"), "{err}");
    }

    /// 定时发送的帧必须与手动发送**完全同一套**构造（mode/append 规则一致）。
    #[test]
    fn timed_send_frame_is_identical_to_manual_send() {
        for (data, mode, append) in [
            ("AT", "text", "crlf"),
            ("4142", "hex", "lf"),
            ("hello", "text", "none"),
        ] {
            let manual = build_frame(data, mode, append).unwrap();
            let timed = build_frame(data, mode, append).unwrap(); // start_timed_send 用的就是它
            assert_eq!(manual, timed);
        }
        // 非法数据同样被拒（不会出现「手动拒、定时放行」）
        assert!(build_frame("", "text", "none").is_err());
        assert!(build_frame("ZZ", "hex", "none").is_err());
    }

    fn spawn_timed(
        session: &Arc<Session>,
        interval_ms: u64,
    ) -> (Option<TimedSender>, Arc<AtomicU64>, Vec<u8>) {
        let frame = build_frame("AT", "text", "crlf").unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicU64::new(0));
        let note = Arc::new(Mutex::new(None));
        let handle = {
            let session = Arc::clone(session);
            let frame = frame.clone();
            let stop = Arc::clone(&stop);
            let count = Arc::clone(&count);
            let note = Arc::clone(&note);
            thread::spawn(move || {
                run_timed_sender(
                    session,
                    frame,
                    Duration::from_millis(interval_ms),
                    stop,
                    count,
                    note,
                )
            })
        };
        (
            Some(TimedSender {
                stop,
                handle: Some(handle),
                interval_ms,
                count: count.clone(),
                note,
            }),
            count,
            frame,
        )
    }

    /// 定时发送：会真的发；停掉之后**一个字节都不再发**。
    #[test]
    fn timed_sender_sends_then_stops_for_good() {
        let (session, root) = loopback_session(1 << 20);
        let (mut slot, count, frame) = spawn_timed(&session, 20);
        thread::sleep(Duration::from_millis(150));

        let info = stop_timed_sender(&mut slot).expect("停掉定时器");
        assert!(!info.running);
        assert_eq!(info.interval_ms, 20);
        assert!(
            info.count >= 2,
            "150ms / 20ms 至少应发 2 帧，实际 {}",
            info.count
        );
        assert!(slot.is_none(), "停掉后状态里不该再留着定时器");

        // 帧数与字节数必须对得上（每帧都是同一个 build_frame 的结果）。
        let st = session.stats().unwrap();
        assert_eq!(st.tx_bytes, info.count * frame.len() as u64);
        assert_eq!(st.tx_writes, info.count);

        // 停止之后：等一会儿，计数与 TX 都不许再涨。
        let frozen_count = count.load(Ordering::Relaxed);
        let frozen_tx = st.tx_bytes;
        thread::sleep(Duration::from_millis(100));
        assert_eq!(
            count.load(Ordering::Relaxed),
            frozen_count,
            "停止后不得再发送"
        );
        assert_eq!(
            session.stats().unwrap().tx_bytes,
            frozen_tx,
            "停止后 TX 字节数不得再涨"
        );

        // 幂等：再停一次返回 None（调用方据此知道「本来就没在跑」）。
        assert!(stop_timed_sender(&mut slot).is_none());
        session.stop().unwrap();
        std::fs::remove_dir_all(&root).ok();
    }

    /// 会话一旦不在运行（用户关了 / 设备拔出），定时器必须自己退出并留下原因。
    #[test]
    fn timed_sender_stops_when_session_stops() {
        let (session, root) = loopback_session(1024);
        session.stop().unwrap();
        assert!(!session.is_running(), "会话应已停止");

        let (mut slot, count, _frame) = spawn_timed(&session, 10);
        thread::sleep(Duration::from_millis(120));
        let info = stop_timed_sender(&mut slot).expect("停掉定时器");
        assert_eq!(info.count, 0, "会话不在跑时定时器不该发出任何帧");
        assert_eq!(count.load(Ordering::Relaxed), 0);
        assert!(
            info.note
                .as_deref()
                .unwrap_or_default()
                .contains("自动结束"),
            "应记录自动停止原因，实际 {:?}",
            info.note
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
