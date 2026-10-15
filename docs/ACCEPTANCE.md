# embhub 验收清单

> **这份文档给谁用**：项目维护者（你）在每轮施工后做独立验收。
> **怎么用**：第 2 节的 15 条可以一条命令跑完（`.\scripts\accept.ps1`），也可以按下面的原始命令逐条手敲——**每条都给了命令、期望输出、通过标准**，所以你不必信任脚本本身。
> **这次验收覆盖**：Phase 0 + 增量 A / B1 / B2 / C1。**不包含增量 C2**（GUI 尚未接入新管线，见第 4 节）。

---

## 0. 两条会误导你的操作习惯（先看这个）

1. **不要用 `| Select-Object -First N` 截断被验收命令的输出。** PowerShell 提前掐断管道会杀掉上游进程，退出码变成 `-1`，看起来像失败但其实是命令被你自己打断。这个坑我在施工中踩了三次。
2. **"收到 0 字节"不等于失败**，只代表那段时间里没有对端在发数据。要判断接收通路，必须有一个**正在发送**的对端（板子开机打印、或另一个串口程序在发）。**单纯把 TX-RX 短接做回环测不出来**——本工具只读不发。

---

## 1. 环境准备

```powershell
cd "C:\Users\24928\Documents\deepseek-harness\Integrated Tool"
```

不需要设任何环境变量：`scripts\cargo.cmd` 会自动把 `RUSTUP_HOME` / `CARGO_HOME` / `TMP` 指向工作区内（原因见 `docs/PLAN.md` 5.4）。

一条命令跑完全部自动项：

```powershell
.\scripts\accept.ps1
# 若被执行策略拦：Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass  然后重跑
```

期望结尾：`=== summary: 15 passed, 0 failed ===`，退出码 0。

---

## 2. 自动项（15 条）

### B. 构建、测试、静态检查

| ID | 原始命令 | 通过标准 | 失败长什么样 |
| --- | --- | --- | --- |
| **B1** | `.\scripts\cargo.cmd test -p embhub-core` | **50 passed; 0 failed**，退出码 0 | 数字小于 50 ⇒ 有测试被删/改名，**要追问** |
| **B2** | `.\scripts\cargo.cmd test -p embhub-cli` | **17 passed; 0 failed**，退出码 0 | 数字小于 17 ⇒ 同上 |
| **B3** | `.\scripts\cargo.cmd clippy --all-targets -- -D warnings` | 退出码 0，无 warning | 任何 warning 都会让它非零退出 |
| **B4** | `.\scripts\cargo.cmd fmt --all --check` | 退出码 0，无 diff | 会打印具体 diff |
| **B5** | `.\scripts\cargo.cmd build -p embhub-app` | 退出码 0 | GUI 被弄坏了 |

### A. 零丢失闭环（不需要硬件）

| ID | 原始命令 | 通过标准 |
| --- | --- | --- |
| **A1** | `.\scripts\cargo.cmd run --quiet -p embhub-cli -- selftest --bytes 67108864` | 退出码 0，输出含 `magic=EMBHLOG`；程序内部断言**注入 == 落盘 == 回放读回**且逐字节一致，不一致就非零退出 |
| **A2** | `.\scripts\cargo.cmd run --quiet -p embhub-cli -- selftest --bytes 67108864 --repo . --bin docs\SPEC.md` | 退出码 0，会话头显示 `版本=2`，并打印 `✓ 指纹逐字段比对：会话头读回 == 采集值（8/8 字段）` |

> A2 是"日志 ↔ 固件指纹"这条差异化能力的端到端证据：采集指纹 → 写进会话头 → **从磁盘读回** → 逐字段比对。

### C. 流式管线与背压（SPEC 4.3）

| ID | 原始命令 | 通过标准 |
| --- | --- | --- |
| **C1** | `.\scripts\cargo.cmd run --quiet -p embhub-cli -- stream --bytes 67108864 --consume fast` | 退出码 0；`batches_dropped : 0` |
| **C2** | `.\scripts\cargo.cmd run --quiet -p embhub-cli -- stream --bytes 67108864 --consume stalled` | 退出码 0；`batches_dropped` **> 0**（显示确实在丢），**且**结尾打印 `✓ 落盘零丢失` |
| **C3** | `.\scripts\cargo.cmd run --quiet -p embhub-cli -- stream --bytes 67108864 --consume stalled --display-capacity 0` | 退出码 0；`batches_emitted : 0`、`display_bytes_dropped : 67108864`（**显示 100% 丢弃**），**而 `logged_bytes` 仍等于 67108864** |

> **C3 是这一轮最关键的证据**：显示路径一个字节都没送达，落盘路径零丢失。这直接证明"显示"和"落盘"是两条独立路径，而不是共用一条会互相拖累的队列。
> 附加自洽检查：C2 里 `display_bytes_emitted + display_bytes_dropped` 必须等于 67108864。

### D. 固件指纹（与外部实现交叉校验）

| ID | 原始命令 | 通过标准 |
| --- | --- | --- |
| **D1** | `.\scripts\cargo.cmd run --quiet -p embhub-cli -- fingerprint --repo . --bin docs\SPEC.md`，再用 `Get-FileHash docs\SPEC.md -Algorithm SHA256` | 两个 SHA-256 **逐字符相同**（脚本会自动比对） |
| **D2** | 同上一条的输出 | `commit` 是完整 **40 位十六进制**（与 `git rev-parse HEAD` 一致） |
| **D3** | `.\scripts\cargo.cmd run --quiet -p embhub-cli -- list` | 退出码 0，列出串口（本机 10 个） |

> D1 的重要性：SHA-256 是自己实现的（沙箱无网络、不许拉 crate），自实现最容易"错一点点但仍输出 64 位 hex"。**拿 Windows 的实现当参照**才能证明它是对的。

### E. 范围与依赖漂移

| ID | 原始命令 | 通过标准 |
| --- | --- | --- |
| **E1** | `git diff --quiet -- Cargo.lock` | 退出码 0（**Cargo.lock 完全未变** ⇒ 零新增 crate） |
| **E2** | 检查 `crates\embhub-core\Cargo.toml` 是否含 `tauri` | **不含**（分层铁律：core 不得依赖 UI 层） |

---

## 3. 只有你能验的 3 条（需要人眼 / 需要硬件）

| ID | 操作 | 通过标准 |
| --- | --- | --- |
| **G1** | `.\scripts\cargo.cmd run -p embhub-app` | 弹出一个窗口，日志区显示「发现 N 个串口」 |
| **G2** | 插上 / 拔掉一个 USB 串口设备，点「刷新端口」 | 端口数量跟着变（插上 10 → 11，拔掉回到 10） |
| **G3** | 选一个端口、115200，**在板子正在发送时**点「打开并读取」 | 显示收到**大于 0** 的字节数与 HEX |

> G3 的前提是**对端真的在发**。你之前那次是板子供电不足导致起不来，看起来像"收不到数据"——所以先确认板子已上电（LED 亮）。

---

## 4. 已知限制（避免误判为失败）

1. **GUI 还没接入新管线**（增量 C2 未开始）。窗口里那个「打开并读取」仍是早期 spike 的阻塞式读法：**看不到** C1 的实时统计（源字节数 / 速率 / 显示丢弃计数），也**没有**"开始会话 / 停止会话"按钮。
2. **本机 10 个 COM 口大多是假的**：注册表显示 `COM3–COM12` 全是 `\Device\BthModem*`（蓝牙调制解调器），真实串口设备只有你插上的那个。所以 `list_ports()` 把它们报成 `type=unknown` 是**正确的**，不是 bug。
3. **`chip` / `probe_id` 显示「取不到」是设计如此**：它们依赖调试器（probe-rs），属 Phase 7；当前会明确打印原因而不是留空。
4. **`target\tmp` 下有一些 0 字节残留目录**（测试与独立校验留下的），已被 `.gitignore` 覆盖，`cargo clean` 可清。
5. **还没有的能力**（都在计划内、尚未开工）：同端口互斥、`.embproj` 工程文件、文本/HEX/时间戳正式显示面板、SSH / Git / 波形。

---

## 5. 我没有验证或做不到的部分（诚实清单）

- **我无法验证 GUI**：DSH 沙箱内 WebView2 起不来（`HRESULT(0x8000FFFF)`），所以 G1–G3 只能由你在沙箱外的终端确认。
- **未独立复现**：某条失败路径（把 `TMP` 指向普通文件使会话目录不可建 → 期望 exit 1）。我复现的是"坏参数 → exit 1"。
- **未采集真实 `chip` / `probe_id`**（Phase 7）。
- **未做 SSH / SFTP / Git / 波形**（Phase 4/5/3 未开工）。
- 所有增量都在**同一台机器**上验证，跨平台（Linux/macOS）未验证。

---

## 6. 什么情况下应该推翻我的结论

我把"能证伪我"的点列出来，方便你攻击：

| 如果你看到… | 那么被推翻的是 |
| --- | --- |
| C3 里 `logged_bytes` **小于** 67108864 | 「显示可丢、落盘不可丢」不成立 ⇒ **增量 C1 的结论作废** |
| C2 里 `display_bytes_emitted + display_bytes_dropped ≠ 67108864` | 背压记账不自洽 ⇒ C1 存疑 |
| D1 两个哈希**不一致** | 自实现 SHA-256 有问题 ⇒ **增量 B1 的结论作废** |
| B1/B2 的 passed 数**少于** 50 / 17 | 有测试被删除或改名 ⇒ 要求我给出差集说明 |
| E1 非空 | 引入了新 crate（沙箱下本不该发生） |
| E2 里出现 tauri | 分层铁律被破坏 |
| A2 会话头版本**不是** 2 | 日志格式升级没生效 ⇒ B2 存疑 |

---

## 7. 每轮施工的标准动作（约定）

1. captain 定契约（objective / acceptance / verify / inScope / outOfScope / nonGoals）；
2. 成员实现，交付时必须给出**实际命令与真实输出**，"已完成"三个字不算交付；
3. **captain 亲自复跑全部 verify**，并额外做成员未声称的独立验证（外部实现交叉校验、字节级解析、对抗性参数）；
4. 成员主动上报自己的证据边界（哪些分支只有单测覆盖、哪些没端到端复现）；
5. captain 建评审任务并**由非实现者给出裁决**（评审者不得是实现者）；
6. 更新 `LOG.md`（本轮记录与决策原因）、`README.md`（现状与能力）；设计变更同步 `docs/SPEC.md`。
