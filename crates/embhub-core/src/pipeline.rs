//! PLAN 4.3 + SPEC 设计原则 4/5 —— 流式管线：常驻读取 + 批量推送 + 有界显示队列背压。
//!
//! 一条常驻线程从 [`ByteSource`] 取数，**在到达点**沿用来源打出的单调时钟读数
//! （不改写），并把每个块**同步写入 [`LogStore`]**；同时在旁路把字节按
//! 「≥ [`PipelineConfig::batch_bytes`]（默认 8 KiB）或 ≥
//! [`PipelineConfig::batch_timeout`]（默认 30 ms），先到先发」攒成批次，用
//! `try_send` 推入一个**有界**显示通道。
//!
//! 硬约束（SPEC 原则 4，术语表「零丢失」）：
//!
//! - **落盘路径永不丢**：`append` 返回 `Ok` 才继续；`append` 出错即视为致命，
//!   管线停下并把错误记进 [`PipelineStats::fatal_error`]。
//! - **显示可以丢，但必须报出来**：显示通道满（或消费者已走）时丢弃该批次并计数
//!   [`PipelineStats::batches_dropped`] / [`PipelineStats::display_bytes_dropped`]。
//! - **两条路径从采集点就分开**：显示通道用 `try_send`（永不阻塞），
//!   所以消费端卡住绝不会拖慢落盘——这正是「不允许共用一条会阻塞的队列」的落实。
//! - 串口/来源的阻塞读发生在**本管线自己的线程**里，绝不在 UI 线程。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::source::{ByteSource, MonoMicros, SourceError};
use crate::store::{Chunk, LogStore, SessionId, SessionMeta, SessionStart, StoreError};

/// 批量推送的字节阈值（PLAN 4.3：≥8 KB）。
pub const DEFAULT_BATCH_BYTES: usize = 8 * 1024;
/// 批量推送的时间阈值（PLAN 4.3：30–60 Hz 取上限）。
pub const DEFAULT_BATCH_TIMEOUT_MS: u64 = 30;
/// 有界显示通道的默认容量（单位是**批次**，不是字节）。
pub const DEFAULT_DISPLAY_CAPACITY: usize = 64;
/// 单次 `read` 的缓冲区大小。
pub const DEFAULT_READ_BUF: usize = 64 * 1024;
/// 来源「暂时无数据」时的轮询间隔（不是错误，见 SPEC 3.1）。
pub const DEFAULT_POLL_WHEN_EMPTY_MS: u64 = 2;

/// 管线配置。除 `stop_after_bytes` 外都是「先到先发」批量策略与队列容量的旋钮。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineConfig {
    /// 攒够这么多字节就发一批。
    pub batch_bytes: usize,
    /// 或者攒了这么久就发一批（哪怕没到字节阈值）。
    pub batch_timeout: Duration,
    /// 显示通道容量（批次）。满了就丢批次并计数。
    pub display_capacity: usize,
    /// 单次 `read` 的缓冲区大小。
    pub read_buf: usize,
    /// 来源返回「暂时无数据」时的轮询间隔。
    pub poll_when_empty: Duration,
    /// 读满这么多字节后自动停止（`None` = 一直读，直到 `stop()` 或来源结束）。
    pub stop_after_bytes: Option<u64>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            batch_bytes: DEFAULT_BATCH_BYTES,
            batch_timeout: Duration::from_millis(DEFAULT_BATCH_TIMEOUT_MS),
            display_capacity: DEFAULT_DISPLAY_CAPACITY,
            read_buf: DEFAULT_READ_BUF,
            poll_when_empty: Duration::from_millis(DEFAULT_POLL_WHEN_EMPTY_MS),
            stop_after_bytes: None,
        }
    }
}

/// 推给显示层的一个批次。`t_mono` 是**本批第一个字节**的到达时刻（来源打出，不改写）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayBatch {
    pub t_mono: MonoMicros,
    pub bytes: Vec<u8>,
}

/// 管线为什么停下。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// 还在跑。
    Running,
    /// 外部调用了 [`Pipeline::stop`]。
    Requested,
    /// 达到了 [`PipelineConfig::stop_after_bytes`]。
    BytesReached,
    /// 来源报 `SourceError::Closed`（正常结束，不是错误）。
    SourceClosed,
    /// 来源报 `SourceError::Fatal`，或落盘 `append` 失败。
    FatalError,
}

impl StopReason {
    /// 编码进 `AtomicU8`（快照读的时候要原子）。未知值按 `Running` 处理。
    fn code(self) -> u8 {
        match self {
            StopReason::Running => 0,
            StopReason::Requested => 1,
            StopReason::BytesReached => 2,
            StopReason::SourceClosed => 3,
            StopReason::FatalError => 4,
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            1 => StopReason::Requested,
            2 => StopReason::BytesReached,
            3 => StopReason::SourceClosed,
            4 => StopReason::FatalError,
            _ => StopReason::Running,
        }
    }
}

/// 管线计数快照。三个「字节数」的关系是零丢失的核心断言：
/// `source_bytes == logged_bytes`（落盘没丢），而显示侧可能少了
/// `display_bytes_dropped`（显示丢了，但报出来了）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineStats {
    /// 会话创建时刻的 `(墙上时钟, 单调时钟)`：把 `Block::t_mono` 换算成墙上时间的**精确**基准。
    /// 它不是「第一批到达时刻」——那会让所有时间戳带上「开始到首字节之间那段沉默」的偏移。
    pub session_start: SessionStart,
    /// 从来源读到的字节总数。
    pub source_bytes: u64,
    /// 成功 append 进 LogStore 的字节总数。
    pub logged_bytes: u64,
    /// 成功 `read` 的次数（不含「暂时无数据」）。
    pub reads: u64,
    /// 来源返回「暂时无数据」的次数（不是错误）。
    pub empty_reads: u64,
    /// 成功推入显示通道的批次数。
    pub batches_emitted: u64,
    /// 成功推入显示通道的字节数。
    pub display_bytes_emitted: u64,
    /// 因显示通道满/已断开而丢弃的批次数。
    pub batches_dropped: u64,
    /// 因显示通道满/已断开而丢弃的字节数。
    pub display_bytes_dropped: u64,
    /// 停止原因。
    pub stop_reason: StopReason,
    /// 致命错误（来源断开 / 落盘失败）；`None` 表示没有。
    pub fatal_error: Option<String>,
    /// 读取线程是否已经结束。
    pub thread_finished: bool,
}

/// 内部计数器（跨线程共享；快照用 Relaxed 读，量级足够）。
#[derive(Debug, Default)]
struct Counters {
    source_bytes: AtomicU64,
    logged_bytes: AtomicU64,
    reads: AtomicU64,
    empty_reads: AtomicU64,
    batches_emitted: AtomicU64,
    display_bytes_emitted: AtomicU64,
    batches_dropped: AtomicU64,
    display_bytes_dropped: AtomicU64,
    stop_reason: AtomicU64,
    thread_finished: AtomicBool,
    fatal_error: Mutex<Option<String>>,
    /// 「线程已结束」的等待点（`wait_until_finished` 用它，不必忙等）。
    finished: Mutex<bool>,
    finished_cv: Condvar,
}

impl Counters {
    fn mark_finished(&self) {
        self.thread_finished.store(true, Ordering::Relaxed);
        *self.finished.lock().unwrap() = true;
        self.finished_cv.notify_all();
    }

    fn finished(&self) -> bool {
        *self.finished.lock().unwrap()
    }
}

fn load(counter: &AtomicU64) -> u64 {
    // u64 计数器用 Relaxed 足够：统计快照不需要与别的计数器构成 happens-before。
    counter.load(Ordering::Relaxed)
}

/// 一个正在跑的采集管线：常驻读取线程 + 有界显示通道 + 一个 LogStore 会话。
pub struct Pipeline {
    store: Arc<LogStore>,
    session: SessionId,
    /// 建会话那一刻的 `(墙上时钟, 单调时钟)`：**在 spawn 时取一次并缓存**，
    /// 免得每次 `stats()` 都去问一次 store（也就不可能被后来的调用“漂移”）。
    session_start: SessionStart,
    counters: Arc<Counters>,
    stop_flag: Arc<AtomicBool>,
    handle: Mutex<Option<JoinHandle<()>>>,
    display_rx: Mutex<Option<Receiver<DisplayBatch>>>,
}

impl Pipeline {
    /// 建会话并起读取线程。
    ///
    /// `store` 用 `Arc` 共享给调用方，好让调用方在跑完后自己 `open_replay` 复核。
    pub fn spawn(
        store: Arc<LogStore>,
        meta: SessionMeta,
        source: Box<dyn ByteSource + Send>,
        cfg: PipelineConfig,
    ) -> Result<Self, StoreError> {
        let session = store.create_session(&meta)?;
        // 会话起点紧跟在 create_session 之后取：它就是「会话创建点」的那对时钟读数。
        let session_start = store.session_start(&session)?;
        let (tx, rx) = std::sync::mpsc::sync_channel(cfg.display_capacity);
        let counters = Arc::new(Counters::default());
        let stop_flag = Arc::new(AtomicBool::new(false));

        let handle = {
            let store = Arc::clone(&store);
            let session = session.clone();
            let counters = Arc::clone(&counters);
            let stop_flag = Arc::clone(&stop_flag);
            thread::Builder::new()
                .name("embhub-source-reader".to_string())
                .spawn(move || run_reader(store, session, source, cfg, counters, stop_flag, tx))
                .map_err(|e| StoreError::Io(format!("起读取线程失败: {e}")))?
        };

        Ok(Self {
            store,
            session,
            session_start,
            counters,
            stop_flag,
            handle: Mutex::new(Some(handle)),
            display_rx: Mutex::new(Some(rx)),
        })
    }

    /// 会话创建时刻的 `(墙上时钟, 单调时钟)` 对应关系（精确换算基准）。
    pub fn session_start(&self) -> SessionStart {
        self.session_start
    }

    /// 会话 id（回放、读会话头都用它）。
    pub fn session_id(&self) -> &SessionId {
        &self.session
    }

    /// 取走显示通道的接收端（**只能取一次**；第二次返回 `None`）。
    ///
    /// 消费者该在自己的线程/事件循环里 `recv`；不取走也不影响落盘。
    pub fn take_display_receiver(&self) -> Option<Receiver<DisplayBatch>> {
        self.display_rx.lock().unwrap().take()
    }

    /// 计数快照。**停止之后仍然可读**（这就是它不借用线程状态的原因）。
    pub fn stats(&self) -> PipelineStats {
        PipelineStats {
            session_start: self.session_start,
            source_bytes: load(&self.counters.source_bytes),
            logged_bytes: load(&self.counters.logged_bytes),
            reads: load(&self.counters.reads),
            empty_reads: load(&self.counters.empty_reads),
            batches_emitted: load(&self.counters.batches_emitted),
            display_bytes_emitted: load(&self.counters.display_bytes_emitted),
            batches_dropped: load(&self.counters.batches_dropped),
            display_bytes_dropped: load(&self.counters.display_bytes_dropped),
            stop_reason: StopReason::from_code(load(&self.counters.stop_reason) as u8),
            fatal_error: self.counters.fatal_error.lock().unwrap().clone(),
            thread_finished: self.counters.thread_finished.load(Ordering::Relaxed),
        }
    }

    /// 读取线程是否还在跑。
    pub fn is_running(&self) -> bool {
        !self.counters.thread_finished.load(Ordering::Relaxed)
    }

    /// 等读取线程**自己**结束（达到 `stop_after_bytes`、来源关闭、或出错），至多 `timeout`。
    ///
    /// 返回是否已结束。这条 API 与 [`Pipeline::stop`] 的分工：
    /// `wait_until_finished` 只等待、不切断正在进行的读取；`stop` 是「现在就停」。
    /// 常规用法是「先 wait 到自然结束，再 stop 做 join + seal」。
    pub fn wait_until_finished(&self, timeout: Duration) -> bool {
        if self.counters.finished() {
            return true;
        }
        let deadline = Instant::now() + timeout;
        let mut guard = self.counters.finished.lock().unwrap();
        while !*guard {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (g, _) = self
                .counters
                .finished_cv
                .wait_timeout(guard, deadline - now)
                .unwrap();
            guard = g;
        }
        *guard
    }

    /// 停止：置停止标志 → join 读取线程 → `seal` 会话。**幂等**。
    ///
    /// 注意：若来源正处于一次阻塞 `read` 中，join 会等到这次 read 返回
    /// （串口来源由它的读超时兜底），所以 `stop` 不是「立即返回」。
    pub fn stop(&self) -> Result<(), StoreError> {
        self.stop_flag.store(true, Ordering::SeqCst);
        let handle = self.handle.lock().unwrap().take();
        if let Some(h) = handle {
            let _ = h.join();
        }
        // seal 幂等：重复 stop 只是再 seal 一次。
        self.store.seal(&self.session)
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        // 忘了 stop 也不能把线程漏在那儿写已关闭的会话。
        let _ = self.stop();
    }
}

/// 攒批 + 推送。抽成结构体是为了让「先到先发」的两条阈值写在一处。
struct Emitter {
    tx: SyncSender<DisplayBatch>,
    counters: Arc<Counters>,
    batch: Vec<u8>,
    batch_t0: MonoMicros,
    last_flush: Instant,
    batch_bytes: usize,
    batch_timeout: Duration,
}

impl Emitter {
    fn new(tx: SyncSender<DisplayBatch>, counters: Arc<Counters>, cfg: &PipelineConfig) -> Self {
        Self {
            tx,
            counters,
            batch: Vec::with_capacity(cfg.batch_bytes.max(1)),
            batch_t0: 0,
            last_flush: Instant::now(),
            batch_bytes: cfg.batch_bytes.max(1),
            batch_timeout: cfg.batch_timeout,
        }
    }

    /// 收下一个块（可能触发发送）。
    fn push(&mut self, t_mono: MonoMicros, payload: &[u8]) {
        if self.batch.is_empty() {
            self.batch_t0 = t_mono;
        }
        self.batch.extend_from_slice(payload);
        if self.batch.len() >= self.batch_bytes {
            self.flush();
        } else {
            self.flush_if_due();
        }
    }

    /// 时间阈值到了就发（字节阈值没到也发）。
    fn flush_if_due(&mut self) {
        if !self.batch.is_empty() && self.last_flush.elapsed() >= self.batch_timeout {
            self.flush();
        }
    }

    /// 尽力推一批；队列满/消费者已走就丢弃并计数。**永不阻塞**。
    fn flush(&mut self) {
        if self.batch.is_empty() {
            return;
        }
        self.last_flush = Instant::now();
        let bytes = std::mem::take(&mut self.batch);
        let len = bytes.len() as u64;
        let batch = DisplayBatch {
            t_mono: self.batch_t0,
            bytes,
        };
        match self.tx.try_send(batch) {
            Ok(()) => {
                self.counters
                    .batches_emitted
                    .fetch_add(1, Ordering::Relaxed);
                self.counters
                    .display_bytes_emitted
                    .fetch_add(len, Ordering::Relaxed);
            }
            Err(TrySendError::Full(mut b)) => {
                // 显示层满：丢的是显示，不是数据（数据早已 append 落盘）。
                self.counters
                    .batches_dropped
                    .fetch_add(1, Ordering::Relaxed);
                self.counters
                    .display_bytes_dropped
                    .fetch_add(len, Ordering::Relaxed);
                // 复用这块缓冲，避免丢弃路径上反复分配。
                b.bytes.clear();
                self.batch = b.bytes;
            }
            Err(TrySendError::Disconnected(mut b)) => {
                // 消费者已经走了：显示没人要，同样只计显示丢弃。
                self.counters
                    .batches_dropped
                    .fetch_add(1, Ordering::Relaxed);
                self.counters
                    .display_bytes_dropped
                    .fetch_add(len, Ordering::Relaxed);
                b.bytes.clear();
                self.batch = b.bytes;
            }
        }
    }
}

/// 读取线程主体。
fn run_reader(
    store: Arc<LogStore>,
    session: SessionId,
    mut source: Box<dyn ByteSource + Send>,
    cfg: PipelineConfig,
    counters: Arc<Counters>,
    stop_flag: Arc<AtomicBool>,
    tx: SyncSender<DisplayBatch>,
) {
    let mut buf = vec![0u8; cfg.read_buf.max(1)];
    let mut emitter = Emitter::new(tx, Arc::clone(&counters), &cfg);

    let reason = loop {
        if stop_flag.load(Ordering::Relaxed) {
            break StopReason::Requested;
        }
        // 上限在**读之前**判：否则 `stop_after_bytes == Some(0)`（或来源已耗尽时）
        // 会一直等数据等到天荒地老。
        if let Some(limit) = cfg.stop_after_bytes {
            if load(&counters.source_bytes) >= limit {
                break StopReason::BytesReached;
            }
        }
        match source.read(&mut buf) {
            Ok(block) => {
                if block.len == 0 {
                    // 「暂时无数据」不是错误（SPEC 3.1）：把攒着的批按时间阈值发出去，
                    // 然后小睡一下，避免忙等烧 CPU。
                    counters.empty_reads.fetch_add(1, Ordering::Relaxed);
                    emitter.flush_if_due();
                    thread::sleep(cfg.poll_when_empty);
                    continue;
                }
                let t_mono = block.t_mono; // 到达时刻：来源打出，这里只搬运、不改写
                let payload = &buf[..block.len];

                // 落盘优先：append 成功才算这批字节安全。
                if let Err(e) = store.append(&session, &Chunk { t_mono, payload }) {
                    let msg = format!("append 失败（落盘路径不允许丢字节）: {e}");
                    set_fatal(&counters, msg);
                    break StopReason::FatalError;
                }
                counters
                    .logged_bytes
                    .fetch_add(block.len as u64, Ordering::Relaxed);
                counters
                    .source_bytes
                    .fetch_add(block.len as u64, Ordering::Relaxed);
                counters.reads.fetch_add(1, Ordering::Relaxed);

                emitter.push(t_mono, payload);
            }
            Err(SourceError::Closed) => break StopReason::SourceClosed,
            Err(SourceError::Fatal(msg)) => {
                set_fatal(&counters, msg);
                break StopReason::FatalError;
            }
        }
    };

    // 收尾：把残批发出去（尽力而为，满就计丢弃）。
    emitter.flush();
    counters
        .stop_reason
        .store(reason.code() as u64, Ordering::Relaxed);
    counters.mark_finished();
    // tx（以及 Emitter 里的克隆）在这里 drop → 消费者 recv 会拿到断开信号。
}

fn set_fatal(counters: &Counters, msg: String) {
    let mut slot = counters.fatal_error.lock().unwrap();
    if slot.is_none() {
        *slot = Some(msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SessionMeta;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "embhub-pipeline-test-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 脚本化来源：按脚本逐步吐出数据，可以制造「暂时无数据」「致命错误」「正常关闭」。
    enum Step {
        /// 吐这么多字节（内容是位置相关，便于逐字节核对）。
        Data(usize),
        /// 返回 `Ok(len = 0)`：暂时无数据，不是错误。
        Empty,
        /// 返回 `Err(SourceError::Closed)`：正常结束。
        Close,
        /// 返回 `Err(SourceError::Fatal)`。
        Fatal(&'static str),
    }

    struct ScriptSource {
        steps: VecDeque<Step>,
        chunk: usize,
        emitted: u64,
        pending_close: bool,
    }

    impl ScriptSource {
        fn new(steps: Vec<Step>, chunk: usize) -> Self {
            Self {
                steps: steps.into(),
                chunk: chunk.max(1),
                emitted: 0,
                pending_close: false,
            }
        }
    }

    impl ByteSource for ScriptSource {
        fn read(&mut self, buf: &mut [u8]) -> Result<crate::source::Block, SourceError> {
            if self.pending_close {
                return Err(SourceError::Closed);
            }
            match self.steps.front_mut() {
                None => {
                    // 脚本跑完：一直「暂时无数据」，交给 stop_after_bytes/stop() 收尾。
                    Ok(crate::source::Block {
                        len: 0,
                        t_mono: self.emitted,
                    })
                }
                Some(Step::Empty) => {
                    self.steps.pop_front();
                    Ok(crate::source::Block {
                        len: 0,
                        t_mono: self.emitted,
                    })
                }
                Some(Step::Close) => {
                    self.steps.pop_front();
                    self.pending_close = true;
                    Err(SourceError::Closed)
                }
                Some(Step::Fatal(msg)) => {
                    let msg = *msg;
                    self.steps.pop_front();
                    Err(SourceError::Fatal(msg.to_string()))
                }
                Some(Step::Data(total)) => {
                    let n = (*total).min(buf.len()).min(self.chunk);
                    for (i, b) in buf.iter_mut().take(n).enumerate() {
                        *b = ((self.emitted as usize + i) % 251) as u8;
                    }
                    *total -= n;
                    if *total == 0 {
                        self.steps.pop_front();
                    }
                    self.emitted += n as u64;
                    Ok(crate::source::Block {
                        len: n,
                        t_mono: self.emitted,
                    })
                }
            }
        }

        fn close(&mut self) {
            self.pending_close = true;
        }
    }

    fn meta() -> SessionMeta {
        SessionMeta::new("synthetic:test")
    }

    /// 等管线**自然**结束（达到 `stop_after_bytes` / 来源关闭 / 出错），再 stop（join + seal）。
    ///
    /// 不能在 spawn 之后立刻 stop：那会把刚起步的读取线程按「请求停止」掐掉，
    /// 测出来的就不是「跑到 N 字节」而是「跑了多少算多少」。
    fn finish(pipeline: &Pipeline) {
        assert!(
            pipeline.wait_until_finished(Duration::from_secs(30)),
            "管线没在 30s 内自然结束: {:?}",
            pipeline.stats()
        );
        pipeline.stop().unwrap();
    }

    fn replay_len(store: &LogStore, id: &SessionId) -> u64 {
        let mut r = store.open_replay(id).unwrap();
        let mut buf = [0u8; 4096];
        let mut total = 0u64;
        loop {
            let b = r.read(&mut buf).unwrap();
            if b.len == 0 {
                break;
            }
            total += b.len as u64;
        }
        total
    }

    /// 消费者停住：显示批次丢，但落盘一个字节都不能丢，且回放可验证。
    #[test]
    fn stalled_consumer_drops_display_but_never_logs_less() {
        let root = temp_root("stalled");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        let total = 1u64 << 20; // 1 MiB
        let src = ScriptSource::new(vec![Step::Data(total as usize)], 16 * 1024);
        let cfg = PipelineConfig {
            batch_bytes: 4096,   // 小批次 → 批次多 → 队列很快满
            display_capacity: 2, // 有界且很小
            stop_after_bytes: Some(total),
            poll_when_empty: Duration::from_millis(1),
            ..PipelineConfig::default()
        };
        let pipeline = Pipeline::spawn(Arc::clone(&store), meta(), Box::new(src), cfg).unwrap();
        // 把接收端**拿在手里但一次都不 recv**：这就是「消费者停住」。
        let stalled_rx = pipeline.take_display_receiver().expect("能取到接收端");

        finish(&pipeline);
        let st = pipeline.stats();
        assert!(st.thread_finished, "读取线程必须已结束");
        assert_eq!(st.source_bytes, total, "源产出必须读全");
        assert_eq!(st.logged_bytes, total, "落盘一个字节都不能丢");
        assert!(st.batches_dropped > 0, "消费者停住时必须有批次被丢");
        assert!(
            st.display_bytes_dropped > 0,
            "丢弃的字节数必须报出来: {st:?}"
        );
        assert_eq!(
            st.display_bytes_emitted + st.display_bytes_dropped,
            total,
            "显示侧「发出 + 丢弃」必须等于源产出（显示可以丢，但要报账）"
        );
        // 落盘可验证：不能只信 stats 的自报计数。
        assert_eq!(replay_len(&store, pipeline.session_id()), total);
        drop(stalled_rx);
        std::fs::remove_dir_all(&root).ok();
    }

    /// 消费者全速：不该丢，且仍然零丢失。
    #[test]
    fn fast_consumer_drops_nothing_and_still_loses_nothing() {
        let root = temp_root("fast");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        let total = 1u64 << 20;
        let src = ScriptSource::new(vec![Step::Data(total as usize)], 16 * 1024);
        let cfg = PipelineConfig {
            batch_bytes: 4096,
            display_capacity: 4096,
            stop_after_bytes: Some(total),
            poll_when_empty: Duration::from_millis(1),
            ..PipelineConfig::default()
        };
        let pipeline = Pipeline::spawn(Arc::clone(&store), meta(), Box::new(src), cfg).unwrap();
        let rx = pipeline.take_display_receiver().unwrap();
        let consumer = thread::spawn(move || {
            let mut got = 0u64;
            let mut batches = 0u64;
            while let Ok(batch) = rx.recv() {
                got += batch.bytes.len() as u64;
                batches += 1;
            }
            (got, batches)
        });

        finish(&pipeline);
        let (displayed, batches) = consumer.join().unwrap();
        let st = pipeline.stats();
        assert_eq!(st.source_bytes, total);
        assert_eq!(st.logged_bytes, total);
        assert_eq!(st.batches_dropped, 0, "全速消费者不该丢批次: {st:?}");
        assert_eq!(st.display_bytes_dropped, 0);
        assert_eq!(displayed, total, "消费者收到的字节数必须等于源产出");
        assert_eq!(batches, st.batches_emitted);
        assert_eq!(replay_len(&store, pipeline.session_id()), total);
        std::fs::remove_dir_all(&root).ok();
    }

    /// 时间阈值：字节阈值远没到也必须发批（30 ms 那条腿）。
    #[test]
    fn time_threshold_flushes_small_batches() {
        let root = temp_root("timeflush");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        // 10 步 × 100 字节 = 1000 字节，每步之间睡 8 ms。
        let mut steps = Vec::new();
        for _ in 0..10 {
            steps.push(Step::Data(100));
            steps.push(Step::Empty);
        }
        steps.push(Step::Close);
        let cfg = PipelineConfig {
            batch_bytes: 1 << 20, // 字节阈值大到永远不会触发
            batch_timeout: Duration::from_millis(10),
            display_capacity: 64,
            poll_when_empty: Duration::from_millis(8),
            ..PipelineConfig::default()
        };
        let mut src = ScriptSource::new(steps, 100);
        src.chunk = 100;
        let pipeline = Pipeline::spawn(Arc::clone(&store), meta(), Box::new(src), cfg).unwrap();
        let rx = pipeline.take_display_receiver().unwrap();
        let consumer = thread::spawn(move || {
            let mut batches = Vec::new();
            while let Ok(b) = rx.recv() {
                batches.push(b);
            }
            batches
        });
        finish(&pipeline);
        let batches = consumer.join().unwrap();
        let st = pipeline.stats();
        assert_eq!(st.stop_reason, StopReason::SourceClosed);
        assert_eq!(st.logged_bytes, 1000);
        assert!(
            batches.len() >= 2,
            "时间阈值必须把 1000 字节拆成多批（实际 {} 批）",
            batches.len()
        );
        assert!(batches.iter().all(|b| b.bytes.len() < 1 << 20));
        assert_eq!(
            batches.iter().map(|b| b.bytes.len() as u64).sum::<u64>(),
            1000
        );
        assert_eq!(replay_len(&store, pipeline.session_id()), 1000);
        std::fs::remove_dir_all(&root).ok();
    }

    /// stop() 幂等、join 线程、seal 会话；停止后 stats() 仍可读；重复 stop 不 panic。
    #[test]
    fn stop_is_idempotent_and_seals_the_session() {
        let root = temp_root("stop");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        let src = ScriptSource::new(vec![Step::Data(50_000), Step::Empty], 4096);
        let pipeline = Pipeline::spawn(
            Arc::clone(&store),
            meta(),
            Box::new(src),
            PipelineConfig {
                stop_after_bytes: Some(50_000),
                poll_when_empty: Duration::from_millis(1),
                ..PipelineConfig::default()
            },
        )
        .unwrap();
        finish(&pipeline);
        let first = pipeline.stats();
        // 第二次、第三次 stop：不 panic，stats 仍可读（stop 幂等）。
        pipeline.stop().unwrap();
        pipeline.stop().unwrap();
        let again = pipeline.stats();
        assert_eq!(first.logged_bytes, again.logged_bytes);
        assert!(again.thread_finished);
        assert!(!pipeline.is_running());
        assert_eq!(first.logged_bytes, 50_000);

        // seal 生效：停止后不能再 append。
        let res = store.append(
            pipeline.session_id(),
            &Chunk {
                t_mono: 1,
                payload: b"x",
            },
        );
        assert!(
            matches!(res, Err(StoreError::Sealed)),
            "stop 必须 seal 会话，实际 {res:?}"
        );
        // stop 之后 stats()（管线侧）与 store.stats()（store 侧）都还能读。
        let store_stats = store.stats(pipeline.session_id()).unwrap();
        assert_eq!(store_stats.payload_bytes, 50_000);
        assert_eq!(replay_len(&store, pipeline.session_id()), 50_000);
        std::fs::remove_dir_all(&root).ok();
    }

    /// 致命错误：管线停下、错误记进 stats、不 panic；已有字节不受影响。
    #[test]
    fn fatal_source_error_stops_pipeline_and_is_recorded() {
        let root = temp_root("fatal");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        let src = ScriptSource::new(vec![Step::Data(8000), Step::Fatal("设备已拔出")], 8000);
        let pipeline = Pipeline::spawn(
            Arc::clone(&store),
            meta(),
            Box::new(src),
            PipelineConfig {
                poll_when_empty: Duration::from_millis(1),
                ..PipelineConfig::default()
            },
        )
        .unwrap();
        finish(&pipeline);
        let st = pipeline.stats();
        assert_eq!(st.stop_reason, StopReason::FatalError);
        assert_eq!(
            st.fatal_error.as_deref(),
            Some("设备已拔出"),
            "致命错误必须记进 stats"
        );
        assert_eq!(st.logged_bytes, 8000, "错误之前的字节必须已经落盘");
        assert!(st.thread_finished, "致命错误必须让线程停下，而不是空转");
        assert_eq!(replay_len(&store, pipeline.session_id()), 8000);
        std::fs::remove_dir_all(&root).ok();
    }

    /// 「暂时无数据」不是错误：只计数，管线继续读。
    #[test]
    fn empty_reads_are_not_errors() {
        let root = temp_root("empty");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        let src = ScriptSource::new(
            vec![
                Step::Empty,
                Step::Empty,
                Step::Data(3000),
                Step::Empty,
                Step::Empty,
                Step::Close,
            ],
            3000,
        );
        let pipeline = Pipeline::spawn(
            Arc::clone(&store),
            meta(),
            Box::new(src),
            PipelineConfig {
                poll_when_empty: Duration::from_millis(1),
                ..PipelineConfig::default()
            },
        )
        .unwrap();
        finish(&pipeline);
        let st = pipeline.stats();
        assert!(st.empty_reads >= 4, "空的 read 要被计数: {st:?}");
        assert!(st.fatal_error.is_none(), "「暂时无数据」不得当成错误");
        assert_eq!(st.stop_reason, StopReason::SourceClosed);
        assert_eq!(st.logged_bytes, 3000);
        assert_eq!(replay_len(&store, pipeline.session_id()), 3000);
        std::fs::remove_dir_all(&root).ok();
    }

    /// 落盘失败的语义：记为致命错误（落盘路径不允许「当没发生」）。
    #[test]
    fn append_failure_is_fatal_and_recorded() {
        let root = temp_root("append-fail");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        let src = ScriptSource::new(vec![Step::Data(4000)], 4000);
        let cfg = PipelineConfig {
            stop_after_bytes: Some(4000),
            poll_when_empty: Duration::from_millis(1),
            ..PipelineConfig::default()
        };
        let pipeline = Pipeline::spawn(Arc::clone(&store), meta(), Box::new(src), cfg).unwrap();
        // 从背后把会话 seal 掉，制造 append 失败。
        store.seal(pipeline.session_id()).unwrap();
        // 等读取线程自己撞上 Sealed 停下。
        for _ in 0..2000 {
            if !pipeline.is_running() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        let st = pipeline.stats();
        assert!(!pipeline.is_running(), "append 失败必须让管线停下: {st:?}");
        assert_eq!(st.stop_reason, StopReason::FatalError);
        assert!(
            st.fatal_error
                .as_deref()
                .unwrap_or_default()
                .contains("落盘路径"),
            "错误必须点明是落盘路径: {st:?}"
        );
        finish(&pipeline);
        std::fs::remove_dir_all(&root).ok();
    }

    /// 管线快照里的会话起点必须与 store 记的是同一个（否则换算会两套基准）。
    #[test]
    fn pipeline_stats_expose_the_store_session_start() {
        let root = temp_root("start");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        let src = ScriptSource::new(vec![Step::Data(1000)], 1000);
        let cfg = PipelineConfig {
            stop_after_bytes: Some(1000),
            poll_when_empty: Duration::from_millis(1),
            ..PipelineConfig::default()
        };
        let pipeline = Pipeline::spawn(Arc::clone(&store), meta(), Box::new(src), cfg).unwrap();
        let from_pipeline = pipeline.session_start();
        finish(&pipeline);
        let from_store = store.session_start(pipeline.session_id()).unwrap();
        assert_eq!(from_pipeline, from_store, "管线与 store 的会话起点必须一致");
        let stats = pipeline.stats();
        assert_eq!(stats.session_start, from_store);
        assert!(stats.session_start.wall_ms > 0);
        // 由到达时刻换算墙上时间：起点自身精确相等，且随 t_mono 单调不减。
        assert_eq!(
            stats.session_start.wall_time_ms_for(from_store.mono_micros),
            from_store.wall_ms
        );
        assert!(
            stats
                .session_start
                .wall_time_ms_for(from_store.mono_micros + 10_000)
                >= from_store.wall_ms + 10
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// 显示批次里的到达时刻来自来源（不改写）。
    #[test]
    fn display_batch_carries_source_timestamp() {
        let root = temp_root("ts");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        let src = ScriptSource::new(vec![Step::Data(4096)], 4096);
        let cfg = PipelineConfig {
            batch_bytes: 4096,
            display_capacity: 8,
            stop_after_bytes: Some(4096),
            poll_when_empty: Duration::from_millis(1),
            ..PipelineConfig::default()
        };
        let pipeline = Pipeline::spawn(Arc::clone(&store), meta(), Box::new(src), cfg).unwrap();
        let rx = pipeline.take_display_receiver().unwrap();
        let first = rx.recv().unwrap();
        assert_eq!(first.bytes.len(), 4096);
        assert_eq!(
            first.t_mono, 4096,
            "t_mono 必须是来源打出的值（本测试源=已产出字节数）"
        );
        finish(&pipeline);
        std::fs::remove_dir_all(&root).ok();
    }

    /// 落盘与显示是两条独立路径：显示通道容量为 1 也不影响落盘速率与完整性。
    #[test]
    fn tiny_display_capacity_does_not_affect_disk_path() {
        let root = temp_root("tiny-cap");
        let store = Arc::new(LogStore::new(&root, 1 << 20));
        let total = 4u64 << 20;
        let src = ScriptSource::new(vec![Step::Data(total as usize)], 64 * 1024);
        let cfg = PipelineConfig {
            batch_bytes: 8192,
            display_capacity: 1,
            stop_after_bytes: Some(total),
            poll_when_empty: Duration::from_millis(1),
            ..PipelineConfig::default()
        };
        let pipeline = Pipeline::spawn(Arc::clone(&store), meta(), Box::new(src), cfg).unwrap();
        let _rx = pipeline.take_display_receiver().unwrap(); // 不消费
        finish(&pipeline);
        let st = pipeline.stats();
        assert_eq!(st.source_bytes, total);
        assert_eq!(st.logged_bytes, total);
        assert!(st.batches_dropped >= 1);
        assert_eq!(replay_len(&store, pipeline.session_id()), total);
        std::fs::remove_dir_all(&root).ok();
    }
}
