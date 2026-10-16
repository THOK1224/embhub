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
    describe_serial, DataBits, DisplayBatch, FlowControl, LogStore, Parity, PipelineConfig,
    SerialConfig, SerialFrame, SerialSource, Session, SessionMeta, SessionSnapshot, StopBits,
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
// 4 个 Tauri command
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
    let meta = SessionMeta::new(format!("serial:{port}@{}{}", config.baud, frame.summary()));
    let session = Arc::new(Session::new(
        store,
        meta,
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
    let opened = SessionOpened {
        port: port.clone(),
        baud: config.baud,
        frame: frame_summary.clone(),
        session_id,
        session_dir: session_dir.display().to_string(),
        start_wall_ms: start.wall_ms,
        start_mono_micros: start.mono_micros,
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
            stop_timed_send
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

    /// 删掉 CSS 注释（`/* ... */`）——注释里会出现 `{`、`}`、`;` 之类字符，
    /// 不先删掉，按规则切分就会把注释误当成选择器。
    fn strip_css_comments(css: &str) -> String {
        let mut out = String::with_capacity(css.len());
        let mut rest = css;
        while let Some(start) = rest.find("/*") {
            out.push_str(&rest[..start]);
            match rest[start + 2..].find("*/") {
                Some(end) => rest = &rest[start + 2 + end + 2..],
                None => return out, // 注释没闭合：后面全部丢弃
            }
        }
        out.push_str(rest);
        out
    }

    /// 把样式表拆成 (选择器, 声明块) 列表。
    ///
    /// 只按 `{` / `}` 切分，不处理嵌套与 `@media`——本文件的样式表没有这些，
    /// 而这个助手只服务于下面的布局不变量断言，故意保持极小。
    fn css_rules(css: &str) -> Vec<(String, String)> {
        let stripped = strip_css_comments(css);
        let mut out = Vec::new();
        for block in stripped.split('}') {
            let Some((sel, body)) = block.split_once('{') else {
                continue;
            };
            let sel = sel.trim();
            if sel.is_empty() {
                continue;
            }
            out.push((sel.to_string(), body.trim().to_string()));
        }
        out
    }

    /// 取某个选择器（精确匹配）的声明块。
    fn css_decls(rules: &[(String, String)], selector: &str) -> String {
        rules
            .iter()
            .filter(|(sel, _)| sel.split(',').any(|s| s.trim() == selector))
            .map(|(_, body)| body.as_str())
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// 断言某个选择器的声明里含有某段子串（去掉多余空白后比较）。
    fn assert_decl(rules: &[(String, String)], selector: &str, needle: &str) {
        let got = css_decls(rules, selector).replace('\n', " ");
        let got_squeezed: String = got.split_whitespace().collect::<Vec<_>>().join(" ");
        let needle_squeezed: String = needle.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            got_squeezed.contains(&needle_squeezed),
            "选择器 `{selector}` 必须包含 `{needle}`，实际声明为: {got_squeezed}"
        );
    }

    /// t17 布局缺陷的回归闸门。
    ///
    /// 背景（详见 `ui/index.html` 顶部的根因注释）：页面是 body 上的纵向 flexbox，
    /// 一旦「固定内容区」被允许收缩，它们的内容就会溢出到兄弟节点上（重叠/被挤出可视区）。
    /// 这个测试把修复所依赖的**结构不变量**钉住——它不能替代真机截图，
    /// 但只要有人把布局改回症状补丁或删掉关键约束，`cargo test` 就会红。
    #[test]
    fn layout_invariants_prevent_the_receive_pane_from_squeezing_others() {
        let html = include_str!("../ui/index.html");
        let css = html
            .split_once("<style>")
            .and_then(|(_, rest)| rest.split_once("</style>"))
            .map(|(css, _)| css)
            .expect("index.html 必须有 <style> 块");
        let rules = css_rules(css);
        assert!(!rules.is_empty(), "样式表没解析出任何规则");

        // ① 页面是限高的纵向 flexbox，且有溢出兜底（否则内容一多整页长高、下方被切掉）。
        assert_decl(&rules, "body", "display: flex");
        assert_decl(&rules, "body", "flex-direction: column");
        assert_decl(&rules, "body", "height: 100%");
        assert_decl(&rules, "body", "overflow: auto");
        // `html, body { height: 100% }`：按逗号拆开后查询 `html` 即可命中该规则。
        assert_decl(&rules, "html", "height: 100%");

        // ② 固定内容区**永不收缩**：这是「不再重叠」的核心（内容永不溢出到兄弟身上）。
        assert_decl(&rules, "fieldset.fixed", "flex: 0 0 auto");
        assert_decl(&rules, ".row", "flex: 0 0 auto");
        assert_decl(&rules, "#log", "flex: 0 0 auto");

        // ③ 接收区是唯一的伸缩区，而且必须能缩到内容高度以下（min-height: 0）。
        assert_decl(&rules, "fieldset.pane", "flex: 1 1 auto");
        assert_decl(&rules, "fieldset.pane", "min-height: 0");
        assert_decl(&rules, "#rx", "flex: 1 1 auto");
        assert_decl(&rules, "#rx", "min-height: 0");
        assert_decl(&rules, "#rx", "overflow: auto");

        // ④ 症状补丁的指纹必须不存在：给接收区/页面写死像素高度或像素下限。
        for selector in ["#rx", "fieldset.pane", "body"] {
            let decls = css_decls(&rules, selector);
            for (idx, _) in decls.match_indices("min-height:") {
                let tail = &decls[idx + "min-height:".len()..];
                let value: String = tail
                    .trim_start()
                    .chars()
                    .take_while(|c| *c != ';' && *c != ' ')
                    .collect();
                assert!(
                    !value.ends_with("px"),
                    "`{selector}` 不允许用像素 min-height（{value}）凑布局：\
                     那会在别的窗口尺寸下把下方区域顶出去"
                );
            }
            for (idx, _) in decls.match_indices("height:") {
                let tail = &decls[idx + "height:".len()..];
                let value: String = tail
                    .trim_start()
                    .chars()
                    .take_while(|c| *c != ';' && *c != ' ')
                    .collect();
                assert!(
                    !value.ends_with("px") || selector == "body",
                    "`{selector}` 不允许写死像素高度（{value}）：布局应由 flex 分配，\
                     而不是在某一种窗口尺寸下凑对"
                );
            }
        }

        // ⑤ 长行不许撑宽页面：接收区与日志面板都必须允许任意断行（min-content 宽度才不会失控）。
        assert_decl(&rules, "#rx", "overflow-wrap: anywhere");
        assert_decl(&rules, "#rx", "white-space: pre-wrap");
        assert_decl(&rules, "#log", "overflow-wrap: anywhere");

        // ⑥ CSS 与 HTML 必须对得上：类名不能只写在样式里。
        assert_eq!(
            html.matches("fieldset class=\"pane\"").count(),
            1,
            "必须恰好有一个 class=\"pane\" 的面板（接收区）"
        );
        assert_eq!(
            html.matches("fieldset class=\"fixed\"").count(),
            2,
            "必须恰好有两个 class=\"fixed\" 的面板（发送区 + 事件区）"
        );
        assert!(
            !strip_css_comments(css).contains(".grow"),
            "旧的 `.grow` 规则已废弃：它把接收区与固定区混为一谈，正是挤压的来源"
        );
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
        }
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
