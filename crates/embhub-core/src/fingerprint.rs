//! SPEC 3.5 —— `FirmwareFingerprint`：回答「这段日志是哪个 bin 跑出来的」。
//!
//! 契约要点（以 docs/SPEC.md 3.5 为准）：
//! - `repo_root` / `commit` / `dirty` 由系统 `git` CLI 取得
//!   （`rev-parse --show-toplevel`、`rev-parse HEAD`、`status --porcelain`；
//!   `dirty = status --porcelain 输出非空`）。
//! - `bin_path` / `bin_hash` / `build_time`：`bin_hash` 是**文件内容**的哈希
//!   （sha256，不是路径、不是时间戳），`build_time` 是构建产物 mtime。
//! - `chip` / `probe_id`：Phase 7（probe-rs）填，本 phase 记为缺失。
//! - 采集时机：会话创建时，结果写入日志头（v2 的长度前缀指纹段，见 [`crate::store`]）。
//! - **失败语义**：任一字段取不到 → 记缺失 + 失败原因字符串；**不允许静默省略**。
//!   这里用 [`Field`] 把这个不变量做进类型：要么有值，要么**必须**带原因，
//!   没有「既没值也没原因」这种状态。采集失败**不阻塞**调用方（不返回 Result）。
//!
//! 本模块只读：不改文件、不写 git 配置、不改仓库状态。

use std::fmt;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::sha256::{sha256_file, to_hex};

/// 采集结果：有值（`Value`）或缺失但**必带原因**（`Missing`）。
///
/// 这样「看着有指纹、其实是空的」在类型上就不成立。
///
/// 可序列化是为了写进会话头（日志格式 v2 的长度前缀指纹段）并无损读回。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Field<T> {
    /// 采集成功。
    Value(T),
    /// 采集失败：附人类可读的原因（不得为空）。
    Missing(String),
}

impl<T> Field<T> {
    /// 取到值时返回引用。
    pub fn value(&self) -> Option<&T> {
        match self {
            Field::Value(v) => Some(v),
            Field::Missing(_) => None,
        }
    }

    /// 缺失时返回原因。
    pub fn reason(&self) -> Option<&str> {
        match self {
            Field::Value(_) => None,
            Field::Missing(r) => Some(r.as_str()),
        }
    }

    /// 是否缺失。
    pub fn is_missing(&self) -> bool {
        matches!(self, Field::Missing(_))
    }
}

impl<T: fmt::Display> fmt::Display for Field<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Field::Value(v) => write!(f, "{v}"),
            Field::Missing(r) => write!(f, "取不到（原因: {r}）"),
        }
    }
}

/// 芯片与探针字段：Phase 7（probe-rs）才有值。显式记缺失，不静默省略。
const PHASE7_REASON: &str = "Phase 7（probe-rs）尚未采集";

/// 一次会话创建时采集的固件指纹。字段清单同 SPEC 3.5 / PLAN.md。
///
/// `deny_unknown_fields`：从会话头读回的段一旦不是这个形状就报错，
/// 而不是静默丢掉不认识的字段（「以为有指纹，其实缺一半」正是要防的事）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirmwareFingerprint {
    /// 仓库根目录（`git rev-parse --show-toplevel`，路径分隔符为 git 的原始输出）。
    pub repo_root: Field<String>,
    /// 完整 40 位 hex 的提交号。
    pub commit: Field<String>,
    /// 工作区是否有改动（含未跟踪文件）：`git status --porcelain` 输出非空即 true。
    pub dirty: Field<bool>,
    /// **请求的**构建产物路径（即便文件不存在也如实记录请求值）。
    pub bin_path: Field<String>,
    /// 构建产物**内容**的哈希，格式 `sha256:<64 位小写 hex>`。
    pub bin_hash: Field<String>,
    /// 构建产物 mtime（Unix epoch 毫秒）。
    pub build_time: Field<u64>,
    /// 目标芯片（Phase 7）。
    pub chip: Field<String>,
    /// 调试器序列号（Phase 7）。
    pub probe_id: Field<String>,
}

impl FirmwareFingerprint {
    /// 采集失败的字段清单（字段名 + 原因）。空表示全部到手。
    /// 供调用方把「看着有指纹、其实是空的」直接暴露给用户。
    pub fn missing(&self) -> Vec<(&'static str, &str)> {
        let mut out = Vec::new();
        for (name, reason) in [
            ("repo_root", self.repo_root.reason()),
            ("commit", self.commit.reason()),
            ("dirty", self.dirty.reason()),
            ("bin_path", self.bin_path.reason()),
            ("bin_hash", self.bin_hash.reason()),
            ("build_time", self.build_time.reason()),
            ("chip", self.chip.reason()),
            ("probe_id", self.probe_id.reason()),
        ] {
            if let Some(r) = reason {
                out.push((name, r));
            }
        }
        out
    }
}

/// 采集指纹。`repo` 是待探测的目录（git 会向上找仓库根），
/// `bin` 是构建产物路径（可不提供）。
///
/// 永不 panic、永不返回错误：每个字段各自成功或带原因地缺失。
pub fn collect(repo: &Path, bin: Option<&Path>) -> FirmwareFingerprint {
    collect_with_git("git", repo, bin)
}

/// 同 [`collect`]，但可指定 git 可执行文件——用于验证「git 不可用」的失败语义。
fn collect_with_git(git: &str, repo: &Path, bin: Option<&Path>) -> FirmwareFingerprint {
    let repo_root = match run_git(git, repo, &["rev-parse", "--show-toplevel"]) {
        Ok(s) if !s.is_empty() => Field::Value(s),
        Ok(_) => Field::Missing("git rev-parse --show-toplevel 输出为空".to_string()),
        Err(e) => Field::Missing(e),
    };

    let commit = match run_git(git, repo, &["rev-parse", "HEAD"]) {
        Ok(s) if is_hex40(&s) => Field::Value(s),
        Ok(s) => Field::Missing(format!("rev-parse HEAD 不是 40 位 hex: {s:?}")),
        Err(e) => Field::Missing(e),
    };

    let dirty = match run_git(git, repo, &["status", "--porcelain"]) {
        Ok(s) => Field::Value(!s.is_empty()),
        Err(e) => Field::Missing(e),
    };

    let (bin_path, bin_hash, build_time) = probe_bin(bin);

    FirmwareFingerprint {
        repo_root,
        commit,
        dirty,
        bin_path,
        bin_hash,
        build_time,
        chip: Field::Missing(PHASE7_REASON.to_string()),
        probe_id: Field::Missing(PHASE7_REASON.to_string()),
    }
}

/// 判断是否为完整 40 位十六进制提交号。
fn is_hex40(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 构建产物三件套：请求路径、内容哈希、mtime。任一取不到都带原因。
fn probe_bin(bin: Option<&Path>) -> (Field<String>, Field<String>, Field<u64>) {
    let Some(path) = bin else {
        let reason = "未提供构建产物（--bin）".to_string();
        return (
            Field::Missing(reason.clone()),
            Field::Missing(reason.clone()),
            Field::Missing(reason),
        );
    };

    let shown = path.display().to_string();
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            let reason = format!("取 {shown} 元数据失败: {e}");
            return (
                Field::Value(shown),
                Field::Missing(reason.clone()),
                Field::Missing(reason),
            );
        }
    };
    if !meta.is_file() {
        let reason = format!("{shown} 不是普通文件");
        return (
            Field::Value(shown),
            Field::Missing(reason.clone()),
            Field::Missing(reason),
        );
    }

    let bin_hash = match sha256_file(path) {
        Ok(digest) => Field::Value(format!("sha256:{}", to_hex(&digest))),
        Err(e) => Field::Missing(format!("读取 {shown} 内容失败: {e}")),
    };
    let build_time = match meta.modified() {
        Ok(t) => match t.duration_since(UNIX_EPOCH) {
            Ok(d) => Field::Value(d.as_millis() as u64),
            Err(e) => Field::Missing(format!("{shown} 的 mtime 早于 Unix epoch: {e}")),
        },
        Err(e) => Field::Missing(format!("取 {shown} 的 mtime 失败: {e}")),
    };

    (Field::Value(shown), bin_hash, build_time)
}

/// 跑一次 git 子命令，成功返回 trim 过的 stdout，失败返回**带原因**的错误串。
fn run_git(git: &str, repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new(git)
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("执行 {git} 失败（git 不可用或不在 PATH？）: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        return Err(format!(
            "{git} {} 失败（退出码 {:?}）: {}",
            args.join(" "),
            output.status.code(),
            if stderr.is_empty() {
                "无 stderr 输出"
            } else {
                stderr
            }
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "embhub-fp-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 测试专用的 git 调用：身份用 `-c` 传入，**不写全局配置**。
    fn git_in(dir: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("测试环境必须有 git")
    }

    fn git_ok(dir: &Path, args: &[&str]) {
        let out = git_in(dir, args);
        assert!(
            out.status.success(),
            "git {args:?} 失败: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// 自建临时 git 仓库（**不依赖本工作区当前的 git 状态**）。
    fn make_repo(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        git_ok(&dir, &["init", "-q"]);
        fs::write(dir.join("tracked.txt"), b"hello\n").unwrap();
        git_ok(&dir, &["add", "tracked.txt"]);
        git_ok(
            &dir,
            &[
                "-c",
                "user.name=embhub-test",
                "-c",
                "user.email=test@embhub.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        );
        dir
    }

    /// 造一个「git 认不出仓库」的目录。
    ///
    /// 注意：临时目录本身就在本工作区这个 git 仓库里面，所以单纯「没有 .git 的
    /// 目录」会让 git 向上找到工作区仓库（本机 git 2.53 实测如此）。放一个指向
    /// 不存在路径的 `.git` 文件可以让 git 确定性地报「not a git repository」
    /// 而**不向上回退**，这正是非仓库/坏仓库场景的行为。
    fn make_non_repo(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        fs::write(dir.join(".git"), b"gitdir: /embhub/definitely/not/here\n").unwrap();
        dir
    }

    #[test]
    fn dirty_is_false_on_clean_temp_repo_and_true_after_editing_tracked_file() {
        let repo = make_repo("dirty");

        let clean = collect(&repo, None);
        assert_eq!(
            clean.dirty,
            Field::Value(false),
            "刚提交完的仓库必须 dirty=false（实际 {:?}）",
            clean.dirty
        );
        let commit = clean.commit.value().expect("临时仓库必须有 commit").clone();
        assert_eq!(commit.len(), 40, "commit 必须是完整 40 位 hex");
        assert!(commit.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(clean
            .repo_root
            .value()
            .expect("必须有 repo_root")
            .replace('\\', "/")
            .ends_with(
                repo.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
                    .as_str()
            ));

        // 改动**已跟踪**文件 → dirty=true，且 commit 不变。
        fs::write(repo.join("tracked.txt"), b"hello changed\n").unwrap();
        let after = collect(&repo, None);
        assert_eq!(
            after.dirty,
            Field::Value(true),
            "改动已跟踪文件后必须 dirty=true（实际 {:?}）",
            after.dirty
        );
        assert_eq!(after.commit.value(), Some(&commit), "改文件不该改 commit");

        fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn untracked_file_also_counts_as_dirty_per_spec() {
        // SPEC 3.5 原文：`dirty = status --porcelain 输出非空`——含未跟踪文件。
        // 这条测试锁住该语义，防止有人「顺手」加 --untracked-files=no。
        let repo = make_repo("untracked");
        assert_eq!(collect(&repo, None).dirty, Field::Value(false));
        fs::write(repo.join("brand-new-untracked.txt"), b"x\n").unwrap();
        assert_eq!(
            collect(&repo, None).dirty,
            Field::Value(true),
            "未跟踪文件也必须让 dirty=true"
        );
        fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn bin_hash_is_content_addressed_not_path_addressed() {
        let repo = make_repo("binhash");
        let a = repo.join("a.bin");
        let b = repo.join("nested/b.bin");
        fs::create_dir_all(b.parent().unwrap()).unwrap();

        let same: Vec<u8> = (0..200_000u32).map(|i| (i * 31 % 251) as u8).collect();
        fs::write(&a, &same).unwrap();
        fs::write(&b, &same).unwrap();

        let fa = collect(&repo, Some(&a));
        let fb = collect(&repo, Some(&b));
        let ha = fa.bin_hash.value().expect("必须有 bin_hash").clone();
        assert!(ha.starts_with("sha256:"), "bin_hash 必须标明算法: {ha}");
        assert_eq!(ha.len(), "sha256:".len() + 64);
        assert_eq!(
            ha,
            *fb.bin_hash.value().unwrap(),
            "同一内容放不同路径必须同哈希"
        );
        assert_eq!(
            ha,
            format!("sha256:{}", to_hex(&crate::sha256::sha256(&same))),
            "bin_hash 必须与直接对内容做 SHA-256 一致"
        );
        assert!(fa.build_time.value().is_some(), "构建时间必须有值");
        assert_eq!(fa.bin_path.value().unwrap(), &a.display().to_string());

        // 内容改一个字节 → 哈希必须不同。
        let mut one_byte_off = same.clone();
        one_byte_off[100_000] ^= 0x01;
        fs::write(&b, &one_byte_off).unwrap();
        let fb2 = collect(&repo, Some(&b));
        assert_ne!(
            fb2.bin_hash.value().unwrap(),
            &ha,
            "内容改一个字节，bin_hash 必须变"
        );

        fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn missing_bin_is_none_with_reason() {
        let repo = make_repo("nobin");
        let gone = repo.join("no-such-firmware.bin");

        let fp = collect(&repo, Some(&gone));
        assert!(fp.bin_hash.is_missing(), "文件不存在时 bin_hash 必须是缺失");
        let reason = fp.bin_hash.reason().expect("缺失必须带原因");
        assert!(
            reason.contains("no-such-firmware.bin"),
            "原因必须点明是哪个文件: {reason}"
        );
        assert!(fp.build_time.is_missing(), "mtime 同样取不到");
        assert!(!fp.build_time.reason().unwrap().is_empty());
        // 请求路径本身如实记录
        assert_eq!(fp.bin_path.value().unwrap(), &gone.display().to_string());

        // 目录不是构建产物
        let as_dir = collect(&repo, Some(&repo));
        assert!(as_dir.bin_hash.reason().unwrap().contains("不是普通文件"));

        fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn not_a_git_repository_reports_missing_with_reasons() {
        let dir = make_non_repo("nongit");
        let fp = collect(&dir, None);

        assert!(fp.repo_root.is_missing());
        assert!(fp.commit.is_missing());
        assert!(fp.dirty.is_missing());
        for (name, reason) in fp.missing() {
            assert!(!reason.trim().is_empty(), "{name} 的原因不得为空");
        }
        let commit_reason = fp.commit.reason().unwrap();
        assert!(
            commit_reason.contains("not a git repository") || commit_reason.contains("git"),
            "原因必须说明 git 侧失败: {commit_reason}"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn git_unavailable_yields_reasons_and_does_not_panic() {
        let repo = make_repo("nogit");
        let fp = collect_with_git("EMBHUB_NO_SUCH_GIT_PROGRAM", &repo, None);

        assert!(fp.repo_root.is_missing());
        assert!(fp.commit.is_missing());
        assert!(fp.dirty.is_missing());
        assert!(
            fp.commit
                .reason()
                .unwrap()
                .contains("EMBHUB_NO_SUCH_GIT_PROGRAM"),
            "原因必须点明用的是哪个 git: {:?}",
            fp.commit.reason()
        );
        fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn no_field_is_ever_silently_omitted() {
        let repo = make_repo("nosilent");
        let bin = repo.join("fw.bin");
        let mut f = fs::File::create(&bin).unwrap();
        f.write_all(b"firmware").unwrap();
        drop(f);
        let non_repo = make_non_repo("nosilent-nongit");

        let scenarios: Vec<(&str, FirmwareFingerprint)> = vec![
            ("仓库 + bin", collect(&repo, Some(&bin))),
            ("仓库无 bin", collect(&repo, None)),
            ("非 git 目录", collect(&non_repo, None)),
            (
                "git 不可用",
                collect_with_git("EMBHUB_NO_SUCH_GIT_PROGRAM", &repo, Some(&bin)),
            ),
        ];

        for (name, fp) in scenarios {
            // 1) 每个缺失字段都必须带非空原因
            for (field, reason) in fp.missing() {
                assert!(!reason.trim().is_empty(), "{name}: {field} 的原因为空");
            }
            // 2) 8 个契约字段必须各自「要么有值、要么带原因」，没有第三种状态
            let mut named: Vec<&str> = fp.missing().into_iter().map(|(f, _)| f).collect();
            for (field, present) in [
                ("repo_root", fp.repo_root.value().is_some()),
                ("commit", fp.commit.value().is_some()),
                ("dirty", fp.dirty.value().is_some()),
                ("bin_path", fp.bin_path.value().is_some()),
                ("bin_hash", fp.bin_hash.value().is_some()),
                ("build_time", fp.build_time.value().is_some()),
                ("chip", fp.chip.value().is_some()),
                ("probe_id", fp.probe_id.value().is_some()),
            ] {
                if present {
                    named.push(field);
                }
            }
            named.sort_unstable();
            named.dedup();
            assert_eq!(named.len(), 8, "{name}: 字段账目对不上: {named:?}");
        }

        fs::remove_dir_all(&repo).ok();
        fs::remove_dir_all(&non_repo).ok();
    }

    #[test]
    fn chip_and_probe_id_are_explicitly_missing_until_phase7() {
        let repo = make_repo("phase7");
        let fp = collect(&repo, None);
        assert!(fp.chip.is_missing() && fp.probe_id.is_missing());
        assert!(fp.chip.reason().unwrap().contains("Phase 7"));
        fs::remove_dir_all(&repo).ok();
    }
}
