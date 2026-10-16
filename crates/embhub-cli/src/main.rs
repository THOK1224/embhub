//! embhub-cli —— 同一 core 的无头入口（脚本化 / CI / 人工验收）。
//!
//! 用法：
//!   embhub-cli list
//!   embhub-cli read --port COM3 [--baud 115200] [--seconds 3]
//!   embhub-cli selftest [--bytes 67108864] [--seed 42] [--repo <dir> [--bin <file>]]
//!   embhub-cli fingerprint --repo <dir> [--bin <file>]
//!   embhub-cli stream --bytes N [--chunk K] [--consume fast|slow|stalled] [--display-capacity C]
//!   embhub-cli loopback --bytes N [--chunk K] [--display-capacity C]
//!   embhub-cli send --port <p> --data <hex:AA55FF|text:hello> [--append none|cr|lf|crlf] [--repeat N]
//!
//! `selftest` 是 Phase 1「零丢失」闭环的无硬件自证：合成源注入 → LogStore 落盘
//! → `open_replay` 读回，打印「注入 / 落盘 / 回放读回」三个计数并逐字节比对；
//! 任何一项不等都以非零码退出。给了 `--repo` 时再走一遍
//! 「采集指纹 → 写进会话头（v2）→ 从会话头读回 → 逐字段比对」，不一致同样非零退出。
//!
//! `fingerprint` 按 SPEC 3.5 采集固件指纹（git commit/dirty + 构建产物**内容**哈希）。
//! 取不到的字段一律打印**原因**（不允许静默省略）；采集不到某个字段不算命令失败，
//! 所以退出码仍是 0，只有参数错误（缺 `--repo`）才非零退出。
//!
//! `stream` 走 PLAN 4.3 的流式管线（常驻读取 + 批量推送 + 有界显示队列背压）：
//! **退出码只取决于落盘零丢失**（`logged_bytes == N` 且回放读回也是 N），
//! 显示被丢掉多少都不影响退出码——这正是「显示可以丢、落盘一个字节都不能丢」。
//!
//! `loopback` 是无硬件证明「**能发**」的闭环：合成源回环（写进去的字节被同一个源读回）
//! → 会话（源 + 管线 + 落盘）→ 断言「写入 == 读回 == 落盘 == 回放」四方一致。
//! `send` 则是对真实串口的发送命令（`ByteSink`；参数非法时明确报错并非零退出）。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use embhub_core::fingerprint::{self, Field};
use embhub_core::{
    ByteSink, ByteSource, Chunk, DisplayBatch, FingerprintSection, FirmwareFingerprint, LogStore,
    Pipeline, PipelineConfig, SerialConfig, SerialSink, Session, SessionId, SessionMeta,
    SourceError, StopReason, SyntheticConfig, SyntheticSource, DEFAULT_BATCH_BYTES,
    DEFAULT_BATCH_TIMEOUT_MS, DEFAULT_DISPLAY_CAPACITY, RECORD_HEADER_LEN,
};

/// `selftest` 默认注入量：64 MiB（SPEC 3.3 的默认分片大小）。
const SELFTEST_DEFAULT_BYTES: u64 = 64 * 1024 * 1024;
/// 合成源单次 `read` 的块大小。
const SELFTEST_CHUNK_BYTES: usize = 64 * 1024;
/// 分片上限。故意设得远小于 64 MiB，让一次 selftest 真正走多分片路径。
const SELFTEST_SEGMENT_BYTES: u64 = 1024 * 1024;
/// 默认 PRNG 种子（固定值 → selftest 可复现）。
const SELFTEST_DEFAULT_SEED: u64 = 42;
/// 回放侧读缓冲大小。故意与 `SELFTEST_CHUNK_BYTES` 不同，逼出跨记录切片。
const SELFTEST_REPLAY_CHUNK: usize = 48 * 1024;
/// 逐字节比对的分块大小。
const COMPARE_BLOCK: usize = 64 * 1024;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("list");

    match cmd {
        "list" => cmd_list(),
        "read" => cmd_read(&args),
        "selftest" => cmd_selftest(&args),
        "fingerprint" => cmd_fingerprint(&args),
        "stream" => cmd_stream(&args),
        "loopback" => cmd_loopback(&args),
        "send" => cmd_send(&args),
        other => {
            eprintln!(
                "未知子命令: {other}（可用: list | read | selftest | fingerprint | stream | loopback | send）"
            );
            ExitCode::FAILURE
        }
    }
}

/// 取 `--key value` 形式的参数值。
fn arg_value(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn cmd_list() -> ExitCode {
    match embhub_core::list_ports() {
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
    }
}

fn cmd_read(args: &[String]) -> ExitCode {
    let port = match arg_value(args, "--port") {
        Some(p) => p,
        None => {
            eprintln!("用法: embhub-cli read --port COMx [--baud 115200] [--seconds 3]");
            return ExitCode::FAILURE;
        }
    };
    let baud: u32 = arg_value(args, "--baud")
        .and_then(|v| v.parse().ok())
        .unwrap_or(115_200);
    let seconds: u64 = arg_value(args, "--seconds")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);

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

/// 把采集结果渲染成一行：有值就打印值，缺失就打印原因（绝不静默留空）。
fn show_field<T: std::fmt::Display>(field: &Field<T>) -> String {
    match field {
        Field::Value(v) => v.to_string(),
        Field::Missing(reason) => format!("取不到（原因: {reason}）"),
    }
}

/// 显示消费者怎么消费（用来演示背压：快 / 慢 / 干脆停住）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConsumeMode {
    /// 全速抽干。
    Fast,
    /// 每批睡一会儿：慢消费者，队列会溢出。
    Slow,
    /// 拿着接收端一次都不 `recv`：消费者停住。
    Stalled,
}

impl ConsumeMode {
    fn parse(v: &str) -> Option<Self> {
        match v {
            "fast" => Some(ConsumeMode::Fast),
            "slow" => Some(ConsumeMode::Slow),
            "stalled" => Some(ConsumeMode::Stalled),
            _ => None,
        }
    }
}

/// `stream` 的分片上限：比 selftest 的大，减少文件数。
const STREAM_SEGMENT_BYTES: u64 = 8 * 1024 * 1024;
/// `stream` 默认的单次读块大小。
const STREAM_DEFAULT_CHUNK: usize = 64 * 1024;
/// 慢消费者每批的睡眠时长。
const STREAM_SLOW_DELAY: Duration = Duration::from_millis(5);
/// 等管线自然结束的上限（64 MiB 在 debug 构建里约 7 s，留足余量）。
const STREAM_WAIT_TIMEOUT: Duration = Duration::from_secs(600);

/// `stream` 跑完之后的全部计数（退出码只看落盘那两项）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct StreamSummary {
    source_bytes: u64,
    logged_bytes: u64,
    replay_bytes: u64,
    batches_emitted: u64,
    display_bytes_emitted: u64,
    batches_dropped: u64,
    display_bytes_dropped: u64,
    consumer_batches: u64,
    consumer_bytes: u64,
    stop_reason: StopReason,
    fatal_error: Option<String>,
}

impl StreamSummary {
    /// 落盘零丢失（且回放可验证）才算通过——**显示丢弃不影响这个判定**。
    fn disk_path_ok(&self, expected: u64) -> bool {
        self.logged_bytes == expected
            && self.replay_bytes == expected
            && self.source_bytes == expected
            && self.fatal_error.is_none()
    }
}

/// 从落盘分片回放数一遍字节数（不能只信 stats 的自报计数）。
fn replay_bytes_of(store: &LogStore, id: &SessionId) -> Result<u64, String> {
    let mut r = store
        .open_replay(id)
        .map_err(|e| format!("open_replay 失败: {e}"))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let b = r.read(&mut buf).map_err(|e| format!("回放读取失败: {e}"))?;
        if b.len == 0 {
            break;
        }
        total += b.len as u64;
    }
    Ok(total)
}

/// 起一个显示消费者线程，返回 `(批次数, 字节数)`。
fn spawn_consumer(rx: Receiver<DisplayBatch>, mode: ConsumeMode) -> JoinHandle<(u64, u64)> {
    thread::spawn(move || {
        let (mut batches, mut bytes) = (0u64, 0u64);
        while let Ok(batch) = rx.recv() {
            batches += 1;
            bytes += batch.bytes.len() as u64;
            if mode == ConsumeMode::Slow {
                thread::sleep(STREAM_SLOW_DELAY);
            }
        }
        (batches, bytes)
    })
}

/// 跑一次流式管线并收集计数。`stalled` 模式下接收端全程被持有但一次都不 `recv`。
fn run_stream(
    total_bytes: u64,
    chunk: usize,
    consume: ConsumeMode,
    display_capacity: usize,
) -> Result<StreamSummary, String> {
    let root = temp_session_root("stream");
    let store = Arc::new(LogStore::new(&root, STREAM_SEGMENT_BYTES));
    let source = SyntheticSource::open(&SyntheticConfig {
        seed: 42,
        total_bytes,
        chunk_size: chunk.max(1),
    })
    .map_err(|e| format!("打开合成源失败: {e}"))?;
    let cfg = PipelineConfig {
        display_capacity,
        stop_after_bytes: Some(total_bytes),
        ..PipelineConfig::default()
    };
    let meta = SessionMeta::new(format!(
        "synthetic:seed=42,total={total_bytes},chunk={}",
        chunk.max(1)
    ));

    let result = (|| -> Result<StreamSummary, String> {
        let pipeline = Pipeline::spawn(Arc::clone(&store), meta, Box::new(source), cfg)
            .map_err(|e| format!("起管线失败: {e}"))?;
        let rx = pipeline.take_display_receiver();

        // 消费者：fast/slow 起线程；stalled 就把接收端攥在手里（drop 都不 drop）。
        let mut held_rx = None;
        let consumer = match (consume, rx) {
            (ConsumeMode::Stalled, rx) => {
                held_rx = rx;
                None
            }
            (mode, Some(rx)) => Some(spawn_consumer(rx, mode)),
            (_, None) => None,
        };

        // 等管线自己读满 N 字节（不切断它），然后 stop：join 读取线程 + seal 会话。
        if !pipeline.wait_until_finished(STREAM_WAIT_TIMEOUT) {
            return Err(format!(
                "管线没在 {:?} 内结束: {:?}",
                STREAM_WAIT_TIMEOUT,
                pipeline.stats()
            ));
        }
        pipeline
            .stop()
            .map_err(|e| format!("stop/seal 失败: {e}"))?;

        let (consumer_batches, consumer_bytes) = match consumer {
            Some(h) => h.join().unwrap_or((0, 0)),
            None => (0, 0),
        };
        let st = pipeline.stats();
        let replay = replay_bytes_of(&store, pipeline.session_id())?;
        drop(held_rx); // 消费者「停住」到最后一刻
        Ok(StreamSummary {
            source_bytes: st.source_bytes,
            logged_bytes: st.logged_bytes,
            replay_bytes: replay,
            batches_emitted: st.batches_emitted,
            display_bytes_emitted: st.display_bytes_emitted,
            batches_dropped: st.batches_dropped,
            display_bytes_dropped: st.display_bytes_dropped,
            consumer_batches,
            consumer_bytes,
            stop_reason: st.stop_reason,
            fatal_error: st.fatal_error,
        })
    })();

    if let Err(e) = fs::remove_dir_all(&root) {
        eprintln!("提示: 清理 {} 失败: {e}", root.display());
    }
    result
}

fn cmd_stream(args: &[String]) -> ExitCode {
    let usage = "用法: embhub-cli stream --bytes N [--chunk K] [--consume fast|slow|stalled] [--display-capacity C]";
    let total_bytes: u64 = match arg_value(args, "--bytes") {
        Some(v) => match v.parse() {
            Ok(n) => n,
            Err(e) => {
                eprintln!("--bytes 必须是整数: {v}（{e}）\n{usage}");
                return ExitCode::FAILURE;
            }
        },
        None => {
            eprintln!("{usage}");
            return ExitCode::FAILURE;
        }
    };
    let chunk: usize = match arg_value(args, "--chunk") {
        Some(v) => match v.parse() {
            Ok(n) => n,
            Err(e) => {
                eprintln!("--chunk 必须是整数: {v}（{e}）\n{usage}");
                return ExitCode::FAILURE;
            }
        },
        None => STREAM_DEFAULT_CHUNK,
    };
    let consume = match arg_value(args, "--consume") {
        Some(v) => match ConsumeMode::parse(&v) {
            Some(m) => m,
            None => {
                eprintln!("--consume 只能是 fast | slow | stalled，实际 {v}\n{usage}");
                return ExitCode::FAILURE;
            }
        },
        None => ConsumeMode::Fast,
    };
    let display_capacity: usize = match arg_value(args, "--display-capacity") {
        Some(v) => match v.parse() {
            Ok(n) => n,
            Err(e) => {
                eprintln!("--display-capacity 必须是整数: {v}（{e}）\n{usage}");
                return ExitCode::FAILURE;
            }
        },
        None => DEFAULT_DISPLAY_CAPACITY,
    };

    println!("== embhub stream（PLAN 4.3：落盘无损 + 有界显示队列背压）==");
    println!("目标字节数    : {total_bytes}");
    println!("单次读块      : {chunk} 字节");
    println!("显示消费者    : {consume:?}");
    println!("显示队列容量  : {display_capacity} 批（每批 ≥{DEFAULT_BATCH_BYTES} 字节或 ≥{DEFAULT_BATCH_TIMEOUT_MS}ms）");

    match run_stream(total_bytes, chunk, consume, display_capacity) {
        Ok(s) => {
            println!("source_bytes            : {}", s.source_bytes);
            println!("logged_bytes            : {}", s.logged_bytes);
            println!("batches_emitted         : {}", s.batches_emitted);
            println!("batches_dropped         : {}", s.batches_dropped);
            println!("display_bytes_dropped   : {}", s.display_bytes_dropped);
            println!("display_bytes_emitted   : {}", s.display_bytes_emitted);
            println!("consumer_batches        : {}", s.consumer_batches);
            println!("consumer_bytes          : {}", s.consumer_bytes);
            println!("replay_bytes            : {}", s.replay_bytes);
            println!("stop_reason             : {:?}", s.stop_reason);
            println!(
                "fatal_error             : {}",
                s.fatal_error.as_deref().unwrap_or("none")
            );
            if s.display_bytes_dropped > 0 {
                println!(
                    "提示: 显示已丢弃 {} 批 / {} 字节（落盘不受影响）",
                    s.batches_dropped, s.display_bytes_dropped
                );
            }
            if s.disk_path_ok(total_bytes) {
                println!(
                    "✓ 落盘零丢失：logged == replay == source == {total_bytes} 字节（显示丢弃不影响本判定）"
                );
                ExitCode::SUCCESS
            } else {
                println!(
                    "✗ 落盘不完整：期望 {total_bytes}，实际 logged {} / replay {} / source {}",
                    s.logged_bytes, s.replay_bytes, s.source_bytes
                );
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("stream 失败: {e}");
            ExitCode::FAILURE
        }
    }
}

/// 行尾终结符（`--append`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppendEnd {
    None,
    Cr,
    Lf,
    Crlf,
}

impl AppendEnd {
    fn parse(v: &str) -> Option<Self> {
        match v {
            "none" => Some(AppendEnd::None),
            "cr" => Some(AppendEnd::Cr),
            "lf" => Some(AppendEnd::Lf),
            "crlf" => Some(AppendEnd::Crlf),
            _ => None,
        }
    }

    fn bytes(self) -> &'static [u8] {
        match self {
            AppendEnd::None => b"",
            AppendEnd::Cr => b"\r",
            AppendEnd::Lf => b"\n",
            AppendEnd::Crlf => b"\r\n",
        }
    }

    fn label(self) -> &'static str {
        match self {
            AppendEnd::None => "none",
            AppendEnd::Cr => "cr",
            AppendEnd::Lf => "lf",
            AppendEnd::Crlf => "crlf",
        }
    }
}

/// 解析 `--data`：`hex:AA55FF` 或 `text:hello`。**空数据一律拒绝**（不允许静默发送空帧）。
fn parse_send_payload(data: &str) -> Result<Vec<u8>, String> {
    if let Some(hex) = data.strip_prefix("hex:") {
        let clean: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
        if clean.is_empty() {
            return Err("--data hex: 之后没有字节（拒绝发送空数据）".to_string());
        }
        if !clean.len().is_multiple_of(2) {
            return Err(format!(
                "hex 必须是偶数个字符（实际 {} 个）: {clean}",
                clean.len()
            ));
        }
        let bytes = clean.as_bytes();
        let mut out = Vec::with_capacity(bytes.len() / 2);
        for i in (0..bytes.len()).step_by(2) {
            let hi = (bytes[i] as char)
                .to_digit(16)
                .ok_or_else(|| format!("hex 里有非十六进制字符: {clean}"))?;
            let lo = (bytes[i + 1] as char)
                .to_digit(16)
                .ok_or_else(|| format!("hex 里有非十六进制字符: {clean}"))?;
            out.push((hi * 16 + lo) as u8);
        }
        Ok(out)
    } else if let Some(text) = data.strip_prefix("text:") {
        if text.is_empty() {
            return Err("--data text: 之后为空（拒绝发送空数据）".to_string());
        }
        Ok(text.as_bytes().to_vec())
    } else {
        Err(format!(
            "--data 必须写成 hex:AA55FF 或 text:hello（实际 {data:?}）"
        ))
    }
}

/// 一帧 = 数据 + 终结符（`--repeat N` 时每一帧都带终结符）。
fn build_send_frame(payload: &[u8], append: AppendEnd) -> Vec<u8> {
    let mut frame = Vec::with_capacity(payload.len() + append.bytes().len());
    frame.extend_from_slice(payload);
    frame.extend_from_slice(append.bytes());
    frame
}

fn hex_preview(bytes: &[u8], max: usize) -> String {
    bytes
        .iter()
        .take(max)
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `send --port <p> --data <hex:…|text:…> [--append none|cr|lf|crlf] [--repeat N] [--baud B]`
///
/// 参数非法（缺 `--port`/`--data`、hex 非法或空、`text:` 为空、`--append` 不认、
/// `--repeat 0`）一律**先报错再动手**，绝不静默发送空数据。
fn cmd_send(args: &[String]) -> ExitCode {
    let usage = "用法: embhub-cli send --port <p> --data <hex:AA55FF|text:hello> \
                [--append none|cr|lf|crlf] [--repeat N] [--baud 115200]";

    let port = match arg_value(args, "--port") {
        Some(p) if !p.trim().is_empty() => p,
        Some(_) => {
            eprintln!("--port 不得为空\n{usage}");
            return ExitCode::FAILURE;
        }
        None => {
            eprintln!("缺少 --port\n{usage}");
            return ExitCode::FAILURE;
        }
    };
    let data = match arg_value(args, "--data") {
        Some(d) => d,
        None => {
            eprintln!("缺少 --data（不允许静默发送空数据）\n{usage}");
            return ExitCode::FAILURE;
        }
    };
    let payload = match parse_send_payload(&data) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}\n{usage}");
            return ExitCode::FAILURE;
        }
    };
    let append = match arg_value(args, "--append") {
        Some(v) => match AppendEnd::parse(&v) {
            Some(a) => a,
            None => {
                eprintln!("--append 只能是 none | cr | lf | crlf（实际 {v}）\n{usage}");
                return ExitCode::FAILURE;
            }
        },
        None => AppendEnd::None,
    };
    let repeat: u64 = match arg_value(args, "--repeat") {
        Some(v) => match v.parse::<u64>() {
            Ok(0) => {
                eprintln!("--repeat 必须 ≥ 1（0 次发送等于什么都没发）\n{usage}");
                return ExitCode::FAILURE;
            }
            Ok(n) => n,
            Err(e) => {
                eprintln!("--repeat 必须是正整数: {v}（{e}）\n{usage}");
                return ExitCode::FAILURE;
            }
        },
        None => 1,
    };
    let baud: u32 = arg_value(args, "--baud")
        .and_then(|v| v.parse().ok())
        .unwrap_or(115_200);

    let frame = build_send_frame(&payload, append);
    let cfg = SerialConfig {
        port_name: port.clone(),
        baud,
        read_timeout_ms: 200,
    };
    let mut sink = match SerialSink::open(&cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("无法打开 {port}: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!("== embhub send（ByteSink）==");
    println!("端口          : {port} @ {baud}");
    println!("数据          : {} 字节（{}）", payload.len(), data);
    println!(
        "终结符        : {}（{} 字节）",
        append.label(),
        append.bytes().len()
    );
    println!("重复          : {repeat} 次");

    let written = match write_frame_repeatedly(&mut sink, &frame, repeat) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "单帧字节      : {}（HEX: {}）",
        frame.len(),
        hex_preview(&frame, 32)
    );
    println!("共发送        : {written} 字节");
    ExitCode::SUCCESS
}

/// 把同一帧写 `repeat` 次（每次 `flush`），返回成功写出的总字节数。
/// 失败时把「第几次 / 此前已发多少字节」写进错误串——发送失败必须能定位到进度。
fn write_frame_repeatedly(
    sink: &mut dyn ByteSink,
    frame: &[u8],
    repeat: u64,
) -> Result<u64, String> {
    let mut written: u64 = 0;
    for i in 1..=repeat {
        sink.write_all(frame)
            .and_then(|()| sink.flush())
            .map_err(|e| format!("第 {i} 次发送失败（此前已发 {written} 字节）: {e}"))?;
        written += frame.len() as u64;
    }
    Ok(written)
}

/// `loopback --bytes N [--chunk K] [--display-capacity C]` 的计数快照。
#[derive(Debug, Clone, PartialEq, Eq)]
struct LoopbackSummary {
    /// 通过 `Session::send` 写出去（进回环）的字节数。
    written_bytes: u64,
    /// 管线从回环源读回的字节数。
    read_back_bytes: u64,
    /// 落盘字节数。
    logged_bytes: u64,
    /// 由 `open_replay` 独立回放读回的字节数。
    replay_bytes: u64,
    /// 独立重建的注入序列字节数（逐字节比对的参照）。
    reference_bytes: u64,
    /// 首个不一致偏移（`None` = 完全一致）。
    first_diff: Option<u64>,
    batches_emitted: u64,
    batches_received: u64,
    display_bytes_received: u64,
    fatal_error: Option<String>,
}

impl LoopbackSummary {
    /// 四方一致（写入 == 读回 == 落盘 == 回放）+ 逐字节一致 + 无致命错误。
    fn consistent(&self, expected: u64) -> bool {
        self.written_bytes == expected
            && self.read_back_bytes == expected
            && self.logged_bytes == expected
            && self.replay_bytes == expected
            && self.reference_bytes == expected
            && self.first_diff.is_none()
            && self.fatal_error.is_none()
    }
}

/// 无硬件端到端：合成源回环 → `Session`（源 + 管线 + 落盘）→ 四方计数 + 逐字节比对。
fn run_loopback(
    total_bytes: u64,
    chunk: usize,
    display_capacity: usize,
) -> Result<LoopbackSummary, String> {
    let chunk = chunk.max(1);
    let root = temp_session_root("loopback");
    let store = Arc::new(LogStore::new(&root, STREAM_SEGMENT_BYTES));
    let source = SyntheticSource::loopback_with_chunk(chunk);
    let sink = source
        .try_clone_sink()
        .map_err(|e| format!("取回环写入句柄失败: {e}"))?;
    let meta = SessionMeta::new(format!("loopback:synthetic,chunk={chunk}"));
    let cfg = PipelineConfig {
        display_capacity,
        stop_after_bytes: Some(total_bytes),
        ..PipelineConfig::default()
    };
    let session = Session::new(
        Arc::clone(&store),
        meta,
        Box::new(source),
        Some(Box::new(sink)),
        cfg,
    );

    let result = (|| -> Result<LoopbackSummary, String> {
        session.start().map_err(|e| e.to_string())?;

        // 写侧：用同一份 (seed,total,chunk) 的确定性合成流生成要发的字节，
        // 这样「发出去的是什么」可以事后独立重建，用于逐字节核对。
        let gen_cfg = SyntheticConfig {
            seed: 42,
            total_bytes,
            chunk_size: chunk,
        };
        let mut generator = SyntheticSource::open(&gen_cfg).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; chunk];
        loop {
            let b = generator.read(&mut buf).map_err(|e| e.to_string())?;
            if b.len == 0 {
                break;
            }
            session
                .send(&buf[..b.len])
                .map_err(|e| format!("发送失败: {e}"))?;
        }

        if !session.wait_until_finished(STREAM_WAIT_TIMEOUT) {
            return Err(format!(
                "管线没在 {STREAM_WAIT_TIMEOUT:?} 内读满 {total_bytes} 字节"
            ));
        }
        session.stop().map_err(|e| e.to_string())?;
        let st = session.stats().map_err(|e| e.to_string())?;
        let id = session.session_id().ok_or("会话没有 id")?;

        // 排空显示通道：已发出的批次数是已知的，所以这里能确定地排干净。
        let mut batches_received = 0u64;
        let mut display_bytes_received = 0u64;
        while batches_received < st.pipeline.batches_emitted {
            match session
                .recv_batch(Duration::from_millis(200))
                .map_err(|e| e.to_string())?
            {
                Some(b) => {
                    batches_received += 1;
                    display_bytes_received += b.bytes.len() as u64;
                }
                None => break,
            }
        }

        // 独立回放：从落盘分片读回，并与「可重建的注入序列」逐字节比对。
        let mut replay = store
            .open_replay(&id)
            .map_err(|e| format!("open_replay 失败: {e}"))?;
        let mut reference =
            SyntheticSource::open(&gen_cfg).map_err(|e| format!("打开参照合成源失败: {e}"))?;
        let diff = compare_streams(&mut reference, &mut replay)?;

        Ok(LoopbackSummary {
            written_bytes: st.tx_bytes,
            read_back_bytes: st.pipeline.source_bytes,
            logged_bytes: st.pipeline.logged_bytes,
            replay_bytes: diff.actual_bytes,
            reference_bytes: diff.expected_bytes,
            first_diff: diff.first_diff,
            batches_emitted: st.pipeline.batches_emitted,
            batches_received,
            display_bytes_received,
            fatal_error: st.pipeline.fatal_error.clone(),
        })
    })();

    if let Err(e) = fs::remove_dir_all(&root) {
        eprintln!("提示: 清理 {} 失败: {e}", root.display());
    }
    result
}

fn cmd_loopback(args: &[String]) -> ExitCode {
    let usage = "用法: embhub-cli loopback --bytes N [--chunk K] [--display-capacity C]";
    let total_bytes: u64 = match arg_value(args, "--bytes") {
        Some(v) => match v.parse() {
            Ok(n) => n,
            Err(e) => {
                eprintln!("--bytes 必须是整数: {v}（{e}）\n{usage}");
                return ExitCode::FAILURE;
            }
        },
        None => {
            eprintln!("缺少 --bytes\n{usage}");
            return ExitCode::FAILURE;
        }
    };
    let chunk: usize = match arg_value(args, "--chunk") {
        Some(v) => match v.parse() {
            Ok(n) => n,
            Err(e) => {
                eprintln!("--chunk 必须是整数: {v}（{e}）\n{usage}");
                return ExitCode::FAILURE;
            }
        },
        None => STREAM_DEFAULT_CHUNK,
    };
    let display_capacity: usize = match arg_value(args, "--display-capacity") {
        Some(v) => match v.parse() {
            Ok(n) => n,
            Err(e) => {
                eprintln!("--display-capacity 必须是整数: {v}（{e}）\n{usage}");
                return ExitCode::FAILURE;
            }
        },
        None => DEFAULT_DISPLAY_CAPACITY,
    };

    println!("== embhub loopback（无硬件证明「能发」：写入 == 读回 == 落盘 == 回放）==");
    println!("目标字节数    : {total_bytes}");
    println!("回环块大小    : {chunk} 字节");

    match run_loopback(total_bytes, chunk, display_capacity) {
        Ok(s) => {
            println!("written_bytes           : {}", s.written_bytes);
            println!("read_back_bytes         : {}", s.read_back_bytes);
            println!("logged_bytes            : {}", s.logged_bytes);
            println!("replay_bytes            : {}", s.replay_bytes);
            println!("reference_bytes         : {}", s.reference_bytes);
            println!(
                "display_batches         : {}（收到 {} / 发出 {}）",
                s.batches_received, s.batches_received, s.batches_emitted
            );
            println!("display_bytes_received  : {}", s.display_bytes_received);
            match s.first_diff {
                Some(off) => println!("✗ 逐字节比对失败：首个不一致偏移 {off}"),
                None => println!("✓ 逐字节比对：回放内容与写出的数据完全一致"),
            }
            println!(
                "fatal_error             : {}",
                s.fatal_error.as_deref().unwrap_or("none")
            );
            if s.consistent(total_bytes) {
                println!(
                    "✓ 回环闭环通过：写入 == 读回 == 落盘 == 回放 == {total_bytes} 字节（会话：源 + 管线 + 发送）"
                );
                ExitCode::SUCCESS
            } else {
                println!(
                    "✗ 回环闭环失败：写入 {} / 读回 {} / 落盘 {} / 回放 {}",
                    s.written_bytes, s.read_back_bytes, s.logged_bytes, s.replay_bytes
                );
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("loopback 失败: {e}");
            ExitCode::FAILURE
        }
    }
}

/// 渲染指纹报告（纯函数，便于对输出内容做断言）。
fn format_fingerprint(fp: &embhub_core::FirmwareFingerprint) -> String {
    let mut out = String::new();
    out.push_str("== embhub fingerprint（SPEC 3.5）==\n");
    out.push_str(&format!("repo_root : {}\n", show_field(&fp.repo_root)));
    out.push_str(&format!("commit    : {}\n", show_field(&fp.commit)));
    out.push_str(&format!("dirty     : {}\n", show_field(&fp.dirty)));
    out.push_str(&format!("bin_path  : {}\n", show_field(&fp.bin_path)));
    out.push_str(&format!("bin_hash  : {}\n", show_field(&fp.bin_hash)));
    out.push_str(&format!("build_time: {}\n", show_field(&fp.build_time)));
    out.push_str(&format!("chip      : {}\n", show_field(&fp.chip)));
    out.push_str(&format!("probe_id  : {}\n", show_field(&fp.probe_id)));

    let missing = fp.missing();
    if missing.is_empty() {
        out.push_str("✓ 8 个字段全部采集到\n");
    } else {
        out.push_str(&format!(
            "! {} 个字段取不到（逐条原因见上）:\n",
            missing.len()
        ));
        for (name, reason) in &missing {
            out.push_str(&format!("  - {name}: {reason}\n"));
        }
    }
    out
}

/// `fingerprint --repo <dir> [--bin <file>]`：按 SPEC 3.5 采集并打印固件指纹。
fn cmd_fingerprint(args: &[String]) -> ExitCode {
    let repo = match arg_value(args, "--repo") {
        Some(r) => PathBuf::from(r),
        None => {
            eprintln!("用法: embhub-cli fingerprint --repo <dir> [--bin <file>]");
            return ExitCode::FAILURE;
        }
    };
    let bin = arg_value(args, "--bin").map(PathBuf::from);

    let fp = fingerprint::collect(&repo, bin.as_deref());
    print!("{}", format_fingerprint(&fp));
    ExitCode::SUCCESS
}

/// `selftest --repo <dir> [--bin <file>]` 的指纹参数（不给 `--repo` 时整块为 None）。
struct FingerprintArgs {
    repo: PathBuf,
    bin: Option<PathBuf>,
}

fn cmd_selftest(args: &[String]) -> ExitCode {
    let total_bytes: u64 = match arg_value(args, "--bytes") {
        Some(v) => match v.parse() {
            Ok(n) => n,
            Err(e) => {
                eprintln!("--bytes 必须是整数: {v}（{e}）");
                return ExitCode::FAILURE;
            }
        },
        None => SELFTEST_DEFAULT_BYTES,
    };
    let seed: u64 = match arg_value(args, "--seed") {
        Some(v) => match v.parse() {
            Ok(n) => n,
            Err(e) => {
                eprintln!("--seed 必须是整数: {v}（{e}）");
                return ExitCode::FAILURE;
            }
        },
        None => SELFTEST_DEFAULT_SEED,
    };
    let fp_args = arg_value(args, "--repo").map(|repo| FingerprintArgs {
        repo: PathBuf::from(repo),
        bin: arg_value(args, "--bin").map(PathBuf::from),
    });

    match run_selftest(total_bytes, seed, fp_args.as_ref()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("selftest 失败: {e}");
            ExitCode::FAILURE
        }
    }
}

/// 临时会话根目录（进程唯一，避免并发运行互相踩）。`tag` 只用于人眼区分。
fn temp_session_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("embhub-{tag}-{}-{}", std::process::id(), nanos))
}

/// 分片文件字节总和（独立于 writer 侧账务的第二条落盘证据）。
fn segments_on_disk_bytes(dir: &Path) -> Result<u64, String> {
    let mut total = 0u64;
    for entry in fs::read_dir(dir).map_err(|e| format!("读目录 {} 失败: {e}", dir.display()))?
    {
        let entry = entry.map_err(|e| format!("读目录项失败: {e}"))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("seg-") && name.ends_with(".bin") {
            total += entry
                .metadata()
                .map_err(|e| format!("取 {name} 元数据失败: {e}"))?
                .len();
        }
    }
    Ok(total)
}

/// 把 `ByteSource` 变成「连续字节游标」：`fill` 尽量填满调用方缓冲，
/// 与来源自身的块边界无关（同一个来源两次读的块大小不必一致）。
struct SrcReader<'a, S: ByteSource> {
    src: &'a mut S,
    buf: Vec<u8>,
    pos: usize,
    len: usize,
    eof: bool,
}

impl<'a, S: ByteSource> SrcReader<'a, S> {
    fn new(src: &'a mut S, cap: usize) -> Self {
        Self {
            src,
            buf: vec![0u8; cap.max(1)],
            pos: 0,
            len: 0,
            eof: false,
        }
    }

    /// 返回实际填充的字节数；`0` 表示该来源已结束。
    fn fill(&mut self, out: &mut [u8]) -> Result<usize, SourceError> {
        let mut filled = 0;
        while filled < out.len() {
            if self.pos == self.len {
                if self.eof {
                    break;
                }
                let b = self.src.read(&mut self.buf)?;
                if b.len == 0 {
                    self.eof = true;
                    break;
                }
                self.pos = 0;
                self.len = b.len;
            }
            let n = (self.len - self.pos).min(out.len() - filled);
            out[filled..filled + n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            self.pos += n;
            filled += n;
        }
        Ok(filled)
    }
}

/// 两个字节流的比对结果。
struct StreamDiff {
    /// 参照序列（重新生成的注入序列）总字节数。
    expected_bytes: u64,
    /// 回放序列总字节数。
    actual_bytes: u64,
    /// 首个不一致偏移；长度分叉时也有值。
    first_diff: Option<u64>,
    /// 首个不一致偏移处两侧的字节（仅「同一偏移上字节不同」时有值）。
    diff_bytes: Option<(u8, u8)>,
}

/// 逐字节比对两条字节流。
///
/// 参照序列由一个**全新的**同种子 `SyntheticSource` 重新生成——比对基准不来自
/// 写入侧用过的缓冲区，而是来自「同一 (seed, offset) 必得同一字节」这条契约。
fn compare_streams<A: ByteSource, B: ByteSource>(
    expected: &mut A,
    replay: &mut B,
) -> Result<StreamDiff, String> {
    let mut want = SrcReader::new(expected, SELFTEST_REPLAY_CHUNK);
    let mut got = SrcReader::new(replay, SELFTEST_REPLAY_CHUNK);
    let mut buf_a = vec![0u8; COMPARE_BLOCK];
    let mut buf_b = vec![0u8; COMPARE_BLOCK];
    let (mut n_a, mut n_b) = (0u64, 0u64);
    let mut first_diff: Option<u64> = None;
    let mut diff_bytes: Option<(u8, u8)> = None;

    loop {
        let a = want
            .fill(&mut buf_a)
            .map_err(|e| format!("参照源读取失败: {e}"))?;
        let b = got
            .fill(&mut buf_b)
            .map_err(|e| format!("回放读取失败: {e}"))?;
        if a == 0 && b == 0 {
            break;
        }
        let k = a.min(b);
        if first_diff.is_none() {
            match buf_a[..k]
                .iter()
                .zip(buf_b[..k].iter())
                .position(|(x, y)| x != y)
            {
                Some(i) => {
                    first_diff = Some(n_b + i as u64);
                    diff_bytes = Some((buf_a[i], buf_b[i]));
                }
                None => {
                    if a != b {
                        // 一侧先结束：长度已经分叉，偏移落在短的那一侧之后。
                        first_diff = Some(n_b + k as u64);
                    }
                }
            }
        }
        n_a += a as u64;
        n_b += b as u64;
    }
    Ok(StreamDiff {
        expected_bytes: n_a,
        actual_bytes: n_b,
        first_diff,
        diff_bytes,
    })
}

/// 逐字段比对两次采集的指纹，返回不一致的字段清单（字段名 / 采集值 / 读回值）。
///
/// 「逐字段」是字面意思：8 个契约字段各自比一遍，而不是只比一个整体相等——
/// 这样报告能直接点出是哪个字段没往返成功。
fn diff_fingerprint(
    collected: &FirmwareFingerprint,
    read_back: &FirmwareFingerprint,
) -> Vec<(&'static str, String, String)> {
    let pairs: [(&'static str, String, String); 8] = [
        (
            "repo_root",
            format!("{}", collected.repo_root),
            format!("{}", read_back.repo_root),
        ),
        (
            "commit",
            format!("{}", collected.commit),
            format!("{}", read_back.commit),
        ),
        (
            "dirty",
            format!("{}", collected.dirty),
            format!("{}", read_back.dirty),
        ),
        (
            "bin_path",
            format!("{}", collected.bin_path),
            format!("{}", read_back.bin_path),
        ),
        (
            "bin_hash",
            format!("{}", collected.bin_hash),
            format!("{}", read_back.bin_hash),
        ),
        (
            "build_time",
            format!("{}", collected.build_time),
            format!("{}", read_back.build_time),
        ),
        (
            "chip",
            format!("{}", collected.chip),
            format!("{}", read_back.chip),
        ),
        (
            "probe_id",
            format!("{}", collected.probe_id),
            format!("{}", read_back.probe_id),
        ),
    ];
    pairs.into_iter().filter(|(_, a, b)| a != b).collect()
}

/// 跑一次完整的「注入 → 落盘 → 回放」闭环，可选带上「指纹写入会话头 → 读回比对」。
///
/// `Ok(true)` = 三个计数、逐字节比对、（若给了 `--repo`）指纹逐字段比对全过。
fn run_selftest(
    total_bytes: u64,
    seed: u64,
    fp_args: Option<&FingerprintArgs>,
) -> Result<bool, String> {
    let started = Instant::now();
    let root = temp_session_root("selftest");
    let cfg = SyntheticConfig {
        seed,
        total_bytes,
        chunk_size: SELFTEST_CHUNK_BYTES,
    };
    let source_config =
        format!("synthetic:seed={seed},total={total_bytes},chunk={SELFTEST_CHUNK_BYTES}");
    println!("== embhub selftest（零丢失闭环，无硬件）==");
    println!("会话目录      : {}", root.display());
    println!("来源配置      : {source_config}");
    println!("分片上限      : {SELFTEST_SEGMENT_BYTES} 字节");

    let cleanup = || {
        if let Err(e) = fs::remove_dir_all(&root) {
            eprintln!("提示: 清理 {} 失败: {e}", root.display());
        }
    };

    let result = (|| -> Result<bool, String> {
        // ---- 0. 指纹（可选）：采集一次，写进会话头，最后从会话头读回来比对 ----
        let collected = fp_args.map(|a| {
            println!("指纹来源      : --repo {}", a.repo.display());
            match &a.bin {
                Some(b) => println!("              : --bin  {}", b.display()),
                None => println!("              : 未给 --bin（bin_* 字段会记缺失+原因）"),
            }
            fingerprint::collect(&a.repo, a.bin.as_deref())
        });
        let mut meta = SessionMeta::new(source_config.clone());
        match &collected {
            Some(fp) => meta.fingerprint = FingerprintSection::collected(fp.clone()),
            None => {
                meta.fingerprint =
                    FingerprintSection::not_collected("selftest 未给 --repo，本次不采集指纹")
            }
        }

        let store = LogStore::new(&root, SELFTEST_SEGMENT_BYTES);
        let id = store
            .create_session(&meta)
            .map_err(|e| format!("create_session 失败: {e}"))?;
        let dir = root.join(&id.0);
        println!("会话 id       : {id}");

        // 会话头必须能被独立读回（magic / 版本 / 起始时刻 / 来源配置 / 指纹段）。
        let header =
            embhub_core::read_session_header(&dir).map_err(|e| format!("读会话头失败: {e}"))?;
        println!(
            "会话头        : magic=EMBHLOG\\0 版本={} 起始墙上时钟={}ms 来源配置={}",
            header.format_version, header.start_wall_ms, header.source_config
        );

        // ---- 1. 注入 ----
        let mut src = SyntheticSource::open(&cfg).map_err(|e| format!("打开合成源失败: {e}"))?;
        let mut buf = vec![0u8; SELFTEST_CHUNK_BYTES];
        let mut injected: u64 = 0;
        let mut records_written: u64 = 0;
        loop {
            let b = src
                .read(&mut buf)
                .map_err(|e| format!("注入侧读取失败: {e}"))?;
            if b.len == 0 {
                break;
            }
            store
                .append(
                    &id,
                    &Chunk {
                        t_mono: b.t_mono,
                        payload: &buf[..b.len],
                    },
                )
                .map_err(|e| format!("append 失败: {e}"))?;
            injected += b.len as u64;
            records_written += 1;
        }
        store.seal(&id).map_err(|e| format!("seal 失败: {e}"))?;

        // ---- 2. 落盘账务（writer 侧）+ 磁盘实测（独立证据）----
        let stats = store.stats(&id).map_err(|e| format!("stats 失败: {e}"))?;
        let disk_bytes = segments_on_disk_bytes(&dir)?;
        let expect_disk_bytes = stats.payload_bytes + stats.records * RECORD_HEADER_LEN;

        // ---- 3. 回放读回 + 逐字节比对 ----
        let mut replay = store
            .open_replay(&id)
            .map_err(|e| format!("open_replay 失败: {e}"))?;
        let mut reference =
            SyntheticSource::open(&cfg).map_err(|e| format!("打开参照合成源失败: {e}"))?;
        let diff = compare_streams(&mut reference, &mut replay)?;

        println!("注入字节数    : {injected}");
        println!(
            "落盘字节数    : {}（记录 {} 条 / 分片 {} 个 / 磁盘实测 {} 字节，含记录头应为 {}）",
            stats.payload_bytes, stats.records, stats.segments, disk_bytes, expect_disk_bytes
        );
        println!("回放读回字节数: {}", diff.actual_bytes);
        println!(
            "注入次数/参照重放注入字节数: {records_written} 次 / {} 字节",
            diff.expected_bytes
        );
        match (diff.first_diff, diff.diff_bytes) {
            (Some(off), Some((a, b))) => {
                println!("✗ 逐字节比对失败：首个不一致偏移 {off}（注入 {a:#04x}，回放 {b:#04x}）");
            }
            (Some(off), None) => {
                println!("✗ 逐字节比对失败：偏移 {off} 处两侧长度分叉");
            }
            (None, _) => println!("✓ 逐字节比对：回放与注入完全一致"),
        }

        // ---- 4. 指纹：从**落盘后**的会话头读回（不是内存里的那个副本）----
        let after_seal =
            embhub_core::read_session_header(&dir).map_err(|e| format!("读会话头失败: {e}"))?;
        let mut fingerprint_ok = true;
        match (&collected, &after_seal.fingerprint.collected) {
            (Some(fp), Some(back)) => {
                println!(
                    "== 会话头读回的固件指纹（v{}）==",
                    after_seal.format_version
                );
                print!("{}", format_fingerprint(back));
                let mismatches = diff_fingerprint(fp, back);
                if mismatches.is_empty() {
                    println!("✓ 指纹逐字段比对：会话头读回 == 采集值（8/8 字段）");
                } else {
                    fingerprint_ok = false;
                    println!("✗ 指纹逐字段比对失败（采集值 vs 读回值）:");
                    for (name, a, b) in &mismatches {
                        println!("  - {name}: 采集 {a} / 读回 {b}");
                    }
                }
            }
            (None, Some(_)) => {
                fingerprint_ok = false;
                println!("✗ 指纹比对失败：本次没给 --repo，会话头里却读出了指纹");
            }
            (Some(_), None) => {
                fingerprint_ok = false;
                let reason = after_seal
                    .fingerprint
                    .not_collected
                    .clone()
                    .unwrap_or_else(|| "（没有原因，违反不变量）".to_string());
                println!("✗ 指纹比对失败：采集了指纹，会话头里却是「未采集」: {reason}");
            }
            (None, None) => {
                let reason = after_seal
                    .fingerprint
                    .not_collected
                    .as_deref()
                    .unwrap_or("（没有原因，违反不变量）");
                println!("指纹          : 未采集（会话头记录的原因: {reason}）");
            }
        }

        let verdict_ok = verdict(
            injected,
            stats.payload_bytes,
            diff.actual_bytes,
            diff.expected_bytes,
            diff.first_diff.is_none(),
            stats.records == records_written && disk_bytes == expect_disk_bytes,
        ) && fingerprint_ok;

        if verdict_ok {
            println!(
                "✓ 零丢失闭环通过：注入 == 落盘 == 回放读回 == {} 字节（耗时 {:.3}s）",
                injected,
                started.elapsed().as_secs_f64()
            );
        } else {
            println!(
                "✗ 零丢失闭环失败：注入 {injected} / 落盘 {} / 回放 {} / 参照 {} / 逐字节 {} / 账务 {} / 指纹 {}",
                stats.payload_bytes,
                diff.actual_bytes,
                diff.expected_bytes,
                if diff.first_diff.is_none() { "一致" } else { "不一致" },
                if stats.records == records_written && disk_bytes == expect_disk_bytes {
                    "一致"
                } else {
                    "不一致"
                },
                if fingerprint_ok { "一致" } else { "不一致" }
            );
        }
        Ok(verdict_ok)
    })();

    cleanup();
    result
}

/// 闭环判定：**三个计数任一不等、逐字节比对不一致、或账务对不上，一律判失败**
/// （失败在 `cmd_selftest` 里映射成非零退出码）。
fn verdict(
    injected: u64,
    persisted: u64,
    replayed: u64,
    reference: u64,
    byte_identical: bool,
    bookkeeping_ok: bool,
) -> bool {
    injected == persisted
        && injected == replayed
        && injected == reference
        && byte_identical
        && bookkeeping_ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use embhub_core::Block;

    /// 脚本化来源：固定字节序列、固定块大小，用来喂 `compare_streams`。
    struct ScriptSource {
        data: Vec<u8>,
        pos: usize,
        chunk: usize,
        closed: bool,
    }

    impl ScriptSource {
        fn new(data: Vec<u8>, chunk: usize) -> Self {
            Self {
                data,
                pos: 0,
                chunk: chunk.max(1),
                closed: false,
            }
        }
    }

    impl ByteSource for ScriptSource {
        fn read(&mut self, buf: &mut [u8]) -> Result<Block, SourceError> {
            if self.closed {
                return Err(SourceError::Closed);
            }
            let n = (self.data.len() - self.pos).min(buf.len()).min(self.chunk);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(Block {
                len: n,
                t_mono: self.pos as u64,
            })
        }

        fn close(&mut self) {
            self.closed = true;
        }
    }

    /// 「不等必须判失败」：三个计数里任意一个、逐字节比对、账务，单独出问题都要 false。
    #[test]
    fn verdict_is_false_when_any_count_differs() {
        assert!(verdict(100, 100, 100, 100, true, true));
        assert!(
            !verdict(100, 99, 100, 100, true, true),
            "落盘少一字节必须失败"
        );
        assert!(
            !verdict(100, 100, 99, 100, true, true),
            "回放少一字节必须失败"
        );
        assert!(
            !verdict(100, 100, 100, 99, true, true),
            "参照基准不一致必须失败"
        );
        assert!(
            !verdict(100, 100, 100, 100, false, true),
            "逐字节不一致必须失败"
        );
        assert!(
            !verdict(100, 100, 100, 100, true, false),
            "账务对不上必须失败"
        );
        assert!(!verdict(0, 1, 0, 0, true, true));
        assert!(verdict(0, 0, 0, 0, true, true), "空输入是合法闭环");
    }

    #[test]
    fn compare_streams_reports_equal_streams() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let mut a = ScriptSource::new(data.clone(), 7);
        let mut b = ScriptSource::new(data, 4096);
        let d = compare_streams(&mut a, &mut b).unwrap();
        assert_eq!((d.expected_bytes, d.actual_bytes), (10_000, 10_000));
        assert_eq!(d.first_diff, None);
        assert_eq!(d.diff_bytes, None);
    }

    #[test]
    fn compare_streams_detects_flipped_byte() {
        let data: Vec<u8> = (0..5000u32).map(|i| (i % 249) as u8).collect();
        let mut tampered = data.clone();
        tampered[4097] ^= 0x01; // 落在第二个比对块里，验证偏移计算
        let mut a = ScriptSource::new(data, 512);
        let mut b = ScriptSource::new(tampered, 3000);
        let d = compare_streams(&mut a, &mut b).unwrap();
        assert_eq!((d.expected_bytes, d.actual_bytes), (5000, 5000));
        assert_eq!(d.first_diff, Some(4097), "必须报出准确的偏移");
        assert_eq!(d.diff_bytes.map(|(x, y)| x != y), Some(true));
    }

    #[test]
    fn compare_streams_detects_truncation() {
        let data: Vec<u8> = (0..5000u32).map(|i| (i % 249) as u8).collect();
        let mut a = ScriptSource::new(data.clone(), 512);
        let mut b = ScriptSource::new(data[..4000].to_vec(), 512);
        let d = compare_streams(&mut a, &mut b).unwrap();
        assert_eq!(
            (d.expected_bytes, d.actual_bytes),
            (5000, 4000),
            "两侧计数必须各自如实反映长度"
        );
        assert_eq!(d.first_diff, Some(4000), "长度分叉必须被检出");
    }

    #[test]
    fn src_reader_fill_ignores_source_chunk_boundaries() {
        let data: Vec<u8> = (0..100u32).map(|i| i as u8).collect();
        let mut src = ScriptSource::new(data.clone(), 3);
        let mut r = SrcReader::new(&mut src, 1);
        let mut out = [0u8; 64];
        assert_eq!(r.fill(&mut out).unwrap(), 64);
        assert_eq!(&out[..], &data[..64]);
        assert_eq!(r.fill(&mut out).unwrap(), 36, "最后一次只回剩余字节");
        assert_eq!(r.fill(&mut out).unwrap(), 0, "耗尽后返回 0");
    }

    #[test]
    fn selftest_empty_input_passes_through_the_cli_path() {
        let code = cmd_selftest(&["--bytes".to_string(), "0".to_string()]);
        assert_eq!(code, ExitCode::SUCCESS, "空输入必须是一次合法闭环");
    }

    #[test]
    fn selftest_small_run_is_byte_for_byte_clean() {
        let ok = run_selftest(64 * 1024, 42, None).unwrap();
        assert!(ok, "64 KiB 闭环必须通过");
    }

    /// 带 `--repo` 的端到端闭环：采集指纹 → 写进 v2 会话头 → 从会话头读回 → 逐字段比对。
    ///
    /// 这里用「git 认不出的目录 + 真实存在的 bin」，这样指纹里既有「有值」字段
    /// （bin_path/bin_hash/build_time），也有「带原因缺失」字段（git 三件套 + Phase 7 两件套），
    /// 两种形状都真的走出文件往返。真实仓库那一条由人工验收命令覆盖。
    #[test]
    fn selftest_with_repo_roundtrips_fingerprint_through_the_header() {
        let dir = non_repo_dir("selftest");
        let bin = dir.join("fw.bin");
        fs::write(&bin, b"firmware-bytes-for-selftest").unwrap();
        let args = FingerprintArgs {
            repo: dir.clone(),
            bin: Some(bin.clone()),
        };

        // 采集值与「会话头读回值」必须逐字段相等。
        let fp = fingerprint::collect(&dir, Some(&bin));
        assert!(fp.bin_hash.value().is_some(), "bin_hash 应有值");
        assert!(fp.commit.is_missing(), "坏仓库下 commit 应缺失带原因");

        let ok = run_selftest(64 * 1024, 42, Some(&args)).unwrap();
        assert!(ok, "带指纹的闭环必须通过");

        // 直接构造「读回值不同」的场景，确认 diff_fingerprint 真的能逐字段抓出差异。
        let mut mutated = fp.clone();
        mutated.commit = Field::Value("f".repeat(40));
        let diffs = diff_fingerprint(&fp, &mutated);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].0, "commit");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn selftest_without_repo_records_reason_in_header() {
        // 不给 --repo：行为与从前一致（仍 exit 0），但会话头必须写明「未采集 + 原因」。
        let ok = run_selftest(4096, 42, None).unwrap();
        assert!(ok);
    }

    #[test]
    fn selftest_bad_args_exit_nonzero() {
        let bad_bytes = cmd_selftest(&["--bytes".to_string(), "abc".to_string()]);
        assert_eq!(bad_bytes, ExitCode::FAILURE);
        let bad_seed = cmd_selftest(&[
            "--bytes".to_string(),
            "0".to_string(),
            "--seed".to_string(),
            "-1".to_string(),
        ]);
        assert_eq!(bad_seed, ExitCode::FAILURE);
    }

    /// 造一个「git 认不出仓库」的目录：临时目录本身在工作区仓库内，只有放一个
    /// 指向不存在路径的 `.git` 文件才能让 git 确定性地失败而不向上回退。
    fn non_repo_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "embhub-cli-fp-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".git"), b"gitdir: /embhub/definitely/not/here\n").unwrap();
        dir
    }

    // ---- send：参数解析与拒绝空数据 ----------------------------------------

    #[test]
    fn send_payload_parsing_accepts_hex_and_text() {
        assert_eq!(
            parse_send_payload("hex:AA55FF").unwrap(),
            vec![0xAA, 0x55, 0xFF]
        );
        assert_eq!(parse_send_payload("hex: aa 55 ").unwrap(), vec![0xAA, 0x55]);
        assert_eq!(parse_send_payload("hex:00").unwrap(), vec![0x00]);
        assert_eq!(parse_send_payload("text:hello").unwrap(), b"hello".to_vec());
        assert_eq!(
            parse_send_payload("text:你好").unwrap(),
            "你好".as_bytes().to_vec()
        );
    }

    #[test]
    fn send_payload_parsing_rejects_empty_and_garbage() {
        // 「不得静默发送空数据」：所有空/非法输入都必须 Err。
        for bad in [
            "hex:", "hex:   ", "hex:A", "hex:ABC", "hex:ZZ", "hex:AATT", "text:", "", "AA55",
            "ascii:AA", "hex",
        ] {
            assert!(
                parse_send_payload(bad).is_err(),
                "{bad:?} 必须被拒绝（不允许静默发送空/非法数据）"
            );
        }
    }

    #[test]
    fn send_append_terminators_are_exact() {
        assert_eq!(build_send_frame(b"hi", AppendEnd::None), b"hi".to_vec());
        assert_eq!(build_send_frame(b"hi", AppendEnd::Cr), b"hi\r".to_vec());
        assert_eq!(build_send_frame(b"hi", AppendEnd::Lf), b"hi\n".to_vec());
        assert_eq!(build_send_frame(b"hi", AppendEnd::Crlf), b"hi\r\n".to_vec());
        assert_eq!(AppendEnd::parse("crlf"), Some(AppendEnd::Crlf));
        assert_eq!(AppendEnd::parse("CRLF"), None, "大小写不认就是非法");
        assert_eq!(AppendEnd::parse("nl"), None);
    }

    #[test]
    fn send_bad_args_exit_nonzero() {
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // 缺 --port / 缺 --data
        assert_eq!(cmd_send(&s(&[])), ExitCode::FAILURE);
        assert_eq!(cmd_send(&s(&["--port", "NOPE"])), ExitCode::FAILURE);
        assert_eq!(cmd_send(&s(&["--data", "hex:AA55"])), ExitCode::FAILURE);
        // 非法 --data（这些必须在开端口之前就被拒）
        assert_eq!(
            cmd_send(&s(&["--port", "NOPE", "--data", "hex:"])),
            ExitCode::FAILURE
        );
        assert_eq!(
            cmd_send(&s(&["--port", "NOPE", "--data", "hello"])),
            ExitCode::FAILURE
        );
        // 非法 --append / --repeat
        assert_eq!(
            cmd_send(&s(&[
                "--port", "NOPE", "--data", "hex:AA55", "--append", "xyz"
            ])),
            ExitCode::FAILURE
        );
        assert_eq!(
            cmd_send(&s(&[
                "--port", "NOPE", "--data", "hex:AA55", "--repeat", "0"
            ])),
            ExitCode::FAILURE
        );
        assert_eq!(
            cmd_send(&s(&[
                "--port", "NOPE", "--data", "hex:AA55", "--repeat", "abc"
            ])),
            ExitCode::FAILURE
        );
        // 空 --port
        assert_eq!(
            cmd_send(&s(&["--port", "  ", "--data", "hex:AA55"])),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn send_to_missing_port_exits_nonzero_without_panic() {
        // 不依赖硬件：端口不存在 → 明确报错、非零退出（不是 panic，也不是「假装发了」）。
        assert_eq!(
            cmd_send(&[
                "--port".to_string(),
                "EMBHUB_NO_SUCH_PORT".to_string(),
                "--data".to_string(),
                "hex:AA55".to_string(),
            ]),
            ExitCode::FAILURE
        );
    }

    // ---- loopback：无硬件证明「能发」 --------------------------------------

    #[test]
    fn loopback_three_way_counts_are_equal() {
        let n = 64 * 1024;
        let s = run_loopback(n, 4096, 64).unwrap();
        assert_eq!(s.written_bytes, n, "写入");
        assert_eq!(s.read_back_bytes, n, "读回");
        assert_eq!(s.logged_bytes, n, "落盘");
        assert_eq!(s.replay_bytes, n, "回放");
        assert_eq!(s.reference_bytes, n);
        assert_eq!(s.first_diff, None, "回放内容必须与写出的完全一致");
        assert!(s.fatal_error.is_none());
        assert!(
            s.batches_received <= s.batches_emitted,
            "收到的批次数不可能多于发出的"
        );
        assert!(s.consistent(n));
        assert_eq!(
            cmd_loopback(&["--bytes".to_string(), n.to_string()]),
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn loopback_bad_args_exit_nonzero() {
        assert_eq!(cmd_loopback(&[]), ExitCode::FAILURE);
        assert_eq!(
            cmd_loopback(&["--bytes".to_string(), "abc".to_string()]),
            ExitCode::FAILURE
        );
        assert_eq!(
            cmd_loopback(&[
                "--bytes".to_string(),
                "1024".to_string(),
                "--chunk".to_string(),
                "x".to_string()
            ]),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn loopback_zero_bytes_is_a_clean_pass() {
        let s = run_loopback(0, 4096, 4).unwrap();
        assert_eq!(s.logged_bytes, 0);
        assert_eq!(s.replay_bytes, 0);
        assert!(s.consistent(0));
        assert_eq!(
            cmd_loopback(&["--bytes".to_string(), "0".to_string()]),
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn send_frame_repeat_counts_bytes_and_appends_terminator() {
        #[derive(Default)]
        struct RecordingSink {
            written: Vec<u8>,
            flushes: u32,
        }
        impl ByteSink for RecordingSink {
            fn write_all(&mut self, buf: &[u8]) -> Result<(), embhub_core::SinkError> {
                self.written.extend_from_slice(buf);
                Ok(())
            }
            fn flush(&mut self) -> Result<(), embhub_core::SinkError> {
                self.flushes += 1;
                Ok(())
            }
        }

        let mut sink = RecordingSink::default();
        let frame = build_send_frame(b"AT", AppendEnd::Crlf); // "AT\r\n" = 4 字节
        assert_eq!(frame, b"AT\r\n".to_vec());
        let n = write_frame_repeatedly(&mut sink, &frame, 3).unwrap();
        assert_eq!(n, 12, "3 次 × 4 字节");
        assert_eq!(
            sink.written,
            b"AT\r\nAT\r\nAT\r\n".to_vec(),
            "终结符每次都带"
        );
        assert_eq!(sink.flushes, 3, "每帧都要 flush");
    }

    #[test]
    fn send_frame_failure_reports_progress_and_does_not_panic() {
        struct FailingSink;
        impl ByteSink for FailingSink {
            fn write_all(&mut self, _buf: &[u8]) -> Result<(), embhub_core::SinkError> {
                Err(SourceError::Fatal("设备已拔出（测试用）".to_string()))
            }
            fn flush(&mut self) -> Result<(), embhub_core::SinkError> {
                Ok(())
            }
        }
        let err = write_frame_repeatedly(&mut FailingSink, b"AB", 2).unwrap_err();
        assert!(err.contains("第 1 次"), "错误必须点明第几次: {err}");
        assert!(err.contains("已发 0 字节"), "错误必须带上进度: {err}");
        assert!(err.contains("设备已拔出"));
    }

    /// 零字节也要能干净收场（不能因为「等数据」卡死）。
    #[test]
    fn stream_zero_bytes_exits_zero_and_does_not_hang() {
        let s = run_stream(0, 4096, ConsumeMode::Fast, 4).unwrap();
        assert_eq!((s.source_bytes, s.logged_bytes, s.replay_bytes), (0, 0, 0));
        assert_eq!(s.stop_reason, StopReason::BytesReached);
        assert!(s.disk_path_ok(0));
        assert_eq!(
            cmd_stream(&["--bytes".to_string(), "0".to_string()]),
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn stream_requires_bytes_flag() {
        assert_eq!(cmd_stream(&[]), ExitCode::FAILURE);
        assert_eq!(
            cmd_stream(&["--bytes".to_string(), "abc".to_string()]),
            ExitCode::FAILURE
        );
        assert_eq!(
            cmd_stream(&[
                "--bytes".to_string(),
                "1024".to_string(),
                "--consume".to_string(),
                "sprint".to_string(),
            ]),
            ExitCode::FAILURE,
            "非法的 --consume 必须非零退出"
        );
    }

    /// 消费者停住：显示大批丢弃，但**退出码仍然是 0**（落盘零丢失）。
    #[test]
    fn stream_stalled_consumer_drops_display_but_exits_zero() {
        let s = run_stream(256 * 1024, 4096, ConsumeMode::Stalled, 2).unwrap();
        assert_eq!(s.source_bytes, 256 * 1024);
        assert_eq!(s.logged_bytes, 256 * 1024);
        assert_eq!(s.replay_bytes, 256 * 1024);
        assert!(s.batches_dropped > 0, "停住的消费者必须造成显示丢弃: {s:?}");
        assert!(s.display_bytes_dropped > 0);
        assert_eq!(
            s.display_bytes_emitted + s.display_bytes_dropped,
            256 * 1024,
            "显示侧「发出 + 丢弃」必须等于源产出"
        );
        assert!(s.disk_path_ok(256 * 1024));
        assert_eq!(
            cmd_stream(&[
                "--bytes".to_string(),
                "256000".to_string(),
                "--chunk".to_string(),
                "4096".to_string(),
                "--consume".to_string(),
                "stalled".to_string(),
                "--display-capacity".to_string(),
                "2".to_string(),
            ]),
            ExitCode::SUCCESS,
            "显示丢弃不得影响退出码"
        );
    }

    /// 全速消费者：不丢，且仍零丢失。
    #[test]
    fn stream_fast_consumer_drops_nothing_and_exits_zero() {
        let s = run_stream(256 * 1024, 8192, ConsumeMode::Fast, 1024).unwrap();
        assert_eq!(s.batches_dropped, 0, "全速消费者不该丢: {s:?}");
        assert_eq!(s.display_bytes_dropped, 0);
        assert_eq!(
            s.consumer_bytes,
            256 * 1024,
            "消费者收到的字节数必须等于源产出"
        );
        assert_eq!(s.consumer_batches, s.batches_emitted);
        assert!(s.disk_path_ok(256 * 1024));
        assert!(matches!(s.stop_reason, StopReason::BytesReached));
    }

    /// 慢消费者：既丢也仍然零丢失（演示「显示可以丢」不是「数据可以丢」）。
    #[test]
    fn stream_slow_consumer_keeps_disk_path_intact() {
        let s = run_stream(256 * 1024, 4096, ConsumeMode::Slow, 2).unwrap();
        assert_eq!(s.logged_bytes, 256 * 1024);
        assert_eq!(s.replay_bytes, 256 * 1024);
        assert!(s.disk_path_ok(256 * 1024));
    }

    #[test]
    fn fingerprint_requires_repo_flag() {
        assert_eq!(cmd_fingerprint(&[]), ExitCode::FAILURE);
    }

    #[test]
    fn fingerprint_report_prints_all_keys_and_never_silently_omits() {
        let dir = non_repo_dir("broken");
        let args = vec![
            "--repo".to_string(),
            dir.display().to_string(),
            "--bin".to_string(),
            dir.join("no-such-fw.bin").display().to_string(),
        ];
        assert_eq!(
            cmd_fingerprint(&args),
            ExitCode::SUCCESS,
            "采集不到字段不算命令失败"
        );

        let fp = fingerprint::collect(&dir, Some(&dir.join("no-such-fw.bin")));
        let report = format_fingerprint(&fp);
        for key in [
            "repo_root :",
            "commit    :",
            "dirty     :",
            "bin_path  :",
            "bin_hash  :",
            "build_time:",
            "chip      :",
            "probe_id  :",
        ] {
            assert!(report.contains(key), "报告缺少字段 {key}:\n{report}");
        }
        assert!(
            report.contains("取不到（原因"),
            "缺失字段必须打印原因:\n{report}"
        );
        for name in ["repo_root", "commit", "dirty", "bin_hash"] {
            assert!(
                report.contains(&format!("  - {name}: ")),
                "缺失清单里必须点名 {name}:\n{report}"
            );
        }
        fs::remove_dir_all(&dir).ok();
    }
}
