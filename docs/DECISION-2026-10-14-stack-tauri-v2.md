# 决策记录 · 桌面技术栈改定 Tauri v2（D1 第二次修订）

| 项目 | 内容 |
| --- | --- |
| 日期 | 2026-10-14 |
| 来源 | 用户与侧边会话的讨论结论；**用户已明确批准**（「按你的建议来」） |
| 状态 | **待主会话并入** `docs/PLAN.md`（升为 v0.3）与 `LOG.md`（第 2 轮） |
| 影响面 | PLAN.md 第 5 节（技术选型）、第 6 节（Phase 0/4/8）、第 7 节（风险）、第 10 节（待确认项已失效） |

---

## 0. 为什么会有这份文件

`docs/PLAN.md` v0.2 的选型前提是「**项目维护者只掌握 Python**」。该前提来自推断，现已被用户的直接陈述推翻。

用户原话（2026-10-14）：

> 「我想做的是稳定可行的小型项目，不想加太多乱七八糟的东西（有 gui 界面进行交互，实现基本串口通讯、ssh 通讯、git 管理等功能就行，复杂的后面再考虑）。具体代码的编写 agent 来做，我只负责路线选择以及结果验收。」
> 「关于技术栈的选取，我对 python 这条路线不是很自信。」

新前提改写为三条：

1. 维护者的角色是**路线选择 + 行为验收**，不读代码；
2. 目标形态是**小型、稳定、活动部件少**；
3. 维护者对 Python 与 Rust **均无把握** —— 因此「语言熟悉度」不再构成区分项，**真正的区分项变成「哪种失败模式维护者能收场」**。

在这一新前提下重新比较三条路线后，用户选择了 Tauri v2。

---

## 1. 决策

**D1′：宿主语言 = Rust，桌面壳 = Tauri v2，前端 = 极简 TypeScript（不用 React/Vue 等框架）。**

取代 v0.2 的「Python + pywebview(WebView2)」。

> **注意：这是换宿主语言，不是换架构。** 第 3 节的架构、抽象、差异化、阶段骨架全部保留。

---

## 2. 理由（三条，均针对新前提）

1. **「稳定」的真正含义是环境不会烂。**
   Python 路线的稳定性止于运行时：pywebview 依赖 pythonnet + WebView2，paramiko 依赖 cryptography/bcrypt/pynacl 三个原生扩展，PyInstaller 打包对 Qt/WebView 资源敏感——这些都要随 Python 版本演进持续维护。Tauri 的产物是**单个自包含 `.exe`**，环境熵进不了项目。对「非开发者长期自持」这一点，比代码质量更重要。

2. **Node 在 Tauri 里是构建期工具，在 Electron 里是运行时依赖。**
   这是排除 Electron 的核心依据：Electron 的 Node 是产品的一部分，还要长期养 `@electron/rebuild`（node-serialport）与 node-pty（ConPTY）两个必须跟随 Electron 版本重编译的原生模块。用户明确要求「不想加太多乱七八糟的东西」，Electron 是**结构性地**做不到。

3. **编译器是维护者买得到的免费审查员。**
   在「代码由 agent 写、验收靠行为」的前提下，Rust 的类型/借用/`Option` 检查在**编译期**挡掉一整类错误，验收前即已过滤；Python 把错误推到运行时，且崩在 C 扩展（Qt/C 库）里时**没有 Python 堆栈**，维护者无从定位根因。

---

## 3. 不变的东西（架构原样保留）

v0.2 的以下内容**全部保留**，因其与语言无关：

- **产品定位与差异化 C1–C6**，尤其 C3「日志 ↔ 固件指纹（commit + dirty + bin hash）↔ Git 提交」这一最大空位。
- **MVP 判据**：串口能稳定收 + 日志能落盘 + 每条会话带固件指纹。SSH / 波形 / 脚本都是增量，不许挤进 MVP。
- **三条硬约束**：① core 不得依赖 UI 层，必须能独立测试并有 CLI 无头入口；② 数据通道必须批量 + 有背压，落盘路径与显示路径解耦（显示可丢，落盘一个字节不能丢）；③ 固件指纹从 Phase 1 第一天就写进日志头。
- **五个核心抽象**：`ByteSource` / `Session` / `Frame` / `LogStore` / `FirmwareFingerprint`。
- **阶段划分骨架**（Phase 0–8）、绿色便携、Windows 优先且不堵死 Linux/macOS。

### 3.1 依赖映射（Python → Rust）

| 能力 | v0.2（Python） | v0.3（Rust） | 备注 |
| --- | --- | --- | --- |
| 串口 | pyserial | **serialport-rs** | 备选 `tokio-serial`；见风险 R1 |
| SSH / SFTP | paramiko | **russh** | 纯 Rust，无 libssh2 系统依赖 |
| Git | 调系统 `git` CLI | **不变**（`std::process::Command`） | 刻意不引 libgit2/gitoxide |
| 存储 | stdlib `sqlite3` | **rusqlite**（`bundled` feature） | 静态链接 SQLite，免系统依赖 |
| HTTP / WS 网关 | aiohttp | **待 spike 决定**：axum 或复用 Tauri IPC | 倾向先用 Tauri IPC，少一个依赖 |
| 桌面外壳 | pywebview | **Tauri v2** | Win11 26100 内置 WebView2 |
| 前端终端 | xterm.js | 不变 | |
| 前端波形 | uPlot | 不变 | |
| 前端构建 | 无（原生 ES module） | **Vite + TypeScript** | 取舍见第 8 节 |
| 测试 | pytest | `cargo test` | |
| 打包 | PyInstaller | `tauri build`（.msi / .exe） | |

---

## 4. 三条减法（建议写进验收标准，防止 agent 堆复杂度）

1. **前端不用 React/Vue**，原生 TypeScript + 一层有类型的 Tauri command。页面只有三个：串口 / SSH / Git。
2. **Git 不引 libgit2 / gitoxide，直接调系统 `git` CLI**。行为与用户命令行完全一致，出问题可用命令行复现。这是**降复杂度**，不是偷懒。
3. **SSH 第一期不做终端仿真**：命令执行 + 输出显示 + SFTP 上传下载。交互式终端（xterm.js）留到 Phase 4。

---

## 5. Spike（第一步不是写功能，是证明最危险的一环）

**范围**：最小 Tauri 窗口 → 枚举 COM 口 → 选择一个 → 打开 → 收到的数据按 HEX 滚动显示。

**通过判据**（全部满足才算通过）：

- `cargo tauri dev` 在本机出窗口（本机 Win11 build 26100，WebView2 内置）；
- 插拔 USB 串口后端口列表能刷新；
- 115200 下能收到真实数据（本机实测在线 9 个口：COM3–COM12）。

**失败 / 切换判据**（满足任一，即切回 Python + pywebview 路线；架构与五个抽象不受影响，只损失已投入时间）：

- rustup / 工具链装不上，或 MSVC 链接失败；
- `serialport-rs` 或 Tauri 关键 crate 编译不过，且 4 小时内无解；
- 累计超过 **1 个工作日**仍出不了窗口。

**用户需要亲自验收的只有两件事**（不需要读懂代码）：
① 插拔串口设备后列表是否刷新；② 打开一个口后是否收到真实数据。

---

## 6. 时间与代价（**估计值，非实测**）

| | Python（v0.2 原方案） | **Tauri（本决策）** |
| --- | --- | --- |
| 装工具链 | 0（但 pip 需提权一次） | 0.5–1 小时（rustup + Node） |
| 首次跑出窗口 | 半天 | **1 天**（首次编译 5–15 分钟） |
| 之后每改一次 | 秒级 | 几十秒 |
| 第一期可用版本 | ~3 天 | **~5–7 天** |
| 安装包体积 | 150–250 MB（PyInstaller） | **~10 MB 量级** |

多花的 2–4 天，买的是「以后不用再碰环境」与「验收前编译器替你挡错」。

**本机利好（已实测）**：VS2022 + MSVC 已装（`D:\study\vs2022\environment`）——这是 Rust on Windows 最大的一个坑，已经过了；winget 可用；WebView2 在 Win11 26100 内置。

---

## 7. 风险变更

| # | 风险 | 级别 | 对策 |
| --- | --- | --- | --- |
| R1 | **`serialport-rs` 的 Windows 维护者缺位**（父会话调研已核实：README 公开招维护者） | 中 | spike 阶段就暴露；备选 `tokio-serial`，或 `windows-rs` 直接调 Win32 API |
| R2 | ~~v0.2 风险 7.2：pywebview + pythonnet 在 Python 3.14 上未真机跑通~~ | — | **随本决策消除** |
| R3 | rustup 首次安装为 GB 级下载；首次编译 5–15 分钟 | 低 | 一次性成本 |
| R4 | agent 写 Rust 迭代慢 2–3 倍（借用检查反复 + 每次 cargo build） | 中 | 接受；用「编译通过」换取验收质量 |
| R5 | pip 在 sandbox 下无法写盘（已定性） | — | 对 Rust 路线不再相关；**若 spike 失败切回 Python 则重新生效** |
| R6 | 被动监听「已被占用的串口」在 Windows 可能需内核过滤驱动 | 中 | Phase 2 先 spike，不成则砍（原样保留） |
| R7 | 高吞吐下 UI 卡死 / 丢数据 | 高 | 原 4.3 的批量 + 背压；落盘与显示解耦（原样保留） |
| R8 | 范围蔓延 | 高 | 第 4 节三条减法 + MVP 判据 |
| R9 | 许可证 | 低 | Tauri/Rust 生态以 MIT/Apache-2.0 为主；**分发前逐项核实**，不抄 GPL 代码（Serial Studio 为 GPLv3/商业双许可） |

---

## 8. 未核实事项（**不要当事实用**）

- **Tauri v2 + Rust 在本机实际能否跑通，目前没有任何证据** —— 这正是第 5 节 spike 要验证的第一件事。
- 第 6 节所有时间数字均为**估计值**，非实测。
- 「Tauri 前端可做到零 Node」这一说法：Tauri CLI 可以直接 serve 静态目录，理论上可行；但本决策**建议仍用 Vite + TypeScript**，以换取类型检查替 agent 挡错，代价是 Node 成为**构建期**（非运行期）依赖。这是一个取舍，不是定论。
- 原调研报告中「Tauri / Electron / Wails 体积与内存对比」未采用任何量化数据，本决策第 6 节的体积数字为量级估计。
- 所有 crate 的许可证与 Python 3.14 兼容性细节，未逐项核实。

---

## 9. 给主会话的执行清单

1. **升级 `docs/PLAN.md` 到 v0.3**：
   - 第 1 节：技术栈一行改为 Tauri v2 + Rust core + 极简 TS 前端；
   - 第 5 节：整体替换为 Rust 依赖映射（本文第 3.1 节）；
   - 第 6 节：Phase 0 改为「spike 验证 + 骨架 + `cargo test` 基线」，Phase 4（SSH）与 Phase 8（打包）按 Rust 改写；
   - 第 7 节：按本文第 7 节替换；
   - **第 10 节作废**：原 D1 问的是「pywebview ① 还是 PySide6 ②」，两者都属 Python 路线，问题已随本决策失效，无需再问用户。
2. **`LOG.md` 追加第 2 轮记录**：D1 第二次修订（Python → Tauri v2）、原因（前提被用户陈述推翻）、新增/消除的风险。
3. **保留** v0.2 中与语言无关的部分（本文第 3 节），不要跟着重写——那是已确认且经调研支撑的内容。
4. **待处理（父会话遗留）**：工作区根目录 `.tmp/` 与 `probe-tmp/` 残留，其 `pip-build-tracker-*` 子目录带 `Everyone:(DENY)(DC)` ACE，`Remove-Item` 与 `icacls /remove:d` 均被拒，需提权删除或由用户手动删除。
5. **第一步实际动作**：执行第 5 节 spike，按通过/失败判据验收。**通过后再写 `SPEC.md`**；失败则按切换判据回到 Python 路线，架构不变。

---

## 10. 用户批准记录

> 用户（2026-10-14，侧边会话）：「按你的建议来，可以这个建议交给主会话了」

批准对象：第 1 节的 D1′（Tauri v2 + Rust），连同第 4 节三条减法与第 5 节 spike 判据。
