//! SPEC 3.4 —— `Session`：把一个「来源 + 落盘 + 发送」绑成一个可启停的单元。
//!
//! 本 phase 只做**最小实现**，接口就是五个操作：
//!
//! | 操作 | 含义 |
//! | --- | --- |
//! | [`Session::start`] | 起流式管线（常驻读取线程 + 落盘 + 显示旁路） |
//! | [`Session::stop`] | 停管线（join 读取线程）、`seal` 会话；**幂等** |
//! | [`Session::stats`] | 计数快照（落盘/显示 + **tx 计数**）；停止后仍可读 |
//! | [`Session::recv_batch`] | 从有界显示通道取一批（显示可以丢，落盘不会） |
//! | [`Session::send`] | 通过 [`ByteSink`] 发字节，并记 tx 计数 |
//!
//! 几条写死在实现里的语义：
//!
//! - **不得出现僵尸读取线程**：`stop()` 先 join 读取线程、再 `seal`，所以
//!   `stop()` 返回之后不可能再有线程往已关闭的 store 写。`Drop` 也会兜底 stop。
//! - **`stop()` 幂等**：重复调用不 panic、不重复报错；`stats()` 在停止后仍可读
//!   （最终计数不会因为「会话关了」而看不到）。
//! - **发送失败不 panic**：错误记进 `tx_errors` / `tx_last_error` 并作为 `Err` 返回，
//!   会话本身继续可用（串口拔掉不该把整个采集会话干掉）。
//!
//! 明确**未做**（留给 Phase 2，且不假装做了）：
//!
//! - SPEC 3.4 的 `Stopped → Running` 重启：本实现里来源在线程启动时就被移交给
//!   管线，无法重新开始；重启 = 新建 `Session`。第二次 `start()` 会明确报错。
//! - SPEC 3.4 的「同一物理端口不得被两个 Session 同时持有」校验（需要端口占用表）。
//! - `.embproj` 序列化、`config_snapshot`、指纹采集（指纹由调用方采集后放进
//!   [`SessionMeta`]，见 [`crate::fingerprint`]）。

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::pipeline::{DisplayBatch, Pipeline, PipelineConfig, PipelineStats};
use crate::source::{ByteSink, ByteSource, MonoMicros};
use crate::store::{LogStore, SessionId, SessionMeta, SessionStart, StoreError};

/// 会话操作错误。把「没启动」与「启动后失败」分开，调用方不必靠字符串判断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// 会话还没 `start`（或 `start` 失败后从未成功）。
    NotStarted,
    /// 已经启动过：本最小实现不支持重启（重启 = 新建 Session）。
    AlreadyStarted,
    /// 管线/落盘侧失败。
    Failed(String),
    /// 发送侧失败（没有发送通道、写失败、flush 失败、拒绝空发送）。
    Send(String),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::NotStarted => write!(f, "会话尚未 start"),
            SessionError::AlreadyStarted => {
                write!(f, "会话已经启动过（本实现不支持重启，请新建会话）")
            }
            SessionError::Failed(msg) => write!(f, "会话失败: {msg}"),
            SessionError::Send(msg) => write!(f, "发送失败: {msg}"),
        }
    }
}

impl std::error::Error for SessionError {}

/// 发送侧计数（跨线程可读；停止后仍然保留）。
#[derive(Debug, Default)]
struct TxCounters {
    bytes: AtomicU64,
    writes: AtomicU64,
    errors: AtomicU64,
    last_error: Mutex<Option<String>>,
}

/// 会话计数快照。`pipeline` 是落盘/显示侧的账，`tx_*` 是发送侧的账。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSnapshot {
    /// 是否已经 stop 过。
    pub stopped: bool,
    /// 会话创建时刻的 `(墙上时钟, 单调时钟)`——把到达时刻换算成墙上时间的**精确**基准。
    pub start: SessionStart,
    /// 管线侧计数（source/logged/batches/dropped/fatal_error…）。
    pub pipeline: PipelineStats,
    /// 成功写出的字节总数。
    pub tx_bytes: u64,
    /// 成功写出的次数（一次 `send` 算一次）。
    pub tx_writes: u64,
    /// 失败次数。
    pub tx_errors: u64,
    /// 最近一次发送失败的原因（成功不覆盖）。
    pub tx_last_error: Option<String>,
}

impl SessionSnapshot {
    /// 由到达时刻（`Block::t_mono`）精确算出墙上时钟（Unix epoch 毫秒）。
    ///
    /// 基准是**会话创建点**（`self.start`），不是第一批到达时刻；所以会话打开后
    /// 即便静默很久才来数据，第一条数据的时间戳也不会带上那段沉默的偏移。
    pub fn wall_time_ms_for(&self, t_mono: MonoMicros) -> u64 {
        self.start.wall_time_ms_for(t_mono)
    }
}

struct Running {
    pipeline: Pipeline,
    stopped: bool,
}

/// 一个可启停的采集会话。
pub struct Session {
    store: Arc<LogStore>,
    meta: SessionMeta,
    /// 来源：`start()` 时移交给管线线程。
    source: Mutex<Option<Box<dyn ByteSource + Send>>>,
    /// 发送句柄：留给 `send()`；与读侧相互独立（串口用 `try_clone` 得到）。
    sink: Option<Mutex<Box<dyn ByteSink + Send>>>,
    pipeline_cfg: PipelineConfig,
    running: Mutex<Option<Running>>,
    /// 显示通道接收端（`start()` 时从管线取走，只能取一次）。
    display_rx: Mutex<Option<Receiver<DisplayBatch>>>,
    tx: TxCounters,
}

impl Session {
    /// 组装会话（不启动）。发送通道可不给（只采集不发）。
    pub fn new(
        store: Arc<LogStore>,
        meta: SessionMeta,
        source: Box<dyn ByteSource + Send>,
        sink: Option<Box<dyn ByteSink + Send>>,
        pipeline_cfg: PipelineConfig,
    ) -> Self {
        Self {
            store,
            meta,
            source: Mutex::new(Some(source)),
            sink: sink.map(Mutex::new),
            pipeline_cfg,
            running: Mutex::new(None),
            display_rx: Mutex::new(None),
            tx: TxCounters::default(),
        }
    }

    /// 起管线：建会话（写会话头）→ 起常驻读取线程 → 落盘 + 显示旁路。
    ///
    /// 只能成功一次；第二次返回 [`SessionError::AlreadyStarted`]。
    pub fn start(&self) -> Result<(), SessionError> {
        let mut running = self.running.lock().unwrap();
        if running.is_some() {
            return Err(SessionError::AlreadyStarted);
        }
        let source = self
            .source
            .lock()
            .unwrap()
            .take()
            .ok_or(SessionError::AlreadyStarted)?;
        let pipeline = Pipeline::spawn(
            Arc::clone(&self.store),
            self.meta.clone(),
            source,
            self.pipeline_cfg.clone(),
        )
        .map_err(|e: StoreError| SessionError::Failed(format!("起管线失败: {e}")))?;

        // 显示接收端拿到会话手里：`recv_batch` 用它，且它不与管线锁交叉。
        if let Some(rx) = pipeline.take_display_receiver() {
            *self.display_rx.lock().unwrap() = Some(rx);
        }
        *running = Some(Running {
            pipeline,
            stopped: false,
        });
        Ok(())
    }

    /// 停止：join 读取线程并 `seal` 会话。**幂等**。
    ///
    /// 返回 `Ok` 之后不可能再有读取线程写这个会话（这就是「没有僵尸线程」的保证）。
    pub fn stop(&self) -> Result<(), SessionError> {
        let mut running = self.running.lock().unwrap();
        let r = running.as_mut().ok_or(SessionError::NotStarted)?;
        // Pipeline::stop 本身幂等（join 一次 + seal 幂等），所以重复 stop 也安全。
        r.pipeline
            .stop()
            .map_err(|e| SessionError::Failed(format!("停止会话失败（seal）: {e}")))?;
        r.stopped = true;
        Ok(())
    }

    /// 计数快照。**停止后仍可读**（最终计数不会因为会话关了而消失）。
    pub fn stats(&self) -> Result<SessionSnapshot, SessionError> {
        let running = self.running.lock().unwrap();
        let r = running.as_ref().ok_or(SessionError::NotStarted)?;
        Ok(SessionSnapshot {
            stopped: r.stopped,
            start: r.pipeline.session_start(),
            pipeline: r.pipeline.stats(),
            tx_bytes: self.tx.bytes.load(Ordering::Relaxed),
            tx_writes: self.tx.writes.load(Ordering::Relaxed),
            tx_errors: self.tx.errors.load(Ordering::Relaxed),
            tx_last_error: self.tx.last_error.lock().unwrap().clone(),
        })
    }

    /// 从显示通道取一批。`timeout` 内没有数据返回 `Ok(None)`——
    /// 「暂时没数据」与「通道已关闭」都不是错误。
    pub fn recv_batch(&self, timeout: Duration) -> Result<Option<DisplayBatch>, SessionError> {
        let guard = self.display_rx.lock().unwrap();
        let rx = guard.as_ref().ok_or(SessionError::NotStarted)?;
        match rx.recv_timeout(timeout) {
            Ok(batch) => Ok(Some(batch)),
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => Ok(None),
        }
    }

    /// 发送一批字节（写 + flush），并记 tx 计数。
    ///
    /// 拒绝空发送：空 `bytes` 返回 `Err`（`--data` 解析成空却不报错，正是要防的事）。
    /// 写失败把原因记进 `tx_last_error` 并返回 `Err`，**不 panic**，会话继续可用。
    pub fn send(&self, bytes: &[u8]) -> Result<usize, SessionError> {
        if bytes.is_empty() {
            return Err(self.record_tx_error("拒绝发送 0 字节".to_string()));
        }
        let Some(sink) = self.sink.as_ref() else {
            return Err(self.record_tx_error("本会话没有发送通道（sink）".to_string()));
        };
        let mut guard = sink.lock().unwrap();
        let outcome = guard.write_all(bytes).and_then(|()| guard.flush());
        match outcome {
            Ok(()) => {
                self.tx
                    .bytes
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                self.tx.writes.fetch_add(1, Ordering::Relaxed);
                Ok(bytes.len())
            }
            Err(e) => Err(self.record_tx_error(e.to_string())),
        }
    }

    /// 会话 id（`start()` 之后才有）。
    pub fn session_id(&self) -> Option<SessionId> {
        let running = self.running.lock().unwrap();
        running.as_ref().map(|r| r.pipeline.session_id().clone())
    }

    /// 读取线程是否还在跑（`start()` 之前为 false）。
    pub fn is_running(&self) -> bool {
        let running = self.running.lock().unwrap();
        running.as_ref().is_some_and(|r| r.pipeline.is_running())
    }

    /// 等管线自然结束（读满 `stop_after_bytes` / 来源关闭 / 出错），至多 `timeout`。
    pub fn wait_until_finished(&self, timeout: Duration) -> bool {
        let running = self.running.lock().unwrap();
        match running.as_ref() {
            Some(r) => r.pipeline.wait_until_finished(timeout),
            None => false,
        }
    }

    fn record_tx_error(&self, msg: String) -> SessionError {
        self.tx.errors.fetch_add(1, Ordering::Relaxed);
        *self.tx.last_error.lock().unwrap() = Some(msg.clone());
        SessionError::Send(msg)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // 兜底：忘 stop 也不能把读取线程留在那儿。Session 可能从未启动，忽略错误。
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{mono_now, SinkError, SourceError, SyntheticSource};
    use crate::store::LogStore;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "embhub-session-test-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 写入永远失败的 sink：验证「写失败不 panic、错误被记录」。
    struct FailingSink;

    impl ByteSink for FailingSink {
        fn write_all(&mut self, _buf: &[u8]) -> Result<(), SinkError> {
            Err(SourceError::Fatal("设备已拔出（测试用）".to_string()))
        }
        fn flush(&mut self) -> Result<(), SinkError> {
            Ok(())
        }
    }

    fn replay_all(store: &LogStore, id: &SessionId) -> Vec<u8> {
        let mut r = store.open_replay(id).unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let b = r.read(&mut buf).unwrap();
            if b.len == 0 {
                break;
            }
            out.extend_from_slice(&buf[..b.len]);
        }
        out
    }

    /// 会话起点必须是**创建点**，不能拿「第一批到达时间」冒充。
    ///
    /// 判据：建会话 → 故意静默 30ms → 才发数据；如果实现用首批 t_mono 当起点，
    /// 那么 `首批 t_mono - start.mono_micros` 会塌成 ~0；正确实现里这段差值
    /// 必须 ≥ 静默时长（也就是偏移里真的含了那段沉默）。
    #[test]
    fn session_start_is_creation_point_not_first_batch() {
        let root = temp_root("start-point");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        let src = SyntheticSource::loopback_with_chunk(1024);
        let sink = src.try_clone_sink().unwrap();
        let session = Session::new(
            Arc::clone(&store),
            SessionMeta::new("loopback:start-point"),
            Box::new(src),
            Some(Box::new(sink)),
            PipelineConfig {
                batch_bytes: 1024,
                stop_after_bytes: Some(4096),
                poll_when_empty: Duration::from_millis(1),
                ..PipelineConfig::default()
            },
        );
        let before_create = mono_now();
        session.start().unwrap();
        let after_create = mono_now();

        let snap = session.stats().unwrap();
        let start = snap.start;
        assert!(start.wall_ms > 0, "墙上时钟必须有效");
        // start.mono 必须落在「start() 前后两次取样」之间 —— 证明它取自创建点。
        assert!(
            start.mono_micros >= before_create && start.mono_micros <= after_create,
            "start.mono_micros={} 不在 [{before_create}, {after_create}] 之内，说明不是创建点取的",
            start.mono_micros
        );
        // 与落盘会话头里的起始墙上时钟必须是同一个值。
        let id = session.session_id().unwrap();
        let header = crate::store::read_session_header(&root.join(&id.0)).unwrap();
        assert_eq!(
            start.wall_ms, header.start_wall_ms,
            "会话头与快照必须是同一个起点"
        );

        // 故意静默 30ms 再发数据。
        std::thread::sleep(Duration::from_millis(30));
        session.send(&vec![0x5A; 4096]).unwrap();
        let first = loop {
            if let Some(batch) = session.recv_batch(Duration::from_secs(2)).unwrap() {
                break batch;
            }
        };
        let offset = first.t_mono - start.mono_micros;
        assert!(
            offset >= 25_000,
            "首批 t_mono 距会话起点只有 {offset}µs，说明 start 是用首批到达时间近似的（静默 30ms 应体现在偏移里）"
        );
        // 墙上时间换算：起点自身必须精确等于 wall_ms，且随 t_mono 单调不减。
        assert_eq!(snap.wall_time_ms_for(start.mono_micros), start.wall_ms);
        assert_eq!(
            snap.wall_time_ms_for(first.t_mono),
            start.wall_ms + offset / 1000,
            "由 t_mono 换算墙上时间必须按定义精确成立"
        );
        assert!(snap.wall_time_ms_for(first.t_mono) >= start.wall_ms);
        // 早于起点的 t_mono 退化为 wall_ms（不下溢、不 panic）。
        assert_eq!(snap.wall_time_ms_for(0), start.wall_ms);

        session.stop().unwrap();
        // 停止后快照仍在，且 start 不变（最终计数里也能换算时间）。
        let after_stop = session.stats().unwrap();
        assert_eq!(after_stop.start, start);
        assert!(after_stop.stopped);
        std::fs::remove_dir_all(&root).ok();
    }

    /// 每个会话各自取自己的起点；两个会话的起点必须不同、且后者不早于前者。
    #[test]
    fn each_session_has_its_own_start_point() {
        let root = temp_root("two-starts");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        let mut picks = Vec::new();
        for tag in ["a", "b"] {
            let src = SyntheticSource::loopback();
            let sink = src.try_clone_sink().unwrap();
            let session = Session::new(
                Arc::clone(&store),
                SessionMeta::new(format!("loopback:{tag}")),
                Box::new(src),
                Some(Box::new(sink)),
                PipelineConfig {
                    stop_after_bytes: Some(0),
                    ..PipelineConfig::default()
                },
            );
            session.start().unwrap();
            picks.push(session.stats().unwrap().start);
            session.stop().unwrap();
        }
        assert!(
            picks[1].mono_micros >= picks[0].mono_micros,
            "后建的会话起点不应早于先建的"
        );
        assert!(picks[1].wall_ms >= picks[0].wall_ms);
        // 未知会话不会给出一个假起点。
        assert!(store.session_start(&SessionId("nope".into())).is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    /// 回环会话：写进去的字节经「源 → 管线 → 落盘」回来，三方计数一致。
    #[test]
    fn loopback_session_roundtrip_is_lossless() {
        let root = temp_root("roundtrip");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        let src = SyntheticSource::loopback_with_chunk(16 * 1024);
        let sink = src.try_clone_sink().unwrap();
        let total: usize = 200_000;
        let session = Session::new(
            Arc::clone(&store),
            SessionMeta::new("loopback:test"),
            Box::new(src),
            Some(Box::new(sink)),
            PipelineConfig {
                stop_after_bytes: Some(total as u64),
                poll_when_empty: Duration::from_millis(1),
                ..PipelineConfig::default()
            },
        );

        // 没 start 之前：stats/send 都该明确报错，而不是 panic。
        assert_eq!(session.stats(), Err(SessionError::NotStarted));
        session.start().unwrap();
        // 起两次要明确报错（本实现不支持重启）。
        assert_eq!(session.start(), Err(SessionError::AlreadyStarted));

        let payload: Vec<u8> = (0..total).map(|i| (i * 7 % 251) as u8).collect();
        for chunk in payload.chunks(8192) {
            session.send(chunk).unwrap();
        }
        assert!(
            session.wait_until_finished(Duration::from_secs(30)),
            "管线没在 30s 内读满"
        );
        session.stop().unwrap();

        let st = session.stats().unwrap();
        assert!(st.stopped);
        assert_eq!(st.tx_bytes, total as u64, "发送字节数");
        assert_eq!(st.tx_writes, (total as u64).div_ceil(8192));
        assert_eq!(st.tx_errors, 0);
        assert_eq!(st.pipeline.source_bytes, total as u64, "读回字节数");
        assert_eq!(st.pipeline.logged_bytes, total as u64, "落盘字节数");
        assert!(st.pipeline.fatal_error.is_none());

        // 落盘内容必须与写进去的完全一致（不是只对计数）。
        let id = session.session_id().unwrap();
        assert_eq!(replay_all(&store, &id), payload);
        std::fs::remove_dir_all(&root).ok();
    }

    /// stop 幂等 + 没有僵尸读取线程继续写。
    #[test]
    fn stop_is_idempotent_and_leaves_no_zombie_writer() {
        let root = temp_root("zombie");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        let src = SyntheticSource::loopback_with_chunk(1024);
        let sink = src.try_clone_sink().unwrap();
        let session = Session::new(
            Arc::clone(&store),
            SessionMeta::new("loopback:zombie"),
            Box::new(src),
            Some(Box::new(sink)),
            PipelineConfig {
                stop_after_bytes: Some(10_000),
                poll_when_empty: Duration::from_millis(1),
                ..PipelineConfig::default()
            },
        );
        session.start().unwrap();
        session.send(&[0x5A; 10_000]).unwrap();
        assert!(session.wait_until_finished(Duration::from_secs(10)));
        session.stop().unwrap();

        let first = session.stats().unwrap();
        assert_eq!(first.pipeline.logged_bytes, 10_000);
        // 重复 stop：不 panic、不报错。
        session.stop().unwrap();
        session.stop().unwrap();
        assert!(!session.is_running(), "stop 之后读取线程必须已经结束");

        // 僵尸判据：等一会儿，落盘字节数不许再动。
        std::thread::sleep(Duration::from_millis(80));
        let later = session.stats().unwrap();
        assert_eq!(later.pipeline.logged_bytes, first.pipeline.logged_bytes);
        assert_eq!(later.tx_bytes, first.tx_bytes);

        // 停止即 seal：不能再写；但统计仍可读。
        let id = session.session_id().unwrap();
        assert!(matches!(
            store.append(
                &id,
                &crate::store::Chunk {
                    t_mono: 1,
                    payload: b"x"
                }
            ),
            Err(StoreError::Sealed)
        ));
        assert_eq!(store.stats(&id).unwrap().payload_bytes, 10_000);
        std::fs::remove_dir_all(&root).ok();
    }

    /// 写失败：返回 Err、错误被记录、不 panic，会话仍可继续用/停止。
    #[test]
    fn send_failure_is_recorded_and_does_not_panic() {
        let root = temp_root("sendfail");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        let session = Session::new(
            store,
            SessionMeta::new("test:failingsink"),
            Box::new(SyntheticSource::loopback()),
            Some(Box::new(FailingSink)),
            PipelineConfig {
                stop_after_bytes: Some(0),
                ..PipelineConfig::default()
            },
        );
        session.start().unwrap();
        let err = session.send(b"hello").unwrap_err();
        assert!(matches!(err, SessionError::Send(ref m) if m.contains("设备已拔出")));
        let st = session.stats().unwrap();
        assert_eq!(st.tx_errors, 1);
        assert_eq!(st.tx_bytes, 0, "失败不能算进 tx_bytes");
        assert!(st.tx_last_error.as_deref().unwrap().contains("设备已拔出"));
        // 会话还能正常停。
        session.stop().unwrap();
        assert!(session.stats().unwrap().stopped);
    }

    /// 空发送被拒绝，且计入 errors（不允许「静默发送空数据」）。
    #[test]
    fn empty_send_is_rejected() {
        let root = temp_root("empty-send");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        let src = SyntheticSource::loopback();
        let sink = src.try_clone_sink().unwrap();
        let session = Session::new(
            store,
            SessionMeta::new("loopback:empty"),
            Box::new(src),
            Some(Box::new(sink)),
            PipelineConfig {
                stop_after_bytes: Some(0),
                ..PipelineConfig::default()
            },
        );
        session.start().unwrap();
        let err = session.send(&[]).unwrap_err();
        assert!(matches!(err, SessionError::Send(ref m) if m.contains("0 字节")));
        assert_eq!(session.stats().unwrap().tx_errors, 1);
        session.stop().unwrap();
    }

    /// 没有发送通道时 send 明确报错（不是静默丢弃）。
    #[test]
    fn send_without_sink_is_an_error() {
        let root = temp_root("nosink");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        let session = Session::new(
            store,
            SessionMeta::new("test:nosink"),
            Box::new(SyntheticSource::loopback()),
            None,
            PipelineConfig {
                stop_after_bytes: Some(0),
                ..PipelineConfig::default()
            },
        );
        session.start().unwrap();
        let err = session.send(b"x").unwrap_err();
        assert!(matches!(err, SessionError::Send(ref m) if m.contains("没有发送通道")));
        session.stop().unwrap();
    }

    /// recv_batch：有数据时给出批次，空/关闭时给 `Ok(None)`（不是错误）。
    #[test]
    fn recv_batch_returns_batches_and_none_when_idle() {
        let root = temp_root("recv");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        let src = SyntheticSource::loopback_with_chunk(4096);
        let sink = src.try_clone_sink().unwrap();
        let session = Session::new(
            store,
            SessionMeta::new("loopback:recv"),
            Box::new(src),
            Some(Box::new(sink)),
            PipelineConfig {
                batch_bytes: 1024,
                display_capacity: 8,
                stop_after_bytes: Some(8192),
                poll_when_empty: Duration::from_millis(1),
                ..PipelineConfig::default()
            },
        );
        session.start().unwrap();
        session.send(&vec![0x33; 8192]).unwrap();
        let batch = loop {
            match session.recv_batch(Duration::from_secs(2)).unwrap() {
                Some(b) => break b,
                None => continue,
            }
        };
        assert!(
            batch.bytes.len() >= 1024,
            "攒到 batch_bytes 就该发一批（实际 {}）",
            batch.bytes.len()
        );
        assert_eq!(batch.bytes, vec![0x33; batch.bytes.len()]);
        let first_len = batch.bytes.len();
        assert!(session.wait_until_finished(Duration::from_secs(10)));
        session.stop().unwrap();
        // 排空后：Ok(None)，不是错误。
        let mut drained = 0usize;
        while let Some(b) = session.recv_batch(Duration::from_millis(50)).unwrap() {
            drained += b.bytes.len();
        }
        assert_eq!(
            drained + first_len,
            8192,
            "显示侧收到的字节数必须等于写入量"
        );
        assert_eq!(session.stats().unwrap().pipeline.logged_bytes, 8192);
        std::fs::remove_dir_all(&root).ok();
    }

    /// 会话头里的来源配置来自 SessionMeta（会话可回答「数据从哪来」）。
    #[test]
    fn session_header_records_source_config() {
        let root = temp_root("meta");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        let src = SyntheticSource::loopback();
        let sink = src.try_clone_sink().unwrap();
        let session = Session::new(
            Arc::clone(&store),
            SessionMeta::new("loopback:synthetic,chunk=1024"),
            Box::new(src),
            Some(Box::new(sink)),
            PipelineConfig {
                stop_after_bytes: Some(0),
                ..PipelineConfig::default()
            },
        );
        session.start().unwrap();
        session.stop().unwrap();
        let id = session.session_id().unwrap();
        let header = crate::store::read_session_header(&root.join(&id.0)).unwrap();
        assert_eq!(header.source_config, "loopback:synthetic,chunk=1024");
        assert_eq!(header.format_version, 2);
        std::fs::remove_dir_all(&root).ok();
    }
}
