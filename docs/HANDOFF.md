# HANDOFF —— 会话交接

> **读者是下一个会话的模型，不是用户。**
> 目的：让你在最短时间内接手，并清楚**边界在哪**。本文只给**索引 + 约束**，不重复细节——细节按 §0 去读原文。
> 原则：本文没写的事实，一律以 `README.md` / `docs/` 下的文档与**实际代码**为准；**不要凭本文的概括去改代码**。

---

## §0 先读（按顺序；README 与 SPEC 必须通读）

| 顺序 | 文档 | 读什么 |
| --- | --- | --- |
| 1 | [`README.md`](../README.md) | **通读**。现状、怎么跑、主要能力、三条硬约束 |
| 2 | [`docs/SPEC.md`](SPEC.md) | **通读**。设计原则、模块契约 3.1–3.6、边界、验收标准 |
| 3 | [`docs/PLAN.md`](PLAN.md) §Phase 5（**第 214–217 行**） | **下一个功能的需求原文 + 它的验收标准** |
| 4 | [`docs/DECISION-2026-10-14-stack-tauri-v2.md`](DECISION-2026-10-14-stack-tauri-v2.md) | 为什么是 Rust core + Tauri v2。**不要重新论证选型** |
| 5 | [`docs/ACCEPTANCE.md`](ACCEPTANCE.md) 第 3 节 | **18 条只有用户能验的项（G1–G18）**。你**无法**在沙箱内验它们（沙箱开不了窗，见 §6） |
| 6 | [`docs/test/PROTOCOL.md`](test/PROTOCOL.md) + [`docs/test/README.md`](test/README.md) | 只在涉及串口收发/构造测试数据时读（用户板子 STM32G474 的帧协议） |
| 7 | [`LOG.md`](../LOG.md) | **不要通读**。按 §7 的检索入口查 |

---

## §1 一句话现状

串口助手**基本功能已完成并经用户真机实测通过**；日志格式 v2 已带固件指纹；GUI 已能采集指纹并列举历史会话；布局已区域化（H1a/H1b）并有可独立跑的求解器（**35 项**）与页内 harness（**67 个状态**）；**H2 功能区侧边栏已做并可切换（串口助手 / SSH / Git，SSH 与 Git 仅占位）**，切换**不中断会话、不丢显示内容**。
**当前是用户指定的暂停点（H2 之后）**——不要自动往下推。
**下一个功能：H3 = Git 状态条 + 从日志定位到 commit**（= PLAN 的 Phase 5「Git 溯源」，本项目唯一区别于竞品的差异化能力）。**只显示/复制 commit，不做 checkout**（用户在 G2 阶段明确否掉了 checkout，且已有闸门 `app_never_mutates_the_repository` 钉死）。**任务拆分、每项要点与完成判据见 §2。**

---

## §2 下一个任务：H3 —— Git 状态条 + 从日志定位到 commit

### 为什么是它（三条硬理由）

1. `README` 的「规划中」表里，**Phase 5 ★ Git 溯源**是唯一标星、且**不需要新依赖**的未开工阶段。Phase 3（uPlot）/ Phase 4（russh）都要下载 crate，而沙箱无网络（§6）。
2. **H2 已把框架建好**：`L.FUNCTION_ORDER = [serial, ssh, git]`，侧边栏 + 每个功能区一棵视图树；`ssh`/`git` 目前是**占位区**（只写「尚未实现 + 计划范围」，且 Rust 闸门禁止在占位体里创建控件）。⇒ **填 git 功能区是 H2 的自然延续**，不是新开战场。
3. 这是调研里**唯一没人做过**的能力（日志 ↔ 固件指纹 ↔ Git 提交）。现状：`crates/embhub-app/src/main.rs` 里对 git 的引用**只有闸门与注释**，`embhub-core` **没有 `git` 模块**。

### 任务拆分（照此建 AgentTeams 任务）

| ID | kind | 归属 | 依赖 | 交付 |
| --- | --- | --- | --- | --- |
| **H3a** | implementation | member | — | `crates/embhub-core/src/git.rs`：仓库状态 / 提交列表 / 单提交 diff；**只读**；超时 + 错误分类；绝不 panic；单测 |
| **H3b** | implementation | member | H3a | `git` 功能区去占位：状态条（branch / HEAD / dirty）+ 会话→commit→**查看 diff** + **按 commit / 固件版本过滤日志**；遵守区域架构与闸门 |
| **H3c** | review | captain | H3a, H3b | 独立评审（**评审者不得是实现者**）+ 按 PLAN 第 217 行的闭环验收（**需用户先同意实验仓库**，见 §8.4）|

**H3a 要点**

- 一律走**系统 `git` CLI 子进程**（SPEC 已决定，**不引 libgit2/gitoxide**）。
- 至少给出：`repo_state(repo)` → branch / HEAD（**完整 40 hex**）/ dirty / 是否 git 仓库；`log(repo, limit)` → 提交列表；`commit_diff(repo, hash)` → 供"查看该提交"。
- **只读**：不得调用任何会改仓库状态的子命令。此约束**已有闸门钉死**：`crates/embhub-app/src/main.rs:2635` 的 `app_never_mutates_the_repository`（注释在 2630 行："绝不改变用户仓库状态：生产代码里不许出现会动工作区的 git 子命令"）——**只覆盖 app 侧，你的新 core 模块要自己守住，并给它补一条同类的 core 侧闸门。**
- **超时 + 错误分类**（git 不存在 / 不是仓库 / 命令失败 / 超时）+ **绝不 panic**。
- 参照物：`fingerprint.rs` 第 218、264 行已在调 git 子进程（218 行用变量 `git`、264 行用字面量 `"git"`）——**照它的路子写，但把超时与错误分类补齐**。
- 测试：临时仓库（既有指纹测试已证明**沙箱内 git CLI 可用**）、非仓库、坏 hash、无提交的空仓库、git 不在 PATH、超时。

**H3b 要点**

- 复用 G2 已有的 repoPath 输入与持久化。
- **必须遵守 H1a/H1b/H2 的区域架构**：区域表 + **唯一一份求解器** + 闸门（含"声明行 vs 首次执行行"顺序规则）；**不得在占位区/新区域里建假控件**；新区域要进功能树与 harness 状态矩阵。
- 沿用 H2 的结论：**切换功能区不得中断会话、不得丢数据**。
- **不要碰 `?selftest=1` / F9 的挂载方式**（第 24 轮刚把它们从 init 尾部解放出来，见 §6）。

**H3 完成的判据**（PLAN 原文含已被用户否掉的 checkout，故给替代判据）

- PLAN 第 216 行：日志列表**按固件版本/commit 过滤**；从一条日志/会话**查看**对应 commit 的 diff（**不是**切分支）。
- PLAN 第 217 行：构造「提交 A 跑出异常日志 → 切到提交 B 不再复现」的闭环，**全程不离开本工具**——其中"切到提交 B"由**用户手工完成**（我们只做只读呈现与过滤），这一点必须写进 H3c 的验收说明。

### 不做（越界会被驳回）

- ❌ **任何破坏性 git 操作**：`checkout` / `reset` / `clean` / `stash` / `commit` / `push` / `merge`。PLAN 里的"跳到对应 commit 的 diff"是**查看**，不是切分支。**任何会改用户仓库状态的动作都要逐次经用户确认。**
- ❌ **不新增任何第三方 crate**（原因见 §6 第一条；沙箱无网络）。这直接排除 Phase 3（uPlot）、Phase 4（russh）——它们要用户配合联网，**另开增量**。
- ❌ **不让 `embhub-core` 依赖 tauri**（有测试 `core_has_no_tauri_dependency` 钉死）。
- ❌ **不改日志格式 v2**；确需改动，先改 SPEC 并在 LOG 记录。
- ❌ **不重做** §3 列出的已完成增量。
- ❌ 不引入 npm / 打包器 / 前端框架（前端是原生 HTML/JS）。

### 验收流程（必须遵守，不是建议）

走 AgentTeams 质量流：**实现 → 独立评审（评审者不得是实现者）→ 交付**。captain 必须**亲自复跑全部 verify**，并做实现者**未声称**的独立验证（外部实现交叉校验、字节级解析、对抗性参数）。
**GUI 与像素级结论一律写成用户验收项，不得声称已验**（沙箱内 WebView2 与 headless Edge 都起不来，见 §6）。

---

## §3 已完成（不要重做）

- **已验收增量**（以 README 顶部状态块 + 「主要能力」各小节为准）：Phase 0；A、B1、B2、C1、C2a、C2b、F1、F2、G1、G2、H1a、H1b、**H2**。
- **测试基线**（本文 2026-10-16 实跑核对，与 README 一致）：core 89 + cli 34 + app 31 = **154 passed**；另有**布局求解器 35 项**（`通过 35 项，失败 0 项`；见 §6 的 node 路径陷阱）。
- **一键验收**：`scripts/accept.ps1`（2026-10-16 实跑 **20 passed / 0 failed**），以及 [`docs/ACCEPTANCE.md`](ACCEPTANCE.md) 第 3 节的 **18 条**人工项（G1–G18）。
  > **口径**：脚本里 `B7` 恰好出现两次是**正常的**——那是 `if (-not $nodeExe) { … } else { … }` 的一对互斥分支，每次只执行一个，所以实跑仍打印 20 项。**不要当成"标签重复"去"修"。**
- **跳过硬名单**：Rust 闸门里有一段**成对断言**（"必须存在" + "不许出现在 `skipped.push` 里"），位置 `crates/embhub-app/src/main.rs:1587–1606`，**实测为 7 条**：`region-clipped`、`pair-isolation`、`guard-failed`、`reopen-failed`、`buffer-lost`、`session-dropped`、`boot-degraded`；`index.html` 里 `skipped.push(` 共 **3 处**。
  > ⚠️ 本文曾写作"13 条"/"14 条"，**与代码不符**（"14"是把同一批 7 条数了两遍）。**改这一段之前请自己到那段代码里数一遍**，不要采信文档里的数字——**把闸门覆盖面写大，本身就是一种"看起来更严"的假象。**
- **布局闸门测试**（app crate 里的 `layout_invariants_prevent_the_receive_pane_from_squeezing_others`）：本文 2026-10-16 复核实测——**函数体 677 行（非空 658 行），含 81 处 `assert!`**（无 `assert_eq!`）。**每加一条布局/交互规则都必须同步扩它**，且新规则要同时进上面那份跳过硬名单（成对断言）。
  > ⚠️ 本文原文写"**106 条断言 / 659 行**"：659 行与实测非空行数吻合，但 **106 条我未能复现**。**引用这个数字前自己数一遍**（`main.rs` 1211–1888 行）。
- **启动路径有两条额外保障（第 24 轮加，别删）**：① 所有启动步骤走 `bootStep`，失败在页顶插红条 `#bootError`（含错误原文）；② 启动后自检 `checkTracksWritten()`——根节点 ≥2 个可见孩子但 `#app` 仍是单轨 ⇒ 报错。**自检入口有三条且都不依赖 init 尾部：F9 / `?selftest=1`（挂 `load`）/ 红条里的按钮。**
- **core 现有模块**：`crc` `fingerprint` `pipeline` `session` `sha256` `source` `store`。**没有 `git` 模块**。
- **已存在、可直接复用**：`store::list_sessions(root)`（`store.rs:616`）、`fingerprint::collect(repo, bin)`、`Session` / `SessionSnapshot` / `LogStore`。
- **布局相关**：纯函数求解器 `crates/embhub-app/ui/layout.js` + 其测试 `layout.test.js`；页内 harness（`?selftest=1` 或 app 内「跑布局自检」按钮）。**改布局前先读 `index.html` 的 `<style>` 顶部根因注释**（那里有禁令）。

---

## §4 已知缺口与**文档漂移**

- **用户验收状态以 [`docs/ACCEPTANCE.md`](ACCEPTANCE.md) 第 3 节为准**（**18 条，G1–G18**）。**不要替用户勾选**，也不要把"我验过结构"写成"这项通过了"。
- **像素级结论普遍未由人验证**：H1a/H1b 布局、以及任何视觉结论，都需用户在真实 WebView2 里确认。
- **GUI 里 `chip` / `probe_id` 显示"取不到"是设计如此**（Phase 7 未做），不是 bug。
- **未开工**：H3（Git 状态条 + 定位 commit，= Phase 5）、Phase 3 波形、Phase 4 SSH/SFTP（SSH/Git 目前只有占位视图）、Phase 6 解析扩展、Phase 7 烧录、Phase 8 打包。
- **README 的四处漂移已于 2026-10-16 全部修正**（自动项 19→20、G1–G10→G1–G17、`lib.rs` 的"spike 版"描述、app `read_probe` 命令、文档表里的"15+3"）。若你日后又发现漂移，**顺手修，别为此单开大任务**。
- **但又新出现一处漂移（本文 2026-10-16 晚复核）**：README 第 14 行与文档表仍写 **17 项（G1–G17）**，而 `ACCEPTANCE.md` 第 3 节标题与表格实为 **18 条（G1–G18）**（第 24 轮又添了一条）。⇒ **以 ACCEPTANCE 文档为准**，顺手把 README 改成 18。

---

## §5 不可违反的关键决策

- **技术栈（D1′）**：Rust core + Tauri v2 外壳 + 原生 HTML/JS 前端。无 React/Vue、无 npm、无打包器。理由见 DECISION 文档。
- **三条硬约束**（README 与 SPEC 都有，别违反）：① `embhub-core` 不得依赖 tauri；② 数据通道必须批量 + 背压——**显示可丢，落盘一个字节都不能丢**；③ **固件指纹从第一天写进日志头**（事后补不回来）。
- **Git 走系统 `git` CLI**，不引 libgit2/gitoxide。
- **会话落盘格式 = v2**（头：magic / version / start_wall_ms / 指纹段 / 配置；记录：`t_mono u64 | len u32 | crc u32` + payload）。

---

## §6 踩过的坑（会浪费你半天的那种）

**环境 / 沙箱**

- **沙箱无网络**：`cargo` 拉不到 crates.io（`schannel: SEC_E_NO_CREDENTIALS`）。现有依赖是当初一次性放行拉下来的。⇒ **不要新增 crate**；确需新增，先让用户配合（第 2–3 轮）。
- **`node` 不在 PATH 上**。README 要求用 `node crates\embhub-app\ui\layout.test.js` 跑布局求解器——**必须用 DSH 捆绑的 Node**，路径由 `load_workspace_dependencies` 返回（本机为 `C:\Users\24928\.dsh\dsh-runtimes\dsh-primary-runtime\dependencies\node\bin\node.exe`）。
- **必须用 `.\scripts\cargo.cmd`，不要裸跑 `cargo`**（它把 `RUSTUP_HOME`/`CARGO_HOME`/`TMP`/`TEMP` 指到工作区内；否则 link.exe 报 `LNK1104`）。
- **`scripts\env.ps1` 必须点源**（`. .\scripts\env.ps1`），不要 `pwsh -File`。
- **`.ps1` 必须纯 ASCII**：会话里的 PowerShell 是 **5.1**，无 BOM 的 UTF-8 会被按 ANSI 解码。`scripts\accept.ps1` 现为非 ASCII 字节数 0，**保持**。
- **沙箱内 GUI 验不了**：WebView2 建窗失败 `HRESULT(0x8000FFFF)`；**headless Edge 也跑不了**——Chromium 的 mojo 需要命名管道，被拒 `拒绝访问 (0x5)`（本轮实测原文）。⇒ 一切像素级/交互结论交给用户。
- **不要用 `| Select-Object -First N` 截断被验收命令的输出**：PowerShell 掐断管道会杀上游进程，退出码变 `-1`，看起来像失败。captain 在这上面栽过 3 次。

**硬件 / 串口**

- **向未知串口设备写字节有真实副作用**（可能复位设备、进 bootloader）：本机 COM3–COM12 多为蓝牙虚拟口。**没有用户授权不写**（第 10 轮）。
- **"收到 0 字节"不等于失败**。用户那次"收不到数据"的真因是**板子供电不足**（USB-TTL 只给 TX/RX/GND）（第 12 轮）。

**布局**

- **不要用固定像素高度"修"布局**。t17 的根因是 flex 约束：固定区要 `flex: 0 0 auto`、**唯一**伸缩区带 `min-height: 0`、滚动内容**不得有 px 下限**。`index.html` 的 `<style>` 顶部有根因注释与禁令，**先读再改**（第 15–16 轮）。
- **`#app` 的静态兜底必须是单格 `100%`，且它自己就是根节点**（`nodes.get(TREE.id) === appEl`）。曾经的两列兜底 `300px 1fr` 与"#app 只有一个孩子"的结构矛盾，把右列**整块裁掉**（第 22 轮）。真实轨道**每次由 `applyLayout()` 写入**。
- **`[hidden]` 规则的特异性必须高于 `.region`/`.split`/`.splitter`**（现为 `.region[hidden], .split[hidden], .splitter[hidden] { display: none }`）。特异性相同时作者样式优先 ⇒ 被隐藏的区域**仍占网格轨道位** ⇒ 错位塌缩（第 21 轮）。闸门 `css_specificity()` 会**真算特异性**（且先剥离注释——注释里的 `[hidden]` 曾骗过 captain 的朴素正则）。
- **布局求解器只能有一份**（`ui/layout.js`，纯函数、不碰 DOM）；区域表也只能一份；页面必须**引用**而非复制（闸门明文禁止页面内出现 `minW:`/`minH:`/`function solve(`）。
- **别改测试来把红变绿**：动测试时逐条问「旧断言删了吗？新断言是不是更弱/自指？」。跳过硬名单**是成对断言**（必须存在 + 不许出现在 `skipped.push` 里），**条数以 §3 的实测为准**（代码在 `crates/embhub-app/src/main.rs:1587`，自己去数）。历史上 captain 每轮都复核这一点。
- **往 Markdown 表格追加行时**：`old_string` 必须包含"新行要插入的位置"，否则会**覆盖掉已有行**（captain 在 `ACCEPTANCE.md` 上因此把 G14、G16 各覆盖过一次——已补回）。

**"验证通过"这件事本身**

- **"PASS" 不等于"没问题"**：页内 harness 曾 44 状态全绿，而真实 app 里右列被整块裁掉。**验证工具的盲区就是结论的盲区**——拿到 PASS 要反问一句"它看不见什么"。
- **"存在性检查"挡不住"结构错误"**：H2 的闸门全是 `html.contains("…")` 式的文本存在性检查，**看不出 TDZ**，于是**能全绿却启动即崩**（第 24 轮）。已补上两条腿：**顺序规则（静态，29 个 init 路径函数的"声明行 < 首次执行行"）+ 运行时自检（`checkTracksWritten` / harness 的 `boot-degraded`）**。
- **自检入口不能挂在"可能出错的那段初始化"之后**：`?selftest=1` 曾挂在 init 尾部 ⇒ 一旦 init 中断，**自检恰在最需要时跑不了**。现在有三条不依赖 init 的入口：**F9**、`?selftest=1`（挂 `load`）、启动失败红条里的按钮。
- **启动失败必须可见**：所有启动步骤走 `bootStep`，失败在页顶插红条 `#bootError`（含错误原文/堆栈），**单步失败不带崩后续**。**"看起来合理"的坏状态比崩溃更危险。**
- **对源码做文本判据前先剥注释**：captain 被注释里的 `[hidden]`、以及根因注释里引用的旧代码各骗过一次。实现者的 `hidden_display_none_rule` 会先剥注释——**照它做**。
- **往 Markdown 表格追加行时**：`old_string` 必须**包含已有行**（新行与它同行出现），否则会覆盖掉已有行（captain 在 `ACCEPTANCE.md` 上因此覆盖过 G14/G16，已补回）。
- **"命中自己"的检查容易假通过**：若把"命中的元素是目标的**祖先**"也算通过，就永远看不见**裁剪/遮挡**。现有实现只接受「元素本身或其子孙」。
- **求解器数学全绿 ≠ DOM 应用正确**：captain 用 Node 独立跑过 192 状态布局扫描 + 4180 条交互断言**全绿**，仍没抓到"轨道与 DOM 孩子错位"这类**求解结果→DOM 应用**环节的 bug。**纯数学验证必要但不充分。**
- **在成员施工中途跑验收会得到瞬时假 FAIL**（工作区共享）。**最终验收必须在工作区静默时跑**；出现 FAIL 先重跑一次再判断。

**AgentTeams 流程**

- **评审任务会被空闲成员抢单**。解法：建评审前**先把下一个实现任务建好占住成员**，或建完立刻 `reassign_task`。评审者不得是实现者（第 11–22 轮反复出现）。
- 质量任务的契约里，**不要把实现形状写死**（captain 曾要求"参数加在 `SerialConfig` 上"，结果被证明不可行且更差；也曾在契约里写进两条**不存在/做不到**的规则：1.5 停止位、parity×data_bits 组合，均被实现者正确顶回）。**写目的，不写形状。**
- 模型分工（见全局 AGENTS.md）：规划/分配/审查 = deepseek；代码/测试 = kimi（配额受限时降级 deepseek-official）；多模态/文案 = qwen；grok 暂不可用。**不要乱开模型，控制 token。**

---

## §7 LOG 检索入口（**不要通读**）

| 主题 | 轮次 |
| --- | --- |
| 调研 + 选型 + 计划 | 1 |
| 技术栈改回 Tauri v2 + Phase 0 spike | 2 |
| 沙箱挡 GUI / Tauri 编译通过 / 清阻塞 | 3 |
| Phase 0 通过 + 启动增量 A | 4 |
| 增量 A：零丢失闭环（`ByteSource`/`LogStore`/`selftest`） | 5 |
| Phase 0 全部通过 + 交付门 | 6 |
| 增量 B1：固件指纹采集 + 自实现 SHA-256 交叉校验 | 7 |
| 增量 B2：指纹写进 v2 会话头 | 8 |
| 增量 C1：流式管线与背压 | 9 |
| 增量 C2a：`ByteSink` 发送能力 + `Session` | 10 |
| 增量 C2b：GUI 串口助手面板 | 11 |
| 用户实测 G1–G7；**发现"GUI 会话无指纹"缺口** | 12 |
| 增量 F1：串口参数 + 精确墙钟对应 | 13 |
| 增量 F2：GUI 参数控件 / 时间戳 / 定时发送 | 14 |
| 用户实测 G8–G10；发现布局挤压缺陷 | 15 |
| 布局缺陷根因修复 | 16 |
| 增量 G1：会话列举 + `sessions` 子命令 | 17 |
| 增量 G2：GUI 指纹采集 + 日志列表 | 18 |
| 增量 H1a：区域化布局内核 | 19 |
| 增量 H1b：区域交互 | 20 |
| 布局 harness 抓到真 bug + harness 自身视口缺陷 | 21 |
| 真实 app 右侧整块被裁（RC1）+ harness 两条盲区（RC2） | 22 |
| 增量 H2：功能区侧边栏 + 切换不丢数据（**用户指定的暂停点**） | 23 |
| **H2 启动即崩（TDZ）** + 堵住「启动静默退化」 | 24 |

> README「主要能力」各小节末尾通常给出"证据见 LOG 第 N 轮"的指针，从那里进更快。

---

## §8 开工前必须先处理 / 需用户拍板的事

1. **开工前先与用户确认代码是否已提交**（**最容易踩的一个**）：
   - 2026-10-16 H2 收尾时的实测状态：`HEAD = 6a175c9「初步完善工具GUI界面，解决不同区块冲突问题」`；**H2 的改动尚未提交**——`git status --short` 显示 `M` 的有 `crates/embhub-app/{src/main.rs, ui/index.html, ui/layout.js, ui/layout.test.js}`（H2 代码）、`LOG.md`、`README.md`、`docs/ACCEPTANCE.md`（captain 的文档更新），未跟踪的是 `docs/HANDOFF.md` 本身。
   - **接手时自己再跑一次 `git status --short` 与 `git log --oneline -5` 核实，不要采信本文。**
   - 为什么重要：（a）验收证据无法与 HEAD 对齐；（b）**任何"新代码让旧 bug 变红/变绿"的对照证据都无法被独立复现**——历史上正因如此，"布局闸门对修复前 CSS 变红"的声明**无法被复核**（t17 那轮）。好在 t27/t29 起，实现者改为**在动手前原样保存修复前的文件副本**放进 `target\tmp`，使对照可复跑（如 `gate-red-check-t29.py`）。**这个做法要保留。**
2. **`target\tmp` 与 `target\debug` 的残留**：`embhub-app-test-fp-ghost-repo-*`、captain 的扫描脚本、以及 **3 个改名占用的产物**（`embhub-app.exe.locked-20261015` / `.locked-t14` / `.locked-t29`，合计约 **37 MB**）。都在 `.gitignore` 覆盖内，**可清可不清**；清之前确认没有实例正在运行（否则下次 build 会 exit 101 / os error 5）。
3. **是否需要 `docs/RUNBOOK.md`**：AGENTS 允许按需增添。目前"怎么跑、怎么验收"散在 README、本文 §6 与 `scripts/`；**若你觉得检索成本高，先问用户再建**。
4. **H3c 的闭环验收需要一个实验仓库**：PLAN 第 217 行要求"提交 A 跑出异常日志 → 切到提交 B 不再复现"。**必须先问用户**能否指定一个测试仓库，或同意在 `target\tmp\` 下建临时仓库。**绝不能拿用户现有仓库做实验**（H3a/H3b 已要求只读，但验收脚本会造提交）。
5. **`docs/HANDOFF.md` 本身从未被提交**（未跟踪）。交接完成后建议让用户把它一并提交，否则下一个会话在 `git log` 里看不到它的来历；本文的准确性随代码演化会失效，**提交时间点就是它的有效期起点**。
