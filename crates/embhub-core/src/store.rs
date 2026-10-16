//! SPEC 3.3 —— `LogStore`：落盘与回放。
//!
//! 契约要点（以 docs/SPEC.md 3.3 为准）：
//! - 操作：`create_session(meta) -> SessionId`；`append(id, chunk)`；
//!   `seal(id)`；`open_replay(id) -> ReplaySource`（实现 `ByteSource`）。
//! - **持久性语义（写死，不许含糊）**：`append` 返回 `Ok` 表示该记录已
//!   `write_all` 到当前分片文件并 `flush`（字节已交给操作系统），
//!   **不含 fsync**——掉电不保证，进程崩溃保证（OS 页缓存仍在）。
//! - 会话头：magic、格式版本号、起始墙上时钟、来源配置、**固件指纹段**。
//! - 分片：按大小切分（本 phase 只实现大小维度；时间维度留待后续），
//!   每条记录自带 CRC32，回放时逐条校验。
//! - 崩溃恢复：已 `seal` 的分片必须完整可读；未 seal 的（或末尾被截断的）
//!   分片读出**有效前缀**——半条记录绝不当成完整数据。
//!
//! 磁盘布局（`<root>/<session_id>/`）：
//! ```text
//! header.bin      会话头
//! seg-000000.bin  分片 0：记录序列
//! seg-000001.bin  分片 1
//! ...
//! 记录 := t_mono_us u64 LE | payload_len u32 LE | crc32 u32 LE | payload
//! ```
//!
//! # 会话头格式
//!
//! ## v2（当前，`FORMAT_VERSION == 2`）
//!
//! ```text
//! magic[8] | version u32 LE | start_wall_ms u64 LE
//!          | fp_len u32 LE | fp_json[fp_len]        <- 长度前缀的指纹段
//!          | cfg_len u32 LE | cfg[cfg_len]
//! ```
//!
//! 指纹段是 [`FingerprintSection`] 的 JSON：要么是采集到的
//! [`FirmwareFingerprint`]（8 个字段各自「有值」或「带原因地缺失」），要么是
//! **带原因**的「本次会话未采集指纹」。v1 的 32 字节固定占位装不下
//! `commit`(40) + `bin_hash`(71)，已被替换掉。
//!
//! ## v1（已废弃）
//!
//! ```text
//! magic[8] | version u32 LE = 1 | start_wall_ms u64 LE | fingerprint[32] | cfg_len u32 | cfg
//! ```
//!
//! 读到 v1 一律报 [`StoreError::BadHeader`] 并点明「v1 已废弃，需 v2」——
//! **绝不**把 v1 的 32 字节占位当成 v2 的指纹段解析（那会读出垃圾）。

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::crc::crc32;
use crate::fingerprint::FirmwareFingerprint;
use crate::source::{Block, ByteSource, MonoMicros, SourceError};

/// 会话头 magic：8 字节。
pub const HEADER_MAGIC: [u8; 8] = *b"EMBHLOG\0";
/// 当前落盘格式版本。v2 起会话头带长度前缀的固件指纹段（v1 的 32 字节占位已废）。
pub const FORMAT_VERSION: u32 = 2;
/// v1 曾用的固定指纹占位长度；仅用于说明旧格式，v2 不再写入。
pub const V1_FINGERPRINT_PLACEHOLDER_LEN: usize = 32;
/// 指纹段的 sanity 上限（防损坏的头把内存读爆）。
const MAX_FINGERPRINT_SECTION_LEN: u32 = 1 << 20;
/// 未采集指纹时写进头的默认原因（不允许静默省略）。
pub const FINGERPRINT_NOT_COLLECTED: &str = "会话创建时未提供固件指纹（未采集）";
/// 记录头长度：t_mono(8) + len(4) + crc32(4)。落盘校验（文件字节数对账）也用得上。
pub const RECORD_HEADER_LEN: u64 = 16;
/// 单条记录 payload 的 sanity 上限（防篡改出的离谱长度把内存读爆）。
const MAX_RECORD_LEN: u32 = 1 << 30;

/// 追加进日志的一块字节：原始字节 + 来源打出的到达时刻（不改写）。
#[derive(Debug, Clone, Copy)]
pub struct Chunk<'a> {
    pub t_mono: MonoMicros,
    pub payload: &'a [u8],
}

/// 会话头里的固件指纹段（v2）。
///
/// 不变量：`collected` 与 `not_collected` **恰好有一个**是 `Some`。
/// 这样「没指纹也没原因」在头里写不出来，也就读不出来。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FingerprintSection {
    /// 会话创建时采集到的固件指纹。
    pub collected: Option<FirmwareFingerprint>,
    /// 未采集时的原因（采集成功时必须为 None）。
    pub not_collected: Option<String>,
}

impl FingerprintSection {
    /// 采集到了指纹。
    pub fn collected(fp: FirmwareFingerprint) -> Self {
        Self {
            collected: Some(fp),
            not_collected: None,
        }
    }

    /// 没采集，并说明原因（原因不得为空）。
    pub fn not_collected(reason: impl Into<String>) -> Self {
        Self {
            collected: None,
            not_collected: Some(reason.into()),
        }
    }

    /// 检查「恰好一个有值」；返回违反不变量时的原因说明。
    fn validate(&self) -> Result<(), String> {
        match (&self.collected, &self.not_collected) {
            (Some(_), None) => Ok(()),
            (None, Some(r)) if !r.trim().is_empty() => Ok(()),
            (None, Some(_)) => Err("指纹段：未采集原因为空".to_string()),
            (None, None) => Err("指纹段：既没有指纹也没有未采集原因".to_string()),
            (Some(_), Some(_)) => Err("指纹段：既有指纹又有未采集原因".to_string()),
        }
    }
}

impl Default for FingerprintSection {
    /// 未采集（带默认原因）。
    fn default() -> Self {
        Self::not_collected(FINGERPRINT_NOT_COLLECTED)
    }
}

/// 会话元数据（写入会话头）。
#[derive(Debug, Clone)]
pub struct SessionMeta {
    /// 来源配置的文本描述（如 `synthetic:seed=42,total=...` 或 `serial:COM3@115200`）。
    pub source_config: String,
    /// 固件指纹段：采集到了就写指纹，没采集就必须带原因。
    pub fingerprint: FingerprintSection,
}

impl SessionMeta {
    /// 只要来源配置；指纹记为「未提供」（带原因，不静默省略）。
    pub fn new(source_config: impl Into<String>) -> Self {
        Self {
            source_config: source_config.into(),
            fingerprint: FingerprintSection::default(),
        }
    }

    /// 带上采集好的指纹。
    pub fn with_fingerprint(source_config: impl Into<String>, fp: FirmwareFingerprint) -> Self {
        Self {
            source_config: source_config.into(),
            fingerprint: FingerprintSection::collected(fp),
        }
    }
}

/// 会话标识：目录名。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 会话创建时刻的**一对**时钟读数：墙上时钟 + 单调时钟（同一时刻的两面）。
///
/// 为什么必须有它：`Block::t_mono` 是单调时钟，屏幕上要显示的是墙上时钟。
/// 只靠单调值无法还原「当时几点几分」，而用「第一批到达时刻」去近似会话起点，
/// 会让所有时间戳带上「会话开始到第一字节之间那段沉默」的偏移——沉默越久越错。
///
/// 有了这一对值，任意到达时刻的墙上时间都是**精确**的：
/// `wall_time_ms_for(t_mono)`（误差只来自「读墙上时钟」与「读单调时钟」两次调用之间的
/// 极短间隔，量级是微秒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionStart {
    /// 会话创建时刻的墙上时钟（Unix epoch 毫秒）——与写进会话头的 `start_wall_ms` 同一个值。
    pub wall_ms: u64,
    /// 会话创建时刻的单调时钟读数（微秒），与 [`crate::source::mono_now`] /
    /// [`Block::t_mono`](crate::source::Block) **同一基准**。
    pub mono_micros: crate::source::MonoMicros,
}

impl SessionStart {
    /// 由到达时刻（`t_mono`，微秒）精确换算墙上时钟（Unix epoch 毫秒）。
    ///
    /// `t_mono` 早于会话起点（理论上不该发生）时退化为 `wall_ms`，不 panic、不下溢。
    pub fn wall_time_ms_for(&self, t_mono: crate::source::MonoMicros) -> u64 {
        self.wall_ms + t_mono.saturating_sub(self.mono_micros) / 1000
    }
}

/// 会话统计（自证计数用）：writer 侧自己的账。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionStats {
    /// 已持久化的 payload 字节总数（不含记录头）。
    pub payload_bytes: u64,
    /// 已持久化的记录条数。
    pub records: u64,
    /// 已创建的分片数。
    pub segments: u64,
    /// 会话创建时刻（墙上 + 单调）——精确的换算基准，不是「首批到达时刻」。
    pub start: SessionStart,
}

#[derive(Debug)]
pub enum StoreError {
    Io(String),
    BadHeader(String),
    Corrupt(String),
    UnknownSession,
    Sealed,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(msg) => write!(f, "I/O 错误: {msg}"),
            StoreError::BadHeader(msg) => write!(f, "会话头非法: {msg}"),
            StoreError::Corrupt(msg) => write!(f, "数据损坏: {msg}"),
            StoreError::UnknownSession => write!(f, "未知会话"),
            StoreError::Sealed => write!(f, "会话已 seal，禁止再写"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        StoreError::Io(e.to_string())
    }
}

/// 解析后的会话头（独立校验入口：不依赖写入方即可读回这些字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHeader {
    /// 读到的一定是当前版本（旧版本在解析前就被拒了）。
    pub format_version: u32,
    /// 起始墙上时钟（Unix epoch 毫秒）。
    pub start_wall_ms: u64,
    /// 固件指纹段：采集到的指纹，或**带原因**的「未采集」。
    pub fingerprint: FingerprintSection,
    pub source_config: String,
}

/// v2 会话头在指纹段之前的固定部分长度：magic(8) + version(4) + start_wall_ms(8) + fp_len(4)。
const V2_FIXED_PREFIX_LEN: usize = 8 + 4 + 8 + 4;

/// 读取并校验会话头。magic 错、版本不认识（含已废弃的 v1）、截断、指纹段解析失败都会报错。
pub fn read_session_header(session_dir: &Path) -> Result<SessionHeader, StoreError> {
    let raw = fs::read(session_dir.join("header.bin"))?;
    if raw.len() < V2_FIXED_PREFIX_LEN {
        return Err(StoreError::BadHeader(format!(
            "header.bin 太短: {} < {V2_FIXED_PREFIX_LEN}",
            raw.len()
        )));
    }
    if raw[..8] != HEADER_MAGIC {
        return Err(StoreError::BadHeader("magic 不匹配".to_string()));
    }

    let version = u32::from_le_bytes(raw[8..12].try_into().unwrap());
    // 版本判定必须先于任何「按当前布局解析」的动作：v1 的 32 字节占位不能当指纹段读。
    if version == 1 {
        return Err(StoreError::BadHeader(format!(
            "日志格式 v1 已废弃，需 v2（当前 v{FORMAT_VERSION}）：v1 的 {V1_FINGERPRINT_PLACEHOLDER_LEN} 字节固定指纹占位 \
             装不下 commit + bin_hash，且没有任何指纹信息。请在 v2 下重新录制该会话"
        )));
    }
    if version != FORMAT_VERSION {
        return Err(StoreError::BadHeader(format!(
            "不支持的格式版本: {version}（当前 v{FORMAT_VERSION}）"
        )));
    }

    let start_wall_ms = u64::from_le_bytes(raw[12..20].try_into().unwrap());

    // 指纹段：长度前缀 + JSON。
    let fp_len = u32::from_le_bytes(raw[20..24].try_into().unwrap()) as usize;
    if fp_len == 0 || fp_len as u32 > MAX_FINGERPRINT_SECTION_LEN {
        return Err(StoreError::BadHeader(format!(
            "指纹段长度非法: {fp_len}（1..={MAX_FINGERPRINT_SECTION_LEN}）"
        )));
    }
    let fp_start = V2_FIXED_PREFIX_LEN;
    let fp_end = fp_start
        .checked_add(fp_len)
        .ok_or_else(|| StoreError::BadHeader("指纹段长度溢出".to_string()))?;
    if raw.len() < fp_end {
        return Err(StoreError::BadHeader(format!(
            "指纹段截断: header.bin 只有 {} 字节，需要 {fp_end}",
            raw.len()
        )));
    }
    let fingerprint: FingerprintSection = serde_json::from_slice(&raw[fp_start..fp_end])
        .map_err(|e| StoreError::BadHeader(format!("指纹段不是合法 JSON: {e}")))?;
    fingerprint
        .validate()
        .map_err(|e| StoreError::BadHeader(format!("指纹段不变量被破坏: {e}")))?;

    let cfg_len = u32::from_le_bytes(raw[fp_end..fp_end + 4].try_into().unwrap()) as usize;
    if raw.len() < fp_end + 4 + cfg_len {
        return Err(StoreError::BadHeader("source_config 截断".to_string()));
    }
    let source_config = String::from_utf8(raw[fp_end + 4..fp_end + 4 + cfg_len].to_vec())
        .map_err(|e| StoreError::BadHeader(format!("source_config 不是 UTF-8: {e}")))?;

    Ok(SessionHeader {
        format_version: version,
        start_wall_ms,
        fingerprint,
        source_config,
    })
}

struct WriterState {
    dir: PathBuf,
    /// 会话创建时刻的一对时钟读数（精确换算基准）。
    start: SessionStart,
    sealed: bool,
    cur_file: Option<File>,
    cur_seg_index: u32,
    cur_seg_bytes: u64,
    payload_bytes: u64,
    records: u64,
    segments: u64,
}

/// 落盘与回放。单写者（内部互斥），多读者（每个 `open_replay` 独立读文件）。
pub struct LogStore {
    root: PathBuf,
    /// 单分片字节上限（含记录头）；超过即切下一个分片。
    segment_max_bytes: u64,
    writers: Mutex<HashMap<String, WriterState>>,
    session_seq: AtomicU64,
}

impl LogStore {
    /// `segment_max_bytes` 可配置（测试用小值即可），运行时至少能装下一条记录头。
    pub fn new(root: impl Into<PathBuf>, segment_max_bytes: u64) -> Self {
        Self {
            root: root.into(),
            segment_max_bytes: segment_max_bytes.max(RECORD_HEADER_LEN + 1),
            writers: Mutex::new(HashMap::new()),
            session_seq: AtomicU64::new(0),
        }
    }

    fn session_dir(&self, id: &SessionId) -> PathBuf {
        self.root.join(&id.0)
    }

    /// 创建会话：建目录、立刻写会话头（起始时刻在此打出）。
    pub fn create_session(&self, meta: &SessionMeta) -> Result<SessionId, StoreError> {
        fs::create_dir_all(&self.root)?;
        let start_wall_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| StoreError::Io(format!("系统时钟异常: {e}")))?
            .as_millis() as u64;
        // 墙上时钟与单调时钟**背靠背**取：这两个读数共同定义「会话起点」，
        // 后面所有 Block::t_mono 都由同一个 mono 基准换算墙上时间。
        let start_mono_micros = crate::source::mono_now();
        let start = SessionStart {
            wall_ms: start_wall_ms,
            mono_micros: start_mono_micros,
        };
        let seq = self.session_seq.fetch_add(1, Ordering::Relaxed);
        let id = SessionId(format!("s{start_wall_ms:013}-{seq:04}"));
        let dir = self.session_dir(&id);
        fs::create_dir(&dir)?;

        // 会话头 v2：magic | version | start_wall_ms | fp_len | fp_json | cfg_len | cfg
        meta.fingerprint.validate().map_err(StoreError::BadHeader)?;
        let fp_json = serde_json::to_vec(&meta.fingerprint)
            .map_err(|e| StoreError::Io(format!("指纹段序列化失败: {e}")))?;
        if fp_json.len() as u32 > MAX_FINGERPRINT_SECTION_LEN {
            return Err(StoreError::BadHeader(format!(
                "指纹段过大: {} > {MAX_FINGERPRINT_SECTION_LEN}",
                fp_json.len()
            )));
        }

        let mut header =
            Vec::with_capacity(V2_FIXED_PREFIX_LEN + fp_json.len() + 4 + meta.source_config.len());
        header.extend_from_slice(&HEADER_MAGIC);
        header.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        header.extend_from_slice(&start_wall_ms.to_le_bytes());
        header.extend_from_slice(&(fp_json.len() as u32).to_le_bytes());
        header.extend_from_slice(&fp_json); // 长度前缀的指纹段（v2 起取代 32 字节占位）
        header.extend_from_slice(&(meta.source_config.len() as u32).to_le_bytes());
        header.extend_from_slice(meta.source_config.as_bytes());
        fs::write(dir.join("header.bin"), &header)?;

        self.writers.lock().unwrap().insert(
            id.0.clone(),
            WriterState {
                dir,
                start,
                sealed: false,
                cur_file: None,
                cur_seg_index: 0,
                cur_seg_bytes: 0,
                payload_bytes: 0,
                records: 0,
                segments: 0,
            },
        );
        Ok(id)
    }

    /// 会话创建时刻的 `(墙上时钟, 单调时钟)` 对应关系。
    ///
    /// 这是把 `Block::t_mono` 换算成墙上时间的**唯一**精确基准。
    pub fn session_start(&self, id: &SessionId) -> Result<SessionStart, StoreError> {
        let writers = self.writers.lock().unwrap();
        let w = writers.get(&id.0).ok_or(StoreError::UnknownSession)?;
        Ok(w.start)
    }

    /// 追加一条记录。**返回 `Ok` = 已 write_all + flush 到分片文件（不含 fsync）**。
    pub fn append(&self, id: &SessionId, chunk: &Chunk<'_>) -> Result<(), StoreError> {
        let mut writers = self.writers.lock().unwrap();
        let w = writers.get_mut(&id.0).ok_or(StoreError::UnknownSession)?;
        if w.sealed {
            return Err(StoreError::Sealed);
        }
        if chunk.payload.len() as u64 > MAX_RECORD_LEN as u64 {
            return Err(StoreError::Corrupt("单条记录超过 1 GiB 上限".to_string()));
        }

        let record_len = RECORD_HEADER_LEN + chunk.payload.len() as u64;
        // 分片滚动：当前分片非空且放不下整条记录时切新片。
        // 空分片哪怕装不下也直接写（单条记录允许大于分片上限，杜绝死循环）。
        if w.cur_seg_bytes > 0 && w.cur_seg_bytes + record_len > self.segment_max_bytes {
            w.cur_file = None; // 关旧片
            w.cur_seg_index += 1;
            w.cur_seg_bytes = 0;
        }
        if w.cur_file.is_none() {
            let path = w.dir.join(format!("seg-{:06}.bin", w.cur_seg_index));
            w.cur_file = Some(OpenOptions::new().create_new(true).write(true).open(path)?);
            w.segments += 1;
        }

        let f = w.cur_file.as_mut().unwrap();
        let crc = crc32(chunk.payload);
        f.write_all(&chunk.t_mono.to_le_bytes())?;
        f.write_all(&(chunk.payload.len() as u32).to_le_bytes())?;
        f.write_all(&crc.to_le_bytes())?;
        f.write_all(chunk.payload)?;
        f.flush()?; // 持久性语义：到 OS 为止，不 fsync

        w.cur_seg_bytes += record_len;
        w.payload_bytes += chunk.payload.len() as u64;
        w.records += 1;
        Ok(())
    }

    /// 封口：之后禁止 append。已 seal 的分片必须完整可读。
    pub fn seal(&self, id: &SessionId) -> Result<(), StoreError> {
        let mut writers = self.writers.lock().unwrap();
        let w = writers.get_mut(&id.0).ok_or(StoreError::UnknownSession)?;
        if let Some(f) = w.cur_file.as_mut() {
            f.flush()?;
        }
        w.cur_file = None;
        w.sealed = true;
        Ok(())
    }

    /// writer 侧账务：注入方对「落盘字节数」的独立计数。
    pub fn stats(&self, id: &SessionId) -> Result<SessionStats, StoreError> {
        let writers = self.writers.lock().unwrap();
        let w = writers.get(&id.0).ok_or(StoreError::UnknownSession)?;
        Ok(SessionStats {
            payload_bytes: w.payload_bytes,
            records: w.records,
            segments: w.segments,
            start: w.start,
        })
    }

    /// 打开回放：返回实现 `ByteSource` 的 `ReplaySource`，
    /// 重建原始字节序列与到达时刻。回放端与写入端互不阻塞。
    pub fn open_replay(&self, id: &SessionId) -> Result<ReplaySource, StoreError> {
        let dir = self.session_dir(id);
        // 先校验会话头（magic / 版本 / 截断），头坏了整个会话不可信。
        let header = read_session_header(&dir)?;

        // 收集分片，按序号排序。
        let mut segments: Vec<PathBuf> = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let name = entry?.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("seg-") && name.ends_with(".bin") {
                segments.push(dir.join(name.as_ref()));
            }
        }
        segments.sort();

        Ok(ReplaySource {
            segments,
            seg_idx: 0,
            cur: None,
            pending: Vec::new(),
            pending_pos: 0,
            pending_t: 0,
            closed: false,
            header: Some(header),
        })
    }
}

// ---------------------------------------------------------------------------
// ReplaySource —— 从分片重建字节流（SPEC 3.1 的 adapter 之一）
// ---------------------------------------------------------------------------

/// 回放源：`ByteSource` 实现。逐条读记录、校验 CRC32，把 payload
/// 按原到达时刻重新交给调用方。读路径上的缓冲复用，稳态不逐条分配。
pub struct ReplaySource {
    segments: Vec<PathBuf>,
    seg_idx: usize,
    cur: Option<BufReader<File>>,
    /// 当前记录的 payload 缓存（跨多次 read 切片返回）。
    pending: Vec<u8>,
    pending_pos: usize,
    pending_t: MonoMicros,
    closed: bool,
    header: Option<SessionHeader>,
}

impl ReplaySource {
    /// 回放时读到的会话头（magic/版本已在 open_replay 校验过）。
    pub fn session_header(&self) -> Option<&SessionHeader> {
        self.header.as_ref()
    }

    fn is_last_segment(&self) -> bool {
        self.seg_idx + 1 >= self.segments.len()
    }

    /// 推进到下一条完整记录并装进 `pending`。
    /// 返回 Ok(false) 表示没有更多完整记录（正常结束或有效前缀尽头）。
    fn load_next_record(&mut self) -> Result<bool, SourceError> {
        loop {
            if self.cur.is_none() {
                if self.seg_idx >= self.segments.len() {
                    return Ok(false);
                }
                let f = File::open(&self.segments[self.seg_idx])
                    .map_err(|e| SourceError::Fatal(format!("打开分片失败: {e}")))?;
                self.cur = Some(BufReader::new(f));
            }
            let r = self.cur.as_mut().unwrap();

            // 记录头 16 字节。
            let mut hdr = [0u8; RECORD_HEADER_LEN as usize];
            match read_exact_or_eof(r, &mut hdr) {
                Ok(true) => {}
                Ok(false) => {
                    // 干净 EOF：本片读完，换下一片。
                    self.cur = None;
                    self.seg_idx += 1;
                    continue;
                }
                Err(e) => {
                    // 半截记录头：末片=有效前缀尽头；非末片=损坏。
                    return if self.is_last_segment() {
                        Ok(false)
                    } else {
                        Err(SourceError::Fatal(format!(
                            "分片 {} 记录头截断: {e}",
                            self.seg_idx
                        )))
                    };
                }
            }

            let t_mono = u64::from_le_bytes(hdr[0..8].try_into().unwrap());
            let len = u32::from_le_bytes(hdr[8..12].try_into().unwrap());
            let crc_expect = u32::from_le_bytes(hdr[12..16].try_into().unwrap());
            if len > MAX_RECORD_LEN {
                return Err(SourceError::Fatal(format!(
                    "记录长度越界: {len}（分片 {}，疑似损坏）",
                    self.seg_idx
                )));
            }

            self.pending.resize(len as usize, 0);
            match read_exact_or_eof(r, &mut self.pending) {
                Ok(true) => {}
                Ok(false) | Err(_) => {
                    // payload 不够长：末片=有效前缀；非末片=损坏。
                    return if self.is_last_segment() {
                        Ok(false)
                    } else {
                        Err(SourceError::Fatal(format!(
                            "分片 {} payload 截断",
                            self.seg_idx
                        )))
                    };
                }
            }

            let crc_actual = crc32(&self.pending);
            if crc_actual != crc_expect {
                return Err(SourceError::Fatal(format!(
                    "CRC32 校验失败（分片 {}，期望 {crc_expect:#010x}，实得 {crc_actual:#010x}）",
                    self.seg_idx
                )));
            }

            self.pending_pos = 0;
            self.pending_t = t_mono;
            return Ok(true);
        }
    }
}

/// `Read::read_exact` 的变体：把「第一个字节就 EOF」与「读到一半 EOF」分开。
/// Ok(true)=读满；Ok(false)=一开始就是 EOF；Err(_)=半截。
fn read_exact_or_eof(r: &mut impl Read, buf: &mut [u8]) -> Result<bool, io::Error> {
    if buf.is_empty() {
        return Ok(true);
    }
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => {
                return if filled == 0 {
                    Ok(false)
                } else {
                    Err(io::Error::new(io::ErrorKind::UnexpectedEof, "记录截断"))
                };
            }
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

impl ByteSource for ReplaySource {
    fn read(&mut self, buf: &mut [u8]) -> Result<Block, SourceError> {
        if self.closed {
            return Err(SourceError::Closed);
        }
        if self.pending_pos == self.pending.len() && !self.load_next_record()? {
            return Ok(Block { len: 0, t_mono: 0 });
        }
        let avail = self.pending.len() - self.pending_pos;
        let n = avail.min(buf.len());
        buf[..n].copy_from_slice(&self.pending[self.pending_pos..self.pending_pos + n]);
        self.pending_pos += n;
        Ok(Block {
            len: n,
            t_mono: self.pending_t,
        })
    }

    fn close(&mut self) {
        self.closed = true;
        self.cur = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::Field;
    use crate::source::{SyntheticConfig, SyntheticSource};

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "embhub-store-test-{}-{}-{}",
            tag,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn meta() -> SessionMeta {
        SessionMeta::new("synthetic:seed=42,total=100000,chunk=4096")
    }

    /// v1 会话头的固定部分长度：magic(8) + version(4) + start_wall_ms(8) + 占位(32) + cfg_len(4)。
    const V1_FIXED_HEADER_LEN: usize = 8 + 4 + 8 + V1_FINGERPRINT_PLACEHOLDER_LEN + 4;

    /// 一份**所有字段都有值**的指纹，字段内容故意带 JSON 需要转义的字符
    /// （反斜杠、引号、中文），用来证明往返无损。
    fn full_fingerprint() -> FirmwareFingerprint {
        FirmwareFingerprint {
            repo_root: Field::Value("C:\\work\\\"embhub\"\\repo".to_string()),
            commit: Field::Value("0123456789abcdef0123456789abcdef01234567".to_string()),
            dirty: Field::Value(true),
            bin_path: Field::Value("out\\fw v2\\app.elf".to_string()),
            bin_hash: Field::Value(format!("sha256:{}", "ab".repeat(32))),
            build_time: Field::Value(1_792_000_000_000),
            chip: Field::Value("STM32F407 中文芯片".to_string()),
            probe_id: Field::Value("SN:0001".to_string()),
        }
    }

    /// 一份**所有字段都缺失**的指纹，原因同样带转义字符。
    fn all_missing_fingerprint() -> FirmwareFingerprint {
        FirmwareFingerprint {
            repo_root: Field::Missing("git 不可用: program not found".to_string()),
            commit: Field::Missing("非 git 目录（\"不存在\"）".to_string()),
            dirty: Field::Missing("git status 失败".to_string()),
            bin_path: Field::Missing("未提供构建产物（--bin）".to_string()),
            bin_hash: Field::Missing("未提供构建产物（--bin）".to_string()),
            build_time: Field::Missing("未提供构建产物（--bin）".to_string()),
            chip: Field::Missing("Phase 7（probe-rs）尚未采集".to_string()),
            probe_id: Field::Missing("Phase 7（probe-rs）尚未采集".to_string()),
        }
    }

    /// 用确定性合成源注满 store，返回注入的字节序列副本。
    fn inject(store: &LogStore, id: &SessionId, total: u64, chunk: usize) -> Vec<u8> {
        let mut src = SyntheticSource::open(&SyntheticConfig {
            seed: 42,
            total_bytes: total,
            chunk_size: chunk,
        })
        .unwrap();
        let mut expected = Vec::new();
        let mut buf = vec![0u8; chunk.max(1)];
        loop {
            let b = src.read(&mut buf).unwrap();
            if b.len == 0 {
                break;
            }
            store
                .append(
                    id,
                    &Chunk {
                        t_mono: b.t_mono,
                        payload: &buf[..b.len],
                    },
                )
                .unwrap();
            expected.extend_from_slice(&buf[..b.len]);
        }
        expected
    }

    fn replay_all(store: &LogStore, id: &SessionId) -> Vec<u8> {
        let mut r = store.open_replay(id).unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 3000]; // 故意用不整除的读缓冲，跨记录切片
        loop {
            let b = r.read(&mut buf).unwrap();
            if b.len == 0 {
                break;
            }
            out.extend_from_slice(&buf[..b.len]);
        }
        out
    }

    #[test]
    fn empty_session_replays_zero_bytes() {
        let root = temp_root("empty");
        let store = LogStore::new(&root, 4096);
        let id = store.create_session(&meta()).unwrap();
        store.seal(&id).unwrap();
        let got = replay_all(&store, &id);
        assert!(got.is_empty(), "空输入必须回放为空");
        let stats = store.stats(&id).unwrap();
        assert_eq!(stats.payload_bytes, 0);
        assert_eq!(stats.segments, 0, "空会话不产生分片文件");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn exactly_one_segment_when_exact_fit() {
        let root = temp_root("one-seg");
        // 分片上限 4096；一条 4080 payload + 16 记录头 = 恰好 4096。
        let store = LogStore::new(&root, 4096);
        let id = store.create_session(&meta()).unwrap();
        let expected = inject(&store, &id, 4080, 4080);
        store.seal(&id).unwrap();
        assert_eq!(store.stats(&id).unwrap().segments, 1, "恰好一个分片");
        assert_eq!(replay_all(&store, &id), expected);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rolls_over_across_segment_boundary() {
        let root = temp_root("rollover");
        let store = LogStore::new(&root, 1024);
        let id = store.create_session(&meta()).unwrap();
        // 10 条 300 字节记录（每条含头 316）：316,632,948 → 下一条越界切新片。
        let expected = inject(&store, &id, 3000, 300);
        store.seal(&id).unwrap();
        let stats = store.stats(&id).unwrap();
        assert!(stats.segments >= 2, "必须跨分片，实际 {}", stats.segments);
        assert_eq!(stats.payload_bytes, 3000);
        let got = replay_all(&store, &id);
        assert_eq!(got, expected, "跨分片边界回放必须逐字节一致");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn replay_is_byte_for_byte_identical_to_injected() {
        let root = temp_root("identical");
        let store = LogStore::new(&root, 64 * 1024);
        let id = store.create_session(&meta()).unwrap();
        let expected = inject(&store, &id, 100_000, 4096);
        store.seal(&id).unwrap();
        let got = replay_all(&store, &id);
        assert_eq!(got.len(), expected.len());
        assert_eq!(got, expected, "回放读回必须与注入逐字节一致");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn replay_preserves_arrival_timestamps() {
        let root = temp_root("timestamps");
        let store = LogStore::new(&root, 64 * 1024);
        let id = store.create_session(&meta()).unwrap();
        let payload = [0xABu8; 100];
        store
            .append(
                &id,
                &Chunk {
                    t_mono: 123_456_789,
                    payload: &payload,
                },
            )
            .unwrap();
        store.seal(&id).unwrap();
        let mut r = store.open_replay(&id).unwrap();
        let mut buf = [0u8; 64];
        let b = r.read(&mut buf).unwrap();
        assert_eq!(b.t_mono, 123_456_789, "到达时刻必须原样重建");
        assert_eq!(&buf[..b.len], &payload[..64]);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn tampered_payload_byte_is_detected_by_crc() {
        let root = temp_root("tamper");
        let store = LogStore::new(&root, 4096);
        let id = store.create_session(&meta()).unwrap();
        let expected = inject(&store, &id, 2000, 500);
        store.seal(&id).unwrap();

        // 人为篡改：seg-000000.bin 中第一笔 payload 的第一个字节翻转。
        let seg = root.join(id.0.clone()).join("seg-000000.bin");
        let mut raw = fs::read(&seg).unwrap();
        let payload_off = RECORD_HEADER_LEN as usize;
        raw[payload_off] ^= 0x01;
        fs::write(&seg, &raw).unwrap();

        let mut r = store.open_replay(&id).unwrap();
        let mut buf = [0u8; 4096];
        let res = r.read(&mut buf);
        assert!(
            matches!(res, Err(SourceError::Fatal(ref m)) if m.contains("CRC32")),
            "篡改一个字节必须被 CRC 检出，实际: {res:?}"
        );
        drop(expected);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn tampered_byte_in_later_segment_is_detected_by_crc() {
        let root = temp_root("tamper-later-seg");
        let store = LogStore::new(&root, 512);
        let id = store.create_session(&meta()).unwrap();
        // 300 字节一块、共 3000 字节 → 每片 1 条记录（316 字节），至少 8 个分片。
        let expected = inject(&store, &id, 3000, 300);
        store.seal(&id).unwrap();
        assert!(store.stats(&id).unwrap().segments >= 3, "需要多分片");

        // 篡改**第二片**里那条记录的 payload 最后一个字节。
        let seg = root.join(id.0.clone()).join("seg-000001.bin");
        let mut raw = fs::read(&seg).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0x80;
        fs::write(&seg, &raw).unwrap();

        // 回放：第一片应原样读出，读到第二片时必须被 CRC 拦下，且报错前不得吐出坏数据。
        let mut r = store.open_replay(&id).unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        let err = loop {
            match r.read(&mut buf) {
                Ok(b) if b.len == 0 => panic!("篡改必须报错，而不是静默结束"),
                Ok(b) => {
                    got.extend_from_slice(&buf[..b.len]);
                    assert!(
                        got.len() <= expected.len(),
                        "报错前吐出的字节数不得超过注入总量"
                    );
                }
                Err(e) => break e,
            }
        };
        assert!(
            matches!(err, SourceError::Fatal(ref m) if m.contains("CRC32")),
            "跨分片篡改必须被 CRC 检出，实际: {err:?}"
        );
        assert_eq!(got, expected[..300].to_vec(), "第一片必须是完好的有效前缀");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn truncated_tail_yields_valid_prefix() {
        let root = temp_root("truncate");
        let store = LogStore::new(&root, 64 * 1024);
        let id = store.create_session(&meta()).unwrap();
        let expected = inject(&store, &id, 3000, 1000); // 3 条记录
        store.seal(&id).unwrap();

        // 把最后一个分片砍掉 20 字节：第三条记录 payload 不完整。
        let seg = root.join(id.0.clone()).join("seg-000000.bin");
        let raw = fs::read(&seg).unwrap();
        fs::write(&seg, &raw[..raw.len() - 20]).unwrap();

        let got = replay_all(&store, &id);
        assert_eq!(
            got,
            expected[..2000].to_vec(),
            "崩溃截断后必须读出有效前缀（前两条完整记录）"
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn bad_magic_or_version_is_rejected() {
        let root = temp_root("badmagic");
        let store = LogStore::new(&root, 4096);
        let id = store.create_session(&meta()).unwrap();
        inject(&store, &id, 10, 10);
        store.seal(&id).unwrap();

        let header_path = root.join(id.0.clone()).join("header.bin");
        let mut raw = fs::read(&header_path).unwrap();
        raw[0] ^= 0xFF; // 破坏 magic
        fs::write(&header_path, &raw).unwrap();
        assert!(matches!(
            store.open_replay(&id),
            Err(StoreError::BadHeader(_))
        ));

        let mut raw = fs::read(&header_path).unwrap();
        raw[0] ^= 0xFF; // 恢复 magic
        raw[8] = 99; // 破坏版本号
        fs::write(&header_path, &raw).unwrap();
        assert!(matches!(
            store.open_replay(&id),
            Err(StoreError::BadHeader(_))
        ));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn session_header_roundtrip_contains_all_contract_fields() {
        let root = temp_root("header");
        let store = LogStore::new(&root, 4096);
        let id = store.create_session(&meta()).unwrap();
        let dir = root.join(id.0.clone());
        let h = read_session_header(&dir).unwrap();
        assert_eq!(h.format_version, FORMAT_VERSION);
        assert_eq!(h.format_version, 2, "本轮必须是 v2");
        assert!(h.start_wall_ms > 0, "起始墙上时钟必须写入");
        // v2：不再是 32 字节全零占位，而是长度前缀的指纹段；未提供指纹时也必须带原因。
        assert!(
            h.fingerprint.collected.is_none(),
            "未提供指纹时不得凭空造一个"
        );
        assert_eq!(
            h.fingerprint.not_collected.as_deref(),
            Some(FINGERPRINT_NOT_COLLECTED),
            "未采集必须带原因，不允许静默省略"
        );
        assert_eq!(h.source_config, meta().source_config);
        // 独立校验：直接读原始字节确认 magic 在最前面、版本字段是 2、指纹段确实是长度前缀的 JSON。
        let raw = fs::read(dir.join("header.bin")).unwrap();
        assert_eq!(&raw[..8], &HEADER_MAGIC);
        assert_eq!(u32::from_le_bytes(raw[8..12].try_into().unwrap()), 2);
        let fp_len = u32::from_le_bytes(raw[20..24].try_into().unwrap()) as usize;
        assert!(fp_len > 0, "指纹段长度前缀不得为 0");
        let fp_json: serde_json::Value =
            serde_json::from_slice(&raw[24..24 + fp_len]).expect("指纹段必须是合法 JSON");
        assert!(fp_json.get("collected").is_some());
        assert!(fp_json.get("not_collected").is_some());
        // v1 的 32 字节占位必须已经不存在：头里没有一段「全零 32 字节」被当作指纹。
        assert_ne!(fp_len, V1_FINGERPRINT_PLACEHOLDER_LEN);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn v2_header_roundtrips_every_fingerprint_field() {
        let root = temp_root("fp-roundtrip");
        let store = LogStore::new(&root, 4096);
        let fp = full_fingerprint();
        let id = store
            .create_session(&SessionMeta::with_fingerprint(
                "synthetic:seed=1",
                fp.clone(),
            ))
            .unwrap();
        store.seal(&id).unwrap();
        let h = read_session_header(&root.join(id.0.clone())).unwrap();

        assert_eq!(h.format_version, 2);
        let back = h.fingerprint.collected.as_ref().expect("必须读回指纹");
        assert_eq!(back, &fp, "指纹必须逐字段无损往返");
        // 逐字段再点名一遍，失败时能直接看出是哪个字段。
        assert_eq!(back.repo_root, fp.repo_root);
        assert_eq!(back.commit, fp.commit);
        assert_eq!(back.dirty, fp.dirty);
        assert_eq!(back.bin_path, fp.bin_path);
        assert_eq!(back.bin_hash, fp.bin_hash);
        assert_eq!(back.build_time, fp.build_time);
        assert_eq!(back.chip, fp.chip);
        assert_eq!(back.probe_id, fp.probe_id);
        assert_eq!(back.missing().len(), 0, "全字段有值时缺失清单必须为空");
        assert_eq!(h.source_config, "synthetic:seed=1");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn v2_header_roundtrips_all_missing_fields_with_reasons() {
        let root = temp_root("fp-missing-roundtrip");
        let store = LogStore::new(&root, 4096);
        let fp = all_missing_fingerprint();
        let id = store
            .create_session(&SessionMeta::with_fingerprint(
                "serial:COM3@115200",
                fp.clone(),
            ))
            .unwrap();
        let h = read_session_header(&root.join(id.0.clone())).unwrap();
        let back = h.fingerprint.collected.as_ref().expect("必须读回指纹");
        assert_eq!(back, &fp, "缺失原因也必须无损往返");
        assert_eq!(back.missing().len(), 8, "8 个字段的缺失原因都要在");
        for (_, reason) in back.missing() {
            assert!(!reason.trim().is_empty(), "原因不得为空");
        }
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn session_start_is_captured_at_creation_and_matches_the_header() {
        let root = temp_root("start-pair");
        let store = LogStore::new(&root, 4096);
        let before = crate::source::mono_now();
        let id = store.create_session(&meta()).unwrap();
        let after = crate::source::mono_now();
        let start = store.session_start(&id).unwrap();

        // ① 墙上时钟与写进会话头的是同一个值（`s<wall_ms>-<seq>` 也用它命名）。
        let header = read_session_header(&root.join(&id.0)).unwrap();
        assert_eq!(start.wall_ms, header.start_wall_ms);
        assert!(start.wall_ms > 0);
        // ② 单调读数取自**创建点**：落在 create_session 前后两次取样之间。
        assert!(
            start.mono_micros >= before && start.mono_micros <= after,
            "start.mono={} 不在 [{before}, {after}] 区间内",
            start.mono_micros
        );
        // ③ 换算自洽：起点自身精确对得上，±2.5ms 就是 ±2ms（整数毫秒截断）。
        assert_eq!(start.wall_time_ms_for(start.mono_micros), start.wall_ms);
        assert_eq!(
            start.wall_time_ms_for(start.mono_micros + 2_500),
            start.wall_ms + 2
        );
        assert_eq!(start.wall_time_ms_for(0), start.wall_ms, "早于起点不下溢");
        // ④ store 侧统计与 session_start 带着同一对读数。
        assert_eq!(store.stats(&id).unwrap().start, start);
        // ⑤ 未知会话报错，绝不返回一个假的起点。
        assert!(matches!(
            store.session_start(&SessionId("nope".into())),
            Err(StoreError::UnknownSession)
        ));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn v1_header_is_rejected_with_clear_error() {
        let root = temp_root("v1-reject");
        let store = LogStore::new(&root, 4096);
        let id = store.create_session(&meta()).unwrap();
        let dir = root.join(id.0.clone());
        store.seal(&id).unwrap();

        // 手写一个 v1 头（旧布局：32 字节指纹占位），证明旧格式被明确拒绝。
        let mut v1: Vec<u8> = Vec::new();
        v1.extend_from_slice(&HEADER_MAGIC);
        v1.extend_from_slice(&1u32.to_le_bytes()); // 版本 1
        v1.extend_from_slice(&1_700_000_000_000u64.to_le_bytes());
        v1.extend_from_slice(&[0u8; V1_FINGERPRINT_PLACEHOLDER_LEN]); // v1 的占位
        let cfg = b"synthetic:seed=42";
        v1.extend_from_slice(&(cfg.len() as u32).to_le_bytes());
        v1.extend_from_slice(cfg);
        assert_eq!(
            v1.len(),
            V1_FIXED_HEADER_LEN + cfg.len(),
            "v1 头长度应与常量一致"
        );
        fs::write(dir.join("header.bin"), &v1).unwrap();

        let err = read_session_header(&dir).expect_err("v1 必须被拒绝");
        let msg = err.to_string();
        assert!(msg.contains("v1"), "错误必须点明 v1: {msg}");
        assert!(msg.contains("废弃"), "错误必须点明已废弃: {msg}");
        assert!(msg.contains("v2"), "错误必须点明需 v2: {msg}");
        // 回放入口同样要在打开时就拒绝，而不是读到垃圾字节。
        assert!(matches!(
            store.open_replay(&id),
            Err(StoreError::BadHeader(_))
        ));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unknown_future_version_is_rejected() {
        let root = temp_root("future-version");
        let store = LogStore::new(&root, 4096);
        let id = store.create_session(&meta()).unwrap();
        let dir = root.join(id.0.clone());
        let mut raw = fs::read(dir.join("header.bin")).unwrap();
        raw[8..12].copy_from_slice(&3u32.to_le_bytes());
        fs::write(dir.join("header.bin"), &raw).unwrap();

        let err = read_session_header(&dir).expect_err("未知版本必须被拒绝");
        assert!(err.to_string().contains("不支持的格式版本"), "{err}");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn broken_fingerprint_section_is_rejected_not_ignored() {
        let root = temp_root("fp-broken");
        let store = LogStore::new(&root, 4096);
        let id = store.create_session(&meta()).unwrap();
        let dir = root.join(id.0.clone());
        let raw = fs::read(dir.join("header.bin")).unwrap();
        let fp_len = u32::from_le_bytes(raw[20..24].try_into().unwrap()) as usize;

        // (a) 指纹段被换成非法 JSON：必须报错，不能当成「没有指纹」混过去。
        let mut bad = raw.clone();
        for b in bad[24..24 + fp_len].iter_mut() {
            *b = b'{';
        }
        fs::write(dir.join("header.bin"), &bad).unwrap();
        let err = read_session_header(&dir).expect_err("非法指纹段必须报错");
        assert!(err.to_string().contains("指纹段"), "{err}");

        // (b) 指纹段长度前缀为 0：v2 不允许（「没指纹也没原因」这种状态不存在）。
        let mut zero_len = raw.clone();
        zero_len[20..24].copy_from_slice(&0u32.to_le_bytes());
        fs::write(dir.join("header.bin"), &zero_len).unwrap();
        assert!(read_session_header(&dir).is_err());

        // (c) 长度前缀谎报超长/截断：必须报错，不能越界读。
        let mut over = raw.clone();
        over[20..24].copy_from_slice(&(fp_len as u32 + 1000).to_le_bytes());
        fs::write(dir.join("header.bin"), &over).unwrap();
        let err = read_session_header(&dir).expect_err("截断的指纹段必须报错");
        assert!(err.to_string().contains("指纹段"), "{err}");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn fingerprint_section_invariant_is_enforced_on_write() {
        // 既没指纹也没原因、或两者都有 → 写会话时就拒绝（不变量不做进类型就靠这里兜底）。
        let root = temp_root("fp-invariant");
        let store = LogStore::new(&root, 4096);
        let empty = FingerprintSection {
            collected: None,
            not_collected: None,
        };
        assert!(store
            .create_session(&SessionMeta {
                source_config: "x".into(),
                fingerprint: empty,
            })
            .is_err());

        let both = FingerprintSection {
            collected: Some(full_fingerprint()),
            not_collected: Some("也写了原因".into()),
        };
        assert!(store
            .create_session(&SessionMeta {
                source_config: "x".into(),
                fingerprint: both,
            })
            .is_err());

        let blank_reason = FingerprintSection::not_collected("   ");
        assert!(store
            .create_session(&SessionMeta {
                source_config: "x".into(),
                fingerprint: blank_reason,
            })
            .is_err());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn append_after_seal_is_rejected() {
        let root = temp_root("sealed");
        let store = LogStore::new(&root, 4096);
        let id = store.create_session(&meta()).unwrap();
        store.seal(&id).unwrap();
        let res = store.append(
            &id,
            &Chunk {
                t_mono: 1,
                payload: b"x",
            },
        );
        assert!(matches!(res, Err(StoreError::Sealed)));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn large_record_sliced_across_many_reads() {
        let root = temp_root("large-rec");
        let store = LogStore::new(&root, 8 * 1024 * 1024);
        let id = store.create_session(&meta()).unwrap();
        // 一条 100 KiB 的大记录（大于读缓冲）。
        let expected = inject(&store, &id, 100 * 1024, 100 * 1024);
        store.seal(&id).unwrap();
        let mut r = store.open_replay(&id).unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            let b = r.read(&mut buf).unwrap();
            if b.len == 0 {
                break;
            }
            got.extend_from_slice(&buf[..b.len]);
        }
        assert_eq!(got, expected, "大记录跨多次 read 切片也必须逐字节一致");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn core_has_no_tauri_dependency() {
        // SPEC 原则 1 / A1：core 的依赖树里不出现 tauri。
        let manifest =
            fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
        assert!(
            !manifest.to_lowercase().contains("tauri"),
            "embhub-core 的 Cargo.toml 不允许出现 tauri"
        );
    }
}
