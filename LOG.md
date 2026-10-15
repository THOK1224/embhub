# LOG — embhub 施工日志

记录每一轮做了什么、为什么这么做、验证了什么。新记录追加在**最上面**。

---

## 2026-10-16 · 第 9 轮：增量 C1（core 流式管线 + 背压）交付并验收通过

`t7`（implementation，`coder-ds`）一次 attempt 完成；captain 建 `t8`（review，**带 t7 依赖**以防被抢）并亲自承担，裁决 **pass**。

### 交付内容（3 个文件，全在 inScope）

| 文件 | 内容 |
| --- | --- |
| `crates/embhub-core/src/pipeline.rs`（新） | 常驻读取线程：**到达点时间戳原样搬运** → **先 append 落盘**（失败即致命并记账）→ 显示旁路按「≥8 KiB 或 ≥30 ms 先到先发」攒批、用 `try_send` 投入 `sync_channel(display_capacity)`，满/断开即丢批次并计 `batches_dropped` / `display_bytes_dropped`；`stop()` 幂等（join+seal）、`stats()` 停后可读、`Drop` 兜底 |
| `crates/embhub-core/src/lib.rs` | 注册并再导出 |
| `crates/embhub-cli/src/main.rs` | `stream --bytes N [--chunk K] [--consume fast\|slow\|stalled] [--display-capacity C]`，**退出码只看落盘零丢失** |

### captain 的两项独立验证（这是本轮的重点）

**① 对抗性测试：`--display-capacity 0`**（投递通道零缓冲，显示路径理论上一个字节都收不到）

```
batches_emitted 0 / batches_dropped 1024 / display_bytes_dropped 67108864（显示 100% 丢弃）
logged_bytes = replay_bytes = source = 67108864      exit 0
```

**显示路径全军覆没、落盘路径零丢失** —— 这是 SPEC 4.3 那条硬约束最强的直接证据，不是推理。账目也自洽：`4194304 + 62914560 = 67108864`（容量 64 时）、`0 + 67108864 = 67108864`（容量 0 时）。

**② 完全绕开 core 的磁盘独立解析**

实现者提醒「CLI 的 `replay_bytes` 与 `stats` 同源，算自证」，建议直接数 `seg-*.bin`。它说得对，而且它自己**只提议、未执行**——captain 做了：

跑 512 MiB stream 期间持续镜像在途会话目录，再用**纯 PowerShell 按 16 字节记录头（`t_mono u64 | len u32 | crc u32`）自行遍历分片字节**：

| 项 | 值 |
| --- | --- |
| 分片数 / 分片总字节 | 65 / 537,001,984 |
| 记录条数 | 8192 |
| payload 总字节 | **536,870,912** |
| 账目 16×8192 + payload + 尾部 | 537,001,984 == 分片总字节 ✓ |
| 到达时间戳单调不减 | True |
| **与源产出对比** | 源 536,870,912，读出 payload 536,870,912，**差 = 0** |

**这不再是程序自报，而是从磁盘原始字节里数出来的零丢失证据。**

### 裁定实现者上报的两点

1. **`stop()` 语义为「现在就停」—— 接受。** 实现者在开发中真实踩到竞态（`spawn` 后立刻 `stop()`，读取线程可能首轮就观察到停止标志而提前收尾，`source_bytes` 只读到一半）。它**没有掩盖**，而是新增 `wait_until_finished(timeout)`（Condvar，不忙等）把「等自然结束」与「现在就停」分成两个命名明确的方法。这与 SPEC 3.4「停止即释放句柄与线程」一致，停止按钮也不应阻塞——两个清晰方法优于一个含糊方法。
2. **既有测试含性**：实现者做了机械差集；captain **独立重做一遍**（上轮 41+12 的名字逐条在本轮 50+17 中查找），结论一致：零删除、零改名，新增 14 条（9 pipeline + 5 stream）。

### 流程：评审任务第 4 次被抢，但这次成员正确拦截

`t8` 建好后仍被调度器派给了 `coder-ds`。它**拒绝自审**、没有跑任何 verify、没有产出任何可被误读为评审证据的东西，而是把「该任务不可由我执行」登记为 finding（severity high）并把任务标 failed，再请 captain 重派——处理得完全正确。captain 随即 reassign 给自己完成 attempt 2。

> 说明：本轮我确实按上一轮的改进**给评审任务加了 `t7` 依赖**，但 `t7` 一完成、`t8` 变可就绪，调度器依然抢先派给了空闲成员。看来"提前建 + 加依赖"不足以避免，**下次改为：任务一就绪就立刻 reassign，接受这一秒的竞态**，或接受成员主动拦截这一层兜底（本轮证明这层兜底是有效的）。

### 下一步：增量 C2

把 GUI 接到这条管线上：`start_session` / `stop_session` / 会话统计事件 + 实时显示，用合成源驱动（不依赖硬件）。C1 已经把 core 侧全部做完并验证，C2 只动 `crates/embhub-app`。

### 附带交付：验收工具（应维护者要求）

维护者要求"把验收清单以及验收成功的标准发给我"，据此新增两件东西：

- [`docs/ACCEPTANCE.md`](docs/ACCEPTANCE.md) —— 验收清单：15 条自动项 + 3 条人工项，**每条都给原始命令、期望输出、通过标准**，并列出"什么情况下应该推翻我的结论"（可证伪点）。
- [`scripts/accept.ps1`](scripts/accept.ps1) —— 一条命令跑完 15 条自动项并逐条打印 PASS/FAIL。**纯 ASCII、PS 5.1 兼容**（三元运算符 `? :` 是 PS7+ 才有的，第一版写错已修；`деп` 误入注释也已清除——非 ASCII 字节数现为 0）。维护者终端可直接 `. .\scripts\accept.ps1`；captain 在沙箱内用 `iex` 自测通过：**15 passed, 0 failed**。

设计要点：脚本只是**便利**，每条检查都能手敲复现（文档给了原始命令），所以维护者不必信任脚本本身；脚本刻意不截断被验命令的输出管道（截断会伪造 `exit=-1`）。

---

## 2026-10-16 · 第 8 轮：增量 B2（指纹写进会话头 / 日志格式 v1→v2）交付并验收通过

`t5`（implementation，`coder-ds`）一次 attempt 完成；captain 建 `t6`（review）并亲自承担，裁决 **pass**——且本次裁决**覆盖当前字节**（框架已提示 t2 的旧裁决所依据的字节已被后续增量改掉）。

### 交付内容

| 项 | 内容 |
| --- | --- |
| 格式 | `FORMAT_VERSION = 2`；头布局 `magic[8] \| version u32 \| start_wall_ms u64 \| fp_len u32 \| fp_json[fp_len] \| cfg_len u32 \| cfg` |
| 指纹段 | 新增 `FingerprintSection { collected: Option<FirmwareFingerprint>, not_collected: Option<String> }`，不变量「**恰好一个有值**」在写入与读取两端都校验 |
| v1 处理 | 明确拒绝并报「日志格式 v1 已废弃，需 v2…」，**绝不按 v2 解析**；未知未来版本单独分支 |
| 端到端 | `selftest --repo <dir> [--bin <file>]`：采集 → 写进 v2 头 → 落盘 seal → **从磁盘 header.bin 读回** → `diff_fingerprint()` 逐字段比对，不一致非零退出 |
| 依赖 | `serde_json` 从 dev-dependencies 移到 dependencies —— **`git diff Cargo.lock` 完全为空**（dev-dep 本就进锁文件，连依赖边都没变） |

### captain 独立验收（不采信自述）

- 复跑 9 条 verify 全部 exit 0：core **41 passed**、cli **12 passed**、clippy/fmt 干净、`selftest --repo` exit 0（**版本=2 + 8/8 字段比对**）、`selftest` 无 `--repo` exit 0（行为不变且记录未采集原因）、`fingerprint`/`list`/`build -p embhub-app` 均 exit 0。
- 加测 `selftest --bytes 268435456`（256 MiB）→ exit 0，8/8 字段比对通过，闭环 268435456 字节（27.5s）。
- **captain 自己做的字节级独立校验（不经任何 Rust 代码）**：对一个真实落盘的 `header.bin` 用 PowerShell 按偏移解析 → 总长 **558**、magic=`EMBHLOG\0`、version=2、fp_len=486、cfg_len=44、source_config 紧随其后，**账目 24+486+4+44 = 558 与实际文件长完全相符**；指纹段能被 .NET `ConvertFrom-Json` 解析，`collected` 下 8 字段齐全（`chip`/`probe_id` 为 `Missing` 且带 Phase 7 原因）。**这条排除了「只在内存里比对、磁盘上其实没有指纹」这一最阴险的失败模式。**
- 读码确认比对对象确实来自磁盘：`run_selftest` 在 `seal` 之后调 `read_session_header(&dir)`，而该函数内部是 `fs::read(.../header.bin)`。

### 裁定：实现者上报的既有测试改动

`session_header_roundtrip_contains_all_contract_fields` 改了断言。它原来断言「32 字节全零指纹占位存在」——而那个占位**正是本增量要替换掉的对象**，v2 下已无意义；且它不在 captain 点名保护的 7 条之内。

**裁定：接受。** 替换后的断言**严格更强**：不止断言未采集时 `collected.is_none()` 且必带原因，还**直接读原始字节**验证 magic 位置、版本字段==2、`fp_len>0`、指纹段是合法 JSON 且含 `collected`/`not_collected` 两键、且 `fp_len != V1_FINGERPRINT_PLACEHOLDER_LEN`。

被点名保护的 7 条（`empty_session_replays_zero_bytes`、`exactly_one_segment_when_exact_fit`、`rolls_over_across_segment_boundary`、两条 `tampered_*`、`replay_is_byte_for_byte_identical_to_injected`、`core_has_no_tauri_dependency`）逐条确认存在且通过。

### 流程问题（第 3 次同类）

评审任务**又一次**被调度器抢先派给实现者本人（t2/t4/t6 各一次），captain 每次都要立刻 `reassign_task` 收回，代价是白中断成员一次。**下次改为：captain 在实现任务完成前就先把评审任务建好并收回给 captain。**

另：`target\tmp` 下留有实现者做独立校验的产物与一个未清理的会话目录（`embhub-store-test-v1-reject-*`）。`target/` 已被 `.gitignore` 覆盖，无仓库影响，属卫生问题。

### 下一步：增量 C

Phase 1 还差**把真实串口接进这条管线**：目前 `SerialSource` 已在 `core` 里，但 GUI 仍用 spike 的阻塞式 `read_probe`，且没有「会话」对象、没有批量事件推送与背压。增量 C = 常驻读取线程 + 批量推送 + 背压 + 用合成源驱动 GUI（不需要硬件即可验收）。

---

## 2026-10-16 · 第 7 轮：增量 B1（固件指纹采集）交付并验收通过

`t3`（implementation，assignee `coder-ds`）一次 attempt 完成；captain 建 `t4`（review）并亲自承担，裁决 **pass**。

### 交付内容（4 个文件，全在 inScope）

| 文件 | 内容 |
| --- | --- |
| `crates/embhub-core/src/sha256.rs`（新） | **自行实现**的流式 SHA-256，纯 std、零新依赖；用 FIPS 180-4 标准向量锁死 |
| `crates/embhub-core/src/fingerprint.rs`（新） | SPEC 3.5 采集：`Field<T> { Value(T), Missing(String) }`；`collect()` 永不 panic；commit 强制 40 位 hex 校验；`dirty` = `git status --porcelain` 非空；`bin_hash = sha256:<64hex>` 流式算内容；`build_time` = 产物 mtime（Unix ms） |
| `crates/embhub-core/src/lib.rs` | 注册模块 + 再导出 |
| `crates/embhub-cli/src/main.rs` | 新增 `fingerprint --repo <dir> [--bin <file>]` |

### captain 独立验收（不采信自述）

- 复跑 8 条 verify：core **35 passed**、cli **10 passed**、clippy `-D warnings` exit 0、fmt `--check` exit 0、`fingerprint` exit 0、`selftest --bytes 1000000` exit 0、`list` exit 0（10 口）、`build -p embhub-app` exit 0。
- **自行增加的交叉校验**：对同一文件，embhub 报 `sha256:b9318235052d3442351f8e523e1d7353a8ae895712eebf6e0ba6ca145abb0784`，Windows `Get-FileHash -Algorithm SHA256` 得到**完全相同**的字符串 → 自实现 SHA-256 与系统实现在真实文件上一致。**这是本项目第一次用外部实现校验自己的实现。**
- 代码级复核：`Field<T>` 只有 `Value` / `Missing(String)` 两态且 `Missing` 必携原因 → 「静默省略」**在类型上不成立**（不是靠约定）；`make_repo` 真跑 `git init/add/commit`（身份用 `-c` 传参，**不写全局配置**）；`make_non_repo` 写坏 `.git` 文件并注释了原因。
- 依赖未变：`Cargo.lock` 仍是 108752 B / mtime 19:01:47。

### 裁定：实现者提出的测试口径问题

实现者问：临时目录在工作区仓库内，git 会对「没有 `.git` 的子目录」**向上**认成工作区仓库，所以「非 git 目录」测试改用「`.git` 文件指向不存在路径」。

**裁定：接受，不补测。** 三条理由，其中两条 captain 自己验过：① 独立复现了 git 向上查找（`target\tmp\notarepo` 的 `rev-parse --show-toplevel` 返回工作区根）；② 沙箱只允许工作区内写、而工作区本身是 git 仓库 → 「完全不在任何仓库内」的目录**物理不可达**；③ 坏 `.git` 构造让 git 确定性报 `not a git repository` 且不回退，走的正是待测错误路径。

### 两条流程问题（与实现质量无关）

1. **评审任务被调度器抢先派给实现者（本会话第 2 次，t2/t4 各一次）**。评审任务建到共享池后，空闲成员会立即抢单——那正是「自己评审自己」的无效情形；captain 每次都要立刻 `reassign_task` 收回，代价是**白中断成员一次**。**后续做法：评审任务建好后立即 reassign，或提前建好。**
2. **captain 第 3 次同类失误**：又用 `Select-Object -First` 截断被验命令的输出管道，得到 `exit=-1` 并一度误读为失败（`list` 一次、`selftest --bytes 0` 一次、本轮 `list` 一次）。**规则化：验证命令不得截断被验命令的输出管道。**

### 下一步

增量 **B2**：把指纹真正写进 `LogStore` 会话头。当前会话头里的指纹字段是 32 字节占位（`FORMAT_VERSION = 1`），而指纹含 commit(40 hex) + dirty + bin_hash(64 hex) + build_time，**32 字节装不下**，因此 B2 需要把格式升到 v2、改为长度前缀的指纹段。这是 B2 唯一的格式级决策。

---

## 2026-10-16 · 第 6 轮：Phase 0 全部通过 + 交付门清除 + 开工增量 B1

### Phase 0：三条判据全部通过

用户用真实板子（STM32G474VET6 + USART1 测试固件，协议见 [`docs/test/PROTOCOL.md`](docs/test/PROTOCOL.md)）实机验证：

| 判据 | 结果 |
| --- | --- |
| 出窗口 | ✅ |
| **插拔串口后列表刷新** | ✅ 插入后 10 → 11 个口（COM13 出现），拔掉后回到 10 个 |
| **115200 收到真实数据** | ✅ |

**因此 Phase 0 结论：通过。技术路线（Tauri v2 + Rust）不再有未验证的前提。** 先前「用户主动延后」的两条已关闭。

> 本轮为定位「收不到数据」排查过一轮：用独立的 .NET `SerialPort` 实现（DTR/RTS 显式关闭）做对照，同样读不到、发 PING 帧也无应答，据此排除「我们工具的问题」。最终原因是**板子供电不足**——USB-TTL 只给 TX/RX/GND 时电流不够，板子起不来，现象与"收不到数据"一致。**结论已归档，无需再排查。**

### 一条对后续有用的事实：本机串口大多是"假的"

用注册表 `HKLM\HARDWARE\DEVICEMAP\SERIALCOMM` 挖出各 COM 口的真实设备路径：

```
\Device\BthModem0..9  <-  COM3..COM12（10 个全都是蓝牙调制解调器，虚拟口）
\Device\Serial2       <-  COM13（真实串口设备）
```

这解释了为什么 `list_ports()` 把它们全报成 `port_type = unknown`（没有 USB VID/PID）。**以后做端口过滤/自动识别时，不要假设 COM 号连续或都存在真实设备。**

### 交付门清除（补做正式评审）

增量 A 交付后，团队 loop 报 `blocked: completed implementation has no passing review`。captain 建了正式评审任务 `t2`（kind=review，reviewedTaskId=t1），并**由 captain 本人承担**——依据是用户明确的职责划分「主代理负责最后的验收」，且实现者是 coder-ds，满足「评审者不得是实现者」。

- 流程插曲（如实记录）：`t2` 建到共享池后被调度器**自动派给了 coder-ds**（attempt 1），那正是"自己评审自己"的无效情形；captain 立即用 `reassign_task` 收回（attempt 2）并完成评审。coder-ds 那次 attempt 被中断，无产出损失。
- **裁决：pass**。完整证据见 `t2` 的 output。

### 增量 B1 已开工

`t3`（kind=implementation）已派给 `coder-ds`，内容：按 SPEC 3.5 实现 `FirmwareFingerprint` 采集（git `rev-parse`/`status --porcelain` 取 commit/dirty，构建产物取**内容**哈希，记录 build_time），并在 `embhub-cli` 增加 `fingerprint` 子命令。**明确不做**：不改日志存储格式（写入会话头留到 B2）、不采集 chip/probe_id（Phase 7）、不新增第三方依赖。

关键验收约束：`dirty` 测试必须**自建临时 git 仓库**（不得依赖本工作区状态）；`bin_hash` 必须是内容哈希（同内容不同路径相同、改一字节不同）；任何取不到的字段必须为 `None` + 原因字符串，**不得静默省略**。

---

## 2026-10-15 · 第 5 轮：Phase 1 增量 A 验收通过（「零丢失」最小闭环）

### 换人决策

kimi（`moonshotai-cn` / `kimi-k3`）累计 **6 次 attempt 全部 429** 失败。项目限速经用户截图确认为**项目级** `并发数 = 1 / RPM = 3 / TPM 500000 / TPD 1500000`。

需要记住的两条经验（都已在上文第 4 轮留证）：

1. **kimi 不是"跑不动"，而是"跑得慢"**——我先前"无法完成任何有意义回合"的判断被 attempt 4 证伪（它活了 6 分钟，产出 38 KB 代码 + 21 个测试）。
2. **失败会累积进度**：成员与 captain 共用文件系统，每次失败留下的文件就是下一次的起点。因此正确策略是「失败 → 等足够长的间隔 → 重试」，而不是立刻重试（实测：间隔 ~10 分钟的 attempt 3/4 有产出，间隔 ~4–6 分钟的 attempt 5/6 产出为 0）。

用户最终决定换 `deepseek-official`。操作：新增成员 `coder-ds`（`deepseek-official` / `deepseek-flash`），把 `t1` 转交（attempt 7），移除 kimi 成员 `coder`。**前一位留下的成果全部保留、未重写**（我在 `coder-ds` 的执行提示里写明了现状与"不要重写"）。

### 交付与 captain 独立验收

`coder-ds` **一次 attempt 即完成**。captain **没有采信成员自述**，亲自复跑全部验收命令，并额外做了成员未声称的加测与代码级复核。

| 验收项 | captain 实测结果 |
| --- | --- |
| `cargo test -p embhub-core` | **24 passed / 0 failed**，exit 0 |
| `cargo test -p embhub-cli` | **8 passed / 0 failed**，exit 0 |
| `clippy --all-targets -- -D warnings` | exit 0，无 warning |
| `fmt --all --check` | exit 0，无 diff |
| `selftest --bytes 67108864` | exit 0；注入 == 落盘 == 回放 == **67108864**；逐字节比对一致；磁盘实测 67125248 |
| `list` | exit 0，10 个串口 |
| `build -p embhub-app` | exit 0（GUI 未被弄坏） |

**captain 额外加测（成员未声称）**：

- `selftest --bytes 1000000 --seed 7` → exit 0，三计数相等，磁盘 `1000256 = 1000000 + 16×16`（证明不是只对 64 MiB 硬编码）。
- `selftest --bytes 0`（边界）→ exit 0，三计数均为 0，打印闭环通过。
- `selftest --bytes abc` → exit 1（参数校验路径）。
- 磁盘字节独立验算：`67108864 + 1024×16 = 67125248` ✓ 与程序自报一致。

**代码级复核（不是只跑测试）**：

- 两条 CRC 篡改测试**是真篡改**，不是同义反复：`tampered_payload_byte_is_detected_by_crc` 实读 `seg-000000.bin`、翻转 payload 首字节、写回，再断言 `open_replay` 返回 `Err(Fatal)` 且消息含 `CRC32`；`tampered_byte_in_later_segment_is_detected_by_crc` 改**第二片**末字节，先断言 `segments >= 3`，再断言必须报错而非静默结束、报错前吐出字节不超过注入总量。
- `verdict()` 是五个条件的**纯合取**，无短路花招；`false → ExitCode::FAILURE` 一行映射（已读码确认）。

**残留缺口（成员主动披露，captain 判定可接受）**：「运行时计数真的对不上 → 非零退出」只有 `verdict()` 单测覆盖，**没有端到端故障注入**。判定理由：为一个恒真的单行映射在生产代码里加故障注入钩子，设计上比「纯函数单测 + 读码确认映射」更差。此缺口如实记录。

**范围合规**：实际改动仅 4 个文件（`cli/main.rs`、`core/lib.rs`、`core/source.rs`、`core/store.rs`），与声明一致；`Cargo.toml`/`Cargo.lock` 未被改动（lock 大小 108752、mtime 19:01:47，早于本轮）；`docs/**`、`scripts/**`、`crates/embhub-app/**` 未被触碰。新增第三方依赖 **0**；core manifest 无 tauri（另有测试 `core_has_no_tauri_dependency` 通过）。

### captain 自身的三次验证失误（如实记录）

1. 用 `cargo build` 代替 `cargo test` 判断"半成品可编译"，漏掉了测试侧的 `E0616`（`va.len` 漏括号）。**教训：验收必须跑 `cargo test`，`build` 不编译测试代码。**
2. 两次把 `Select-Object -First` 截断管道造成的 `exit = -1` 误读为程序失败（`list`、`selftest --bytes 0` 各一次），不截断复测均为 0。**教训：验证命令不要截断被验命令的输出管道。**

这两条都是 captain 的方法论错误，不是实现缺陷，记在此处以免下次重犯。

### 遗留事项

1. **工作区的 git 仓库由用户本人创建**（提交 `85adb45 "init"`）。captain 曾怀疑是 kimi 成员越界执行 `git init`，**经用户确认不是，该怀疑已撤回**——成员的范围合规性没有问题。captain 已把 `.agent-teams/` 与 `*.locked-*` 补进 `.gitignore`；工作树仍有 11 项未提交改动（随用户暂停开发而暂不提交）。
2. `target/debug/embhub-app.exe.locked-20261015`（12.7 MB）——产物改名残留，无害垃圾。
3. 19:10 启动的旧 spike GUI 进程**已自行退出**，先前 `os error 5` 的产物占用问题不复存在。

### 本轮结束后：项目暂停（用户决定）

用户选择**暂停开发**，先去接 USB 串口设备，把 Phase 0 的两条待验项（插拔后列表刷新、115200 收到真实数据）补验完再继续。**通过则进入 Phase 1 增量 B（固件指纹采集），有问题则按 PLAN 第 6 节切换判据回 Python 路线。**

captain 在暂停前给出的待验清单与一处**必须提醒的约束**：当前 `embhub-cli` **没有发送能力**（只有 `list` / `read` / `selftest`），GUI 的 `read_probe` 也只读不发。因此：

- 若要验"收到真实数据"，**必须有一个真正在发数据的对端**（板子开机打印、或另一个串口程序在发）；
- **单纯的 TX-RX 短接回环测不出来**——因为本工具不发，回环回来的还是空的；
- 若用户手上只有 USB-TTL 适配器而没有发送端，需要先给 CLI 加一个 `send` 子命令（预估 5 分钟工作量）才能自测接收通路。**等用户明确要求再做。**

---

## 2026-10-15 · 第 4 轮：Phase 0 通过 + 启动 Phase 1 增量 A（被 kimi 配额卡住）

### Phase 0 spike：通过

用户在普通终端 `cargo run -p embhub-app` 后目视确认：窗口标题正确、日志显示「发现 10 个串口：COM8, COM10, …」、点「刷新端口」产生第二条枚举记录。**这证明的不只是"窗口能开"**，而是整条 IPC 通路（前端 `invoke` → Rust command → `serialport`）是通的。

第 2、3 条（插拔刷新 / 115200 收真实数据）按用户决定标为**「用户主动延后」**，原因是他手头没有 USB 设备。**未通过，也未被证伪**；用户表示后续自验，有问题则重新选路线。

### 本轮新增的基础设施

`scripts/cargo.cmd` —— cargo 包装器。**起因是一个会让子代理直接卡死的坑**：沙箱里点源 `scripts/env.ps1` 会被 PowerShell 执行策略拦（`AuthorizationManager 检查失败`），而 `pwsh -File` 又不保留环境变量。`.cmd` 子进程不受执行策略约束，因此用它把 `RUSTUP_HOME`/`CARGO_HOME`/`TMP` 一次设好。
实测：`--version` 正常、`test -p embhub-core` 全绿、错误退出码 101 正确透传。

### 顺手修正的一处自身错误

先前的 README / PLAN / LOG 都建议「被执行策略拦时用 `pwsh -ExecutionPolicy Bypass -File .\scripts\env.ps1`」——**这条是错的**：那是子进程，环境变量不会留在用户终端里。已全部改为点源 `. .\scripts\env.ps1`，并注明必须先 `Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass`。

另外确认：**本会话的 shell 是 Windows PowerShell 5.1**（`5.1.26100.4202`，Desktop），`pwsh` 不在 PATH。5.1 对无 BOM 的 `.ps1` 按 ANSI(GBK) 解码 → 中文注释会被解错（实测 `iex` 直接抛解析错误）。因此 `scripts/env.ps1` 已改为**纯 ASCII**。

### 团队工作方式的确定

用户要求：**主代理（deepseek）负责规划与最终验收，子代理（kimi-k3）负责中间代码编写**。

据此建立 AgentTeams 团队 `embhub-zero-loss`，成员 `coder` = `moonshotai-cn` / `kimi-k3`，任务 `t1`（kind=implementation）实现 Phase 1 增量 A：

> ByteSource 接口（SerialSource + SyntheticSource）+ LogStore 写入/回放路径 + `cli selftest`，用合成源证明「注入 == 落盘 == 回放」，不依赖硬件、不新增第三方依赖。

**计划已经用户批准。** 契约要点：验收就是一条命令（`selftest --bytes 67108864` 打印三个相等计数）、6 条 verify 命令由 captain 亲自跑、明确禁止新增依赖（沙箱无网络，新增依赖会让整个 workspace 解析失败）、`crates/embhub-app/**` 划为 outOfScope。

### 阻塞：kimi 的组织级限流

`coder` 连续两次 attempt 均以**同一错误**不可恢复失败：

```
429: request reached organization max RPM: 3, please try again after 1 seconds
(type: rate_limit_reached_error)
```

**事实**：该 key 的组织配额是 `max RPM: 3`（每分钟 3 次模型请求）。agentic 编码回合至少需要十几次请求（读 SPEC、读代码、写 3~4 个文件、跑若干命令）。

**判断（有证据，但非定论）**：在 3 RPM 下，kimi 无法作为自主编码代理完成任何有意义的回合——第 4 次调用必然 429，而 runtime 的重试预算远小于等待窗口所需的耐心。这与代码难度无关，是配额结构问题。

**已做**：重试一次确认（attempt 2，同样失败）。用户选择「继续等/重试 kimi」，因此已安排 10 分钟后的定时重试，并设定硬边界：**若第三次仍失败则停止自动重试**，要求用户三选一——提高 Moonshot 组织配额 / 换 coder 模型 / 明确请求 captain 接手。

**用户核对结果**：截图确认项目限速就是 `并发数 = 1 / RPM = 3 / TPM 500000 / TPD 1500000`，是**项目级**配置（面包屑 `个人组织 > default`，左侧有「项目设置」）。`kimi-k3` 这个 model id 是**对的**——成员成功 spawn 过，从没报 id 错误。

### 关键修正：kimi 并没有"跑不动"，只是"跑得慢"，而且**进度会累积**

前三次 attempt（1/2/3）都在启动阶段几十秒内被 429 打回，我据此判断"kimi 无法完成任何 agentic 回合"——**这个判断被 attempt 4 证伪了，在此更正**。

attempt 4 于 20:19 派发（排在一个 10 分钟等待窗口之后），**活了约 6 分钟，写出了 38 KB 真实代码**：

| 文件 | 大小 | 说明 |
| --- | --- | --- |
| `crates/embhub-core/src/crc.rs` | 1,486 B | 自实现 `crc32`（**遵守了"禁止新增依赖"约束**） |
| `crates/embhub-core/src/source.rs` | 9,496 B | `ByteSource` trait、`SourceError`、`Block`、`MonoMicros`、`SyntheticSource`/`SyntheticConfig`、`SerialSource`/`SerialConfig` |
| `crates/embhub-core/src/store.rs` | 28,808 B | `HEADER_MAGIC = b"EMBHLOG\0"`、`FORMAT_VERSION = 1`、`create_session`/`append`/`seal`/`stats`/`open_replay`、`read_session_header`、`ReplaySource`、`SessionHeader/Meta/Stats` |
| `crates/embhub-core/src/lib.rs` | — | 已声明 `pub mod crc/source/store` |

`cargo build -p embhub-core` → **exit 0**（半成品可编译）。

**由此得到一条对工作方式有决定性影响的事实**：成员与我共用文件系统，**每次失败留下的文件都是下一次的起点**。所以正确策略不是"换个能跑的模型"，而是**「失败 → 等一个短窗口 → 重试」**，让进度跨 attempt 累积。

**经验证据（用于选择重试间隔）**：立即重试（attempt 2、3）几十秒即死；间隔 10 分钟的重试（attempt 4）跑了 6 分钟。因此重试必须**带间隔**，不能连发。

**剩余工作**（截至 20:27）：`crates/embhub-cli/src/main.rs` 的 `selftest` 子命令、以及验收要求的测试（空输入 / 跨分片 / CRC 篡改检出 / 回放逐字节一致）。

### 重试节奏与剩余工作量（20:31 更新）

attempt 4 实际不止写了三个模块，还**写了 21 个测试**：`store.rs` 12 个、`source.rs` 6 个、`crc.rs` 3 个 `#[test]`。也就是说剩余工作量比我先前估计的更小——**只差 `crates/embhub-cli/src/main.rs` 的 `selftest` 子命令**。

**重试间隔与产出的关系（实测，用于决定节奏）**：

| attempt | 距上次间隔 | 产出 |
| --- | --- | --- |
| 3 | ~10 分钟 | `crc.rs`（1.5 KB） |
| 4 | ~9 分钟 | 38 KB 代码 + 21 个测试 |
| 5 | ~4 分钟 | **0（几秒即 429）** |

结论：**间隔必须够长，立即或短间隔重试是纯浪费**。据此固定使用 10 分钟间隔。

**待处理**：`t1` 状态 failed（attempt 5），已排 10 分钟后的第 6 次重试（`schedule-cdf10578`）。已用完 3 次承诺重试中的 1 次；**第 6 次是最后一次自动重试**，仍不完成则停下交用户决定（换 `deepseek-official` 等）。

---

## 2026-10-15 · 第 3 轮：恢复 + 清阻塞 + Tauri 编译通过 + 沙箱挡住 GUI

### 恢复（强制关机之后）

| 检查项 | 结果 |
| --- | --- |
| 源码/文档 | 完好 |
| 工具链 | cargo/rustc 1.99.0 可用；`.cargo` 208 MB、`.rustup` 590 MB |
| 已有产物 | `target/debug/embhub-cli.exe`（0.37 MB） |
| registry 缓存 | 17 个文件，其中 **tauri 相关 0 个** → 上次的依赖下载确实从未发生 |
| `Cargo.lock` | **不含 tauri** → 已过期 |
| 探针残留 | `probe-tmp/`、`.tmp/` 均已清除 |
| 磁盘 | 113.71 GB |
| 后台任务 | 无 |

**恢复时发现的两条新事实**（比上次更严重一层）：

1. 加入 `embhub-app` 成员后，`cargo test --offline` **对整个 workspace 失败**（`no matching package named 'tauri' found ... required by embhub-app`）——**连 core/cli 的离线测试都被拖累**，不只是 app 编不了。
2. `rustfmt` / `clippy` **没装**（当初用 `--profile minimal`），`rustup component add` 同样需要网络。

→ 结论：只剩一个动作需要联网（拉依赖 + 装两个组件），做完后日常编译/测试都不需要网络。

### 用户授权的一次联网构建（结果）

| 步骤 | 结果 |
| --- | --- |
| `rustup component add rustfmt clippy` | ✅ 成功 |
| `cargo fetch` | ✅ 成功（tauri 2.12.4 等进入工作区内 CARGO_HOME） |
| `cargo test -p embhub-core -p embhub-cli` | ✅ **全绿**（2 passed：`list_ports_is_callable`、`port_info_serializes`） |
| `cargo build -p embhub-app`（首次） | ❌ `icons/icon.ico` not found; required for generating a Windows Resource file during tauri-build |

### 修复动作与验证

**修复 1：缺图标。**
用 DSH 捆绑的 Python + Pillow 生成 `crates/embhub-app/icons/icon.ico`（16/32/48/64/128/256 六种尺寸），脚本落在 [`scripts/make_icon.py`](scripts/make_icon.py)。
→ 重编 **成功（exit 0，34.52 s，无需提权）**，同时证实了「依赖缓存后离线可编译」。

**修复 2：WebView2 数据目录（本轮最有价值的根因定位）。**

首次运行崩溃：`Failed to setup app: 拒绝访问。(os error 5)`，位置 `tauri-2.12.4/src/app.rs:1444`（即 `setup()` 里建窗口那一步）。

读 Tauri 源码定位到确切行——`tauri-2.12.4/src/manager/webview.rs`：

```rust
// 559-564：data_directory 为空时强制填 app_local_data_dir()
if pending.webview_attributes.data_directory.is_none() {
  match manager.path().app_local_data_dir() {   // → %LOCALAPPDATA%\dev.embhub.spike
    Ok(user_data_dir) => pending.webview_attributes.data_directory = Some(user_data_dir),
// 571-576：不存在就创建它
if let Some(user_data_dir) = &pending.webview_attributes.data_directory {
  if !user_data_dir.exists() {
    create_dir_all(user_data_dir)?;             // ← os error 5 就是这一行
```

- 对照实验：改 `LOCALAPPDATA` 环境变量**无效**——WebView2/known-folder 用 `SHGetKnownFolderPath`，**不读环境变量**；三处候选目录始终未被创建，与「卡在 `create_dir_all`」完全吻合。
- Tauri 的配置项 `dataDirectory` **只接受相对路径**（相对 `%LOCALAPPDATA%\<identifier>` 解析；绝对路径被忽略并告警，见 `tauri-utils-2.10.2/src/config.rs:417-424`），所以走配置解决不了。
- **修法**：在 `.setup()` 里用 `WebviewWindowBuilder::data_directory(<绝对路径>)` 建窗口（`tauri.conf.json` 的 `windows` 置空），数据目录默认取 exe 同级 `embhub-data/`，可用 `EMBHUB_DATA_DIR` 覆盖。
- **附带收益（不是巧合）**：这条正好满足 SPEC A8「绿色便携」——数据放在 exe 旁边、可整体拷走、不往系统盘写。产品约束与沙箱限制同向，所以这个修法不是 workaround。
- 修复后 `target/debug/embhub-data/EBWebView` **成功生成（115 项）**，WebView2 初始化通过。

### 仍未解决：GUI 在沙箱下起不来

修好数据目录后，报错推进到：

```
Failed to setup app: runtime error: failed to create webview:
WebView2 error: WindowsError(Error { code: HRESULT(0x8000FFFF), message: "灾难性故障" })
```

- 试过 `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS="--no-sandbox --disable-gpu --disable-software-rasterizer"`：**报错一字不变**。
- 观察：`msedgewebview2.exe` 子进程**确实被拉起来了**（3 个），但 controller 创建仍失败；这些进程随后自行退出，**无残留**。
- 判断：**沙箱限制（受限令牌 / 命名管道 / 窗口站）导致 WebView2 无法完成初始化**，不是配置问题。
- **结论（尚待在沙箱外验证）**：**GUI 窗口必须在沙箱外启动**。这与「pip 沙箱下 TLS 失败」「工作区外写入被拒」属同一类环境约束。

### 验收状态（诚实记录）

| spike 判据 | 状态 | 证据 |
| --- | --- | --- |
| 工具链可编译 | ✅ 通过 | `cargo build -p embhub-app` exit 0 |
| core/cli 测试全绿 | ✅ 通过 | 2 passed |
| 静态检查（SPEC A9） | ✅ 通过 | `cargo fmt --all --check` exit 0；`cargo clippy --all-targets -- -D warnings` exit 0（首次 fmt 有 3 处差异，已 `cargo fmt --all` 修正并重编） |
| 串口枚举 | ✅ 通过 | 10 个端口（COM3–COM12），类型均为 `unknown` |
| **出窗口** | ✅ **通过** | 用户在普通终端 `cargo run -p embhub-app` 后目视确认：窗口标题正确；日志显示「发现 10 个串口：COM8, COM10, …」；点「刷新端口」产生第二条枚举记录（19:46:59 自动 + 19:49:02 手动），证明 IPC 通路（前端 invoke → Rust command → serialport）通 |
| **插拔后列表刷新** | ⏸ **用户主动延后** | 手头无 USB 设备；用户表示后续自验，有问题则重新选路线 |
| **115200 收到真实数据** | ⏸ **用户主动延后** | 同上（无数据源）。**未通过，也未被证伪** |

### 本轮的诚实补充

- 我**无法在沙箱内程序化验证窗口**，所以「出窗口」这条必须由用户在沙箱外启动后目视确认。我不把「编译通过」当作「窗口可用」。
- 用户上次拒绝提权是因为要关机；本轮明确授权了一次联网构建。**GUI 启动没有再请求提权**，改为请用户在普通终端执行——这既符合已确认的合作方式，也顺便构成一次天然对照实验（沙箱外能起来 ⇒ 沙箱就是原因）。
- **本会话的 shell 是 Windows PowerShell 5.1**（`$PSVersionTable.PSVersion = 5.1.26100.4202`，PSEdition=Desktop），`pwsh` 不在 PATH。5.1 对**无 BOM 的 `.ps1` 按 ANSI(GBK) 解码**，中文注释会被解错，实测 `iex (Get-Content .\scripts\env.ps1 -Raw)` 直接抛 `意外的标记 }`。→ `scripts/env.ps1` 已改为**纯 ASCII**（非 ASCII 字节 0 个，`Parser::ParseFile` 解析错误 0 个）。Python 脚本不受影响（Python 3 源码默认按 UTF-8 解析）。
- **修正了先前文档里的一处错误建议**：`pwsh -File .\scripts\env.ps1` 是**子进程**运行，环境变量不会留在用户终端里；正确做法是点源 `. .\scripts\env.ps1`（被执行策略拦时先 `Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass`）。README / PLAN / LOG 已同步改正。

---

## 2026-10-14 · 第 2 轮：技术栈改回 Tauri v2 + Phase 0 spike（部分完成，卡在网络前置）

### 本轮做了什么

1. 并入旁路会话产出的决策记录 [`docs/DECISION-2026-10-14-stack-tauri-v2.md`](docs/DECISION-2026-10-14-stack-tauri-v2.md)：**D1′ = Rust core + Tauri v2 + 极简 TypeScript 前端**，`docs/PLAN.md` 升到 v0.3。
2. 清理第 1 轮遗留的 `probe-tmp/` 与 `.tmp/`（带 DENY ACE，提权后删除成功）。
3. 安装 Rust 工具链（**装进工作区内**，理由见下）。
4. 建 crate 骨架并写下 spike 代码。
5. 探测环境边界：WebView2、MSVC 链接、串口枚举、沙箱约束。

### 决策记录

- **D1′ 的前提已变更**：维护者只做**路线选择与行为验收、不读代码**；目标是小型、稳定、活动部件少。理由三条（环境熵、Node 在 Tauri 是构建期依赖而非运行期依赖、编译器在验收前挡一类错）。
- **我对该决策的两点补充，已写入 PLAN**：
  1. 编译器挡的是类型/借用错误，**不能替代行为验收**。维护者不读代码，因此验收必须是**可执行脚本**（`embhub-cli` + 测试），而不是「能编译过」。
  2. 据此新增验收标准：`embhub-core` 的依赖里**不允许出现 tauri**（保证可独立测试与 CLI 化）；`embhub-cli` 必须能无头跑通「打开数据源 → 抓 N 秒 → 落盘 → 回放校验」。

### 实测结论（事实，非推断）

| 项 | 结果 |
| --- | --- |
| Rust 工具链 | **cargo 1.99.0 / rustc 1.99.0**（`stable-x86_64-pc-windows-msvc`），装在 `<ws>\.cargo`、`<ws>\.rustup` |
| MSVC 链接 | **通过**（link.exe 找到 kernel32.lib 等）→ 第 1 轮「Windows SDK 可能缺失」的担心是**误判** |
| serialport 4.10.1 | **Windows 上编译通过**（11.18 s）→ 风险 R1 的「编译不过」一半排除；维护支持缺位的风险仍在 |
| 串口枚举 | **通过 serialport-rs 枚举到 10 个端口**（COM3–COM12）。类型**全部是 `unknown`**，即当前无 USB CDC 设备，**VID/PID/序列号路径未被真实验证** |
| WebView2 Runtime | **已就位** 155.0.4283.45（注册表 `pv` 与安装目录双向确认） |
| 磁盘 | C 盘剩余 **113.92 GB** |
| 编译速度 | core+cli 首次全量 11.18 s（依赖已在本地） |
| **未验证** | Tauri app 能否编译、窗口能否打开、插拔后端口列表能否刷新、能否收到真实数据 |

### 环境约束（本轮的真正产出，比代码重要）

1. **沙箱 `workspace-write` 下工作区外写入被拒** → `RUSTUP_HOME` / `CARGO_HOME` **必须落在工作区内**。
2. **`link.exe` 使用系统 TMP（工作区外）会报 `LNK1104`** → `TMP` / `TEMP` 也必须指进工作区（`target\tmp`）。
   - 以上两条落实为 [`scripts/env.ps1`](scripts/env.ps1)。
   - **注意**：在 DSH 会话里 **dot-source 该脚本会被拦**（报 `AuthorizationManager 检查失败`，而执行策略实际是 `Unrestricted`，判断是 sandbox 层拦截脚本执行）。所以我在命令里用**内联**方式设置同样的变量；脚本本身留给用户在自己的终端使用。
3. **crates.io 的 TLS 在沙箱下失败**（`schannel: SEC_E_NO_CREDENTIALS`），提权后正常。
   - ⇒ 依赖下载进工作区内的 `CARGO_HOME` 后，**日常 `cargo build` 不需要提权**（已实测：core+cli 编译 11.18 s 无提权）。
   - ⇒ 但**任何依赖变更（新增 crate）都要重新访问索引，因此需要网络**。
4. **本轮收尾时的阻塞点**：把 `embhub-app` 加入 workspace 成员后，cargo 需要重新解析依赖索引（需网络）。我请求提权，**用户拒绝了这次提权并要求停止绕行**。因此以下命令**尚未执行**：
   - `cargo build -p embhub-app`（Tauri 能否编译 = 未知）
   - `cargo test -p embhub-core -p embhub-cli`（`serde_json` dev-dependency 缺失已修，但**未复跑**，测试是否全绿 = 未知）

### 已确认的合作方式（重要）

**联网类命令由用户在自己的终端执行，agent 不请求提权、不绕行。**
不联网的读写/编译（依赖已就位时）仍由 agent 直接做。

### 下一步（用户在自己的终端执行）

```powershell
# 1) 设置环境（必须点源；pwsh -File 是子进程，变量不会留下）
. .\scripts\env.ps1

# 2) 复跑测试（应全绿）
cargo test -p embhub-core -p embhub-cli

# 3) 编译并启动 spike
cargo run -p embhub-app
```

然后按 [`docs/PLAN.md`](docs/PLAN.md) 第 6 节 spike 判据，**由用户验收两件事**（不需要读代码）：
① 插拔串口设备后点「刷新端口」，列表是否变化；
② 选一个端口点「打开并读取」，是否收到字节数与 HEX。

- **通过** → 写 `SPEC.md`（深化五个接口），进入 Phase 1。
- **失败**（编译不过 / 累计超 1 个工作日仍出不了窗口）→ 按判据切回 Python + pywebview 路线，**架构与五个核心抽象不变**，只损失已投入时间。

### 杂项待处理

- 工作区根目录留着 `rustup-init.exe`（12.7 MB，已 gitignore），可删。
- `scripts/env.ps1` 在 DSH 会话内不可 dot-source，README 已注明替代用法。
- **git 仓库尚未初始化**（`git status` 报 not a repository）。
- 第 1 轮记录中「pip 在沙箱下无法写盘」的结论仍然成立，但 **Rust 路线下已不相关**；若 spike 失败切回 Python 路线，该约束会重新生效。

---

## 2026-10-14 · 第 1 轮：调研 + 选型 + 计划

### 本轮产出

| 文件 | 内容 |
| --- | --- |
| [`docs/research/2026-10-14-embedded-tooling-landscape.md`](docs/research/2026-10-14-embedded-tooling-landscape.md) | GitHub 同类工具与技术库调研报告（397 行） |
| [`docs/research/raw/`](docs/research/raw/) | 原始证据归档：68 个仓库的 API 字段、48 份 manifest 原文、采集脚本 |
| [`docs/PLAN.md`](docs/PLAN.md) | 实施计划 v0.2（架构 / 选型 / 8 个阶段 / 风险 / 验收） |
| `LOG.md` | 本文件 |

### 做了什么

1. **在 GitHub 上做了一手资料调研**（未使用博客/CSDN/知乎等二手来源）：
   数据取自 GitHub REST API 与 Search API、各仓库 manifest 原文、官方 registry（npm/PyPI）、厂商官网。
   采集日 2026-10-14。原始 JSON 与脚本全部归档，结论可逐条回溯。
2. **探测本机工具链**，把「哪些路线在这台机器上零摩擦」变成事实而不是猜测。
3. **合成计划**，并把技术栈从 v0.1 的 Tauri+Rust 改成 v0.2 的 Python 路线。

### 关键结论（有证据支撑）

- **「串口 + SSH + Git + 烧录」四合一没有开源对标品。**
  - Tabby（74,966★ / MIT）用 raw 文件证实内建 `tabby-serial` 与 `tabby-ssh`（`"keywords": ["tabby-builtin-plugin"]`），但 `tabby-git/package.json` 返回 **404**。
  - Serial Studio（7,237★ / GPLv3 或商业双许可）有遥测可视化解码，但无 SSH/Git。
  - ESP-IDF VSCode 扩展（1,472★ / Apache-2.0）有构建烧录 + 串口监视，但 Git 依赖宿主 VSCode。
- **最大空位是「日志 ↔ 固件指纹（commit + dirty + bin hash）↔ Git 提交」的可追溯关联**（调研 C3）。竞品在结构上做不到，因为它们的 Git 是外挂的。
- **次级空位**：多设备会话矩阵视图（C2）、可编程的解析层（C4）、统一的数据源抽象（C6）。
- 参考价值最高的三个：Serial Studio（数据流水线抽象）、Arduino IDE 2.x（外壳 + 独立 CLI 引擎的分层）、probe-rs（烧录 + RTT 日志流的现成能力）。

### 本机环境实测（2026-10-14）

| 项目 | 结果 |
| --- | --- |
| Python | 3.14.4，pip 26.0.1 |
| git | 2.53.0 |
| MSVC | VS2022 已装（`D:\study\vs2022\environment`） |
| cmake | 4.2.3 |
| 串口 | **在线 9 个**（COM3–COM12） |
| Node / npm / Rust / cargo / Go | 均未安装 |
| .NET SDK | 无可用 SDK |
| winget | 可用 |

### 决策记录

**D1（技术栈）：v0.1 推荐 Tauri v2 + Rust → v0.2 改为 Python 路线。**

- 原因：维护者只掌握 Python。Tauri 路线的五个核心抽象（`ByteSource`/`Session`/`Frame`/`LogStore`/`FirmwareFingerprint`）全在 Rust 侧、UI 在 TypeScript 侧，等于要求维护者同时依赖两种自己不掌握的语言。AI 能写出来，但出问题时无法判断是 bug 还是设计错误，也无法接手维护——这是**可维护性**问题，不是能力问题。
- 代价（如实记录）：放弃 Tauri 的体积/内存优势（安装包从约 10 MB 量级变成 150–250 MB 量级），放弃 Rust 生态的原生库集成便利。
- 保留：架构分层与五个核心抽象全部保留，它们与语言无关。

**D1b（外壳）：pywebview(WebView2) + xterm.js/uPlot，而非 PySide6/Qt。**

- 原因：本项目的难题分布是「终端渲染难、业务逻辑不难」。SSH 会话要有颜色、光标控制、全屏程序，自己写 VT100 子集是无底洞；QTermWidget 是 GPL-2.0 且需自行编译；只为终端嵌 QtWebEngine 等于背两套 Web 运行时。xterm.js 是唯一低成本解法。
- 副产品优势：前端是原生 ES module + vendor 静态文件，**全程不需要 Node/npm/构建步骤**，也不会被 electron-rebuild 之类的原生模块 ABI 问题咬到。

**D2（MVP）：只做串口 + 落盘 + 固件指纹。** 其余是增量，不许挤进 MVP。

**D3（项目名）：embhub。**

### 验证与踩坑（重要环境事实）

1. **pip 在当前 DSH sandbox 下无法写盘。**
   - 现象：`pip install/download` 报 `PermissionError: [Errno 13] Permission denied: ...\<wheel>.whl.metadata`。
   - 隔离过程：Python 自身在**同一目录写同名 `.whl.metadata` 文件、建目录、`os.replace`、`shutil.rmtree` 全部成功** → 排除权限/路径问题；`python -m venv` 的 `ensurepip` 子进程同样失败。
   - 定性：**同一条命令提权到 `danger-full-access` 后完全正常**（成功下载 pyserial 并落盘）→ 确认是 sandbox 拦截，不是 pip 或环境损坏。
   - 影响：装依赖必须提权一次，或由用户在自带终端执行。已装好的环境不影响日常开发。
2. **沙箱拒绝了删除我自己创建的 `probe-tmp/` 与 `.tmp/`。** pip 失败时留下的 `pip-build-tracker-*` 目录带 `Everyone:(DENY)(DC)` ACE，`icacls /remove:d` 与 `Remove-Item` 均被拒。**待处理**：目前仍遗留在工作区根目录，需提权删除或手动删除。
3. **接口/工具小坑**：PowerShell 的 here-string 传内联 Python 会被外层引号处理弄坏（`f"..."` 里的引号被吞）；改用独立 `.py` 文件执行即正常。
4. pywebview 依赖链：Windows 上走 EdgeChromium 后端，需 pythonnet + WebView2 Runtime。
   已核实 **pythonnet 3.1.0 起提供 `cp310.cp311.cp312.cp313.cp314` 的 wheel**（3.2.1 覆盖到 cp315）→ Python 3.14 在 wheel 层面可用。
   **但仍未真机跑通开窗**，已列为 Phase 0 的第一项验证（风险 7.2）。

### 明确未核实的事项（不要当事实用）

- pywebview / pythonnet 在 Python 3.14 上的**实际运行**（只核实了 wheel 存在）。
- libgit2 许可证（推断 GPL-2.0 with linking exception）、OpenOCD 许可证、PyInstaller 例外条款原文、QCustomPlot 许可证。
- 闭源工具（STM32CubeIDE / Ozone / MobaXterm / VOFA+）的功能细节与技术栈。
- Tauri / Electron / Wails / Avalonia / egui / iced 的体积与内存对比（官方无量化数据，调研未采用任何第三方跑分）。
- 调研报告中所有「某工具没做到 X」均为**采集范围内的未检到**，不是穷尽性断言。

### 下一步

写 SPEC.md，并等 D1（外壳方案）确认后启动 Phase 0。
