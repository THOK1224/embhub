# embhub — 嵌入式开发集成工具 · 实施计划 v0.3

> **版本沿革**：v0.1 推荐 Tauri v2 + Rust → v0.2 因「维护者只掌握 Python」改为 Python 路线 → **v0.3 改回 Tauri v2 + Rust**。
> 依据：[`docs/DECISION-2026-10-14-stack-tauri-v2.md`](DECISION-2026-10-14-stack-tauri-v2.md)（用户已批准）。
> **新前提**：维护者只做路线选择与行为验收、不读代码；目标是小型、稳定、活动部件少。
> 状态：v0.3 已定稿（原第 10 节待确认项作废）。**Phase 0 进行中**：环境、链接、编译、枚举、骨架已完成；Tauri 窗口 spike 卡在网络前置，详见第 9 节。SPEC.md 待 spike 通过后编写。
> 证据来源：[`docs/research/2026-10-14-embedded-tooling-landscape.md`](research/2026-10-14-embedded-tooling-landscape.md)。
> 口径：事实与推断严格分开；未验证的一律写「未核实」。

---

## 1. 结论先行

1. **定位**：不是「又一个串口助手」，而是 **设备会话 + 固件溯源** 工作台。调研结论 C3：市面没有任何工具把「抓到的那段日志」和「当时跑的是哪个 commit / 哪个 bin」绑定起来——因为它们的 Git 是外挂的，结构上做不到。
2. **MVP 判据**：串口能稳定收、日志能落盘、每条会话都带固件指纹。SSH / 波形 / 脚本都是增量，不许挤进 MVP。
3. **技术栈**：**Rust core + Tauri v2 外壳 + 极简 TypeScript 前端**（不用 React/Vue）。产物是单个自包含 `.exe`。
4. **三条硬约束**（比选型更重要）：
   - **core 层不得依赖 UI 层**，必须能被 `cargo test` 单独测试，并有 CLI 无头入口。
   - **数据通道必须批量 + 有背压**，落盘路径与显示路径解耦：显示可以丢帧，落盘一个字节都不能丢。
   - **固件指纹从 Phase 1 第一天就写进日志头**。事后补是补不回来的——现场日志往往只剩一个文件。
5. **三条减法**（防止 agent 堆复杂度，应写进验收标准）：
   - 前端**不用 React/Vue**，原生 TypeScript + 一层有类型的 Tauri command，页面只有三个：串口 / SSH / Git。
   - **Git 不引 libgit2 / gitoxide**，直接调系统 `git` CLI。行为与命令行完全一致，出问题可用命令行复现。这是降复杂度，不是偷懒。
   - **SSH 第一期不做终端仿真**：先做命令执行 + 输出显示 + SFTP 上传下载；交互式终端（xterm.js）留到 Phase 4。

---

## 2. 范围

### 2.1 一句话定义

跨平台桌面工作台：把「连开发板（串口/JTAG）」「连主机（SSH/SFTP）」「管代码与产物（Git + 固件）」「看数据（文本/HEX/波形）」放进同一条**可保存、可回放、可脚本化**的工作流。

### 2.2 做 / 不做

| 做 | 不做（明确划界） |
| --- | --- |
| 串口：多端口并发、文本/HEX/时间戳、分片落盘、离线回放 | 不做通用终端仿真器的全功能对标（Tabby/WezTerm/Alacritty 已过度竞争） |
| 会话模型：会话 = 数据源 + 解析 + 落盘 + 元数据，可存工程文件 | 不做「又一个 Git GUI」（Lapce/VS Code 的 Git 面板够好了） |
| 固件指纹：commit + dirty + bin hash，与日志强绑定 | MVP 不做自动更新、云同步、账号体系 |
| 解析：内建行/HEX/定长帧；解析逻辑与 UI 解耦 | 不做脚本沙箱（本地单用户工具，运行的是用户自己的解析逻辑；沙箱带来复杂度而非安全收益） |
| SSH/SFTP：定位是「连板子/编译机干活 + 传固件」 | 不做 X11/RDP/VNC（MobaXterm 的地盘，非嵌入式主需求） |
| 绿色便携：不写注册表、单配置目录 | 不为了「跨平台」在 MVP 阶段分散精力（Windows 优先，架构不堵死 Linux/macOS） |

### 2.3 三个核心场景

- **A 单板调试**：插板 → 建会话 → 抓日志 → 发现异常 → 一键跳到对应 commit 看 diff。
- **B 多板联调**：同时抓 3 块板子，统一时间轴对齐，按设备打标签（调研 C2：现有工具最多做到逐设备启停，没有会话矩阵视图）。
- **C 现场/远程**：SSH 连到 Linux 板子或编译机，传固件、跑命令，日志同样落盘并绑定指纹。

---

## 3. 差异化（每条都有调研证据）

| # | 空位 | 证据 |
| --- | --- | --- |
| C1 | 「串口+SSH+Git+烧录」四合一不存在 | Tabby 有 `tabby-serial`/`tabby-ssh` 内置插件，但 `tabby-git` 实测 404；Serial Studio 有可视化解码但无 SSH/Git；ESP-IDF 扩展有构建烧录但 Git 靠宿主 VSCode |
| C2 | 多设备会话编排 | SerialTest 只能逐设备启停；ESP-IDF 扩展一次只对一个芯片+一个串口。会话矩阵视图：未检到 |
| C3 | **日志 ↔ 固件指纹 ↔ Git 提交（最大空位）** | probe-rs 的 defmt 能让日志带源码位置，但没有工具在抓日志时记录固件指纹。日志一落盘就与「当时跑的是哪个 bin」失联 |
| C4 | 协议解析可复用 | Serial Studio 用配置式项目文件而非可编程解析；多数串口助手遇到自定义二进制帧只能自己拉 Python |
| C6 | 数据源统一抽象 | 「串口、RTT、TCP、文件回放都是字节流来源」这一层未被现有产品统一（判断） |

**差异化的最小落地形态**：每份日志都带 `{repo, commit, dirty, bin_hash, session_id, wall_clock}`；UI 支持「按固件版本过滤日志」+「一键切到出问题的 commit」。

> **对「编译器挡错」的诚实说明**：Rust 的类型/借用检查只能在**编译期**挡掉一整类错误，它**不能**替代行为验收。本项目维护者不读代码，因此**验收必须靠可执行的验收脚本**（见第 8 节），不能靠「能编译过」当交付证明。

---

## 4. 架构

### 4.1 分层

```
┌── Tauri v2 窗口（Windows 走 WebView2；本机已核实 155.0.4283.45 就位）──┐
│  工作台外壳：可停靠面板（参照 JupyterLab 的 dock 布局）                 │
│  终端面板(xterm.js) 波形面板(uPlot) 日志面板 会话管理器 Git/SFTP 面板   │
└───────────────┬────────────────────────────────────────────────────────┘
                │ Tauri IPC：有类型的 command + event（二进制、批量、有背压）
┌───────────────▼──── embhub-core（Rust crate，★ 不依赖 Tauri ★）──────┐
│  source/     ByteSource 抽象（trait）                                 │
│              ├ SerialSource   serialport-rs（阻塞 I/O + 独立线程）     │
│              ├ SshChannel     russh 的 exec/shell 通道                 │
│              ├ ReplaySource   文件回放（与实时同一管线）               │
│              └ RttSource      probe-rs CLI 子进程（Phase 7）           │
│  parse/      行 / HEX / 定长帧 / 分隔符 → Frame                       │
│  store/      分片 append-only 二进制 + rusqlite（bundled）索引         │
│  fingerprint/ 固件指纹：commit+dirty+bin hash+构建时间+芯片            │
│  vcs/        系统 git CLI 薄封装（std::process::Command，--porcelain）  │
│  session/    Session 模型 + .embproj 工程文件                         │
├───────────────────────────────────────────────────────────────────────┤
│  embhub-cli：同一 core 的无头入口（脚本化 / CI / 批量回放，验收用）     │
└───────────────────────────────────────────────────────────────────────┘
        ▲ Tauri 层（embhub-app）只是薄适配：把 core 的接口暴露成 command
```

**分层铁律**：`embhub-core` 的 `Cargo.toml` 里**不允许出现 tauri 依赖**。违反这条，可测试性与 CLI 化同时报废。

### 4.2 五个核心抽象（一次做对的东西）

1. **`ByteSource`** — 所有数据源的统一接缝：串口、SSH 通道、RTT、文件回放都实现它。**回放走同一管线是硬要求**：能实时看的才能事后重放；能重放的才谈得上复现。
2. **`Session`** — `{ 数据源配置, 解析器链, 落盘策略, 设备标签, 指纹 }`，可序列化为 `.embproj` 工程文件，跨机器可携带（调研 C5）。「设备即工作区对象」的落地。
3. **`Frame`** — 解析后的结构化记录：`{ 单调时间戳, 原始字节, 解析字段, 会话 id, 指纹 }`。**时间戳在到达数据源线程时打**，不是 UI 渲染时打，否则日志时间轴不可信。
4. **`LogStore`** — 分片 append-only 文件（原始字节，保证不丢）+ SQLite 索引（会话元数据、帧偏移、指纹）。按大小/时间切片，支持按时间轴 seek。
5. **`FirmwareFingerprint`** — `{ repo, commit, dirty, bin_hash, build_time, chip, probe_id }`。

### 4.3 数据通道与背压（最容易被忽略、后果最严重）

- 逐条跨 WebView 边界推送会直接卡死 UI。**规则：core 侧按「≥8 KB 或 30–60 Hz，先到先发」批量推送。**
- **落盘路径永远优先且无损**；IPC 发送队列满时丢的是「显示」，不是「数据」，并且必须在 UI 上明确提示「显示已丢弃 N 条」。
- 串口读取在独立线程，禁止在 UI 线程或异步运行时里做阻塞读。

---

## 5. 技术选型（Rust）

### 5.1 本机实测环境事实（2026-10-14）

| 项目 | 实测结果 | 影响 |
| --- | --- | --- |
| WebView2 Runtime | **已就位**：注册表 `pv = 155.0.4283.45`，`C:\Program Files (x86)\Microsoft\EdgeWebView\Application\155.0.4283.45` | Tauri 运行前提已满足 |
| MSVC（VS2022 VC.Tools.x86.x64） | 已装 `D:\study\vs2022\environment` | Rust MSVC 链接前提已满足（Windows 上最大的坑已过） |
| 操作系统 | Windows 11，NT 10.0.26100 | ConPTY 可用（Win10 1809+） |
| 串口 | **实测在线 9 个**（COM3–COM12） | spike 有真实设备可用 |
| git | 2.53.0 | Git 功能直接调 CLI |
| Python | 3.14.4（**本路线不再需要**） | — |
| Node / npm | 未安装（**spike 阶段不需要**，见 5.3） | — |
| **工作区外写入** | **被沙箱拒绝**（`%USERPROFILE%` 下建目录 → 访问被拒绝） | 见 5.4，这是本路线最关键的工程约束 |

### 5.2 依赖映射

| 能力 | 选型 | 备注 |
| --- | --- | --- |
| 串口 | **serialport-rs** | 备选 `tokio-serial`；风险见 R1 |
| SSH / SFTP | **russh** | 纯 Rust，无 libssh2 系统依赖 |
| Git | **系统 `git` CLI**（`std::process::Command`） | 刻意不引 libgit2/gitoxide |
| 存储 | **rusqlite（`bundled` feature）** | 静态链接 SQLite，免系统依赖 |
| IPC 网关 | **先用 Tauri IPC** | 少一个依赖；若 IPC 吞吐不够再考虑 axum（Phase 1 的性能验收会暴露） |
| 桌面外壳 | **Tauri v2** | — |
| 前端终端 | **xterm.js** | 本地 vendor，MIT |
| 前端波形 | **uPlot** | 本地 vendor，MIT；为高速流式时序设计 |
| 前端构建 | **Vite + TypeScript** | 取舍见 5.3；Node 只是**构建期**依赖 |
| 测试 | `cargo test` | core 独立可测 |
| 打包 | `tauri build`（.msi / .exe） | — |

### 5.3 关于 Node 的诚实说明

Tauri 的 CLI 本身是 Node 包（也可 `cargo install tauri-cli` 用 `cargo tauri`），而 Vite + TypeScript 需要 Node。**差别在于 Node 是构建期工具，不是运行时依赖**——产物 exe 里没有 Node。这是与 Electron 的决定性区别（Electron 的 Node 是产品的一部分，还要长期养 `@electron/rebuild` 与 node-pty 两个随版本重编译的原生模块）。

**spike 阶段不需要 Node**：Tauri 可以直接加载一个静态目录（一个 `index.html`），足以验证「窗口 + 列串口 + 打开 + HEX 显示」。Node 等进入正式前端开发（Phase 1）再装。

### 5.4 沙箱约束与对策（本路线成败所在）

**问题**：当前 DSH 文件策略是 `workspace-write`，**工作区外写入被拒绝**。而 rustup 默认写 `%USERPROFILE%\.rustup`（约 1.5 GB）、cargo 每次拉依赖写 `%USERPROFILE%\.cargo\registry`。按默认装法，**每一步 `cargo build` 都要提权，开发回路是断的**。

**对策（已采用）**：把工具链根目录放进工作区——

```
RUSTUP_HOME = <workspace>\.rustup
CARGO_HOME  = <workspace>\.cargo
```

两者均加入 `.gitignore`。这样工具链与依赖缓存都在工作区内，编译无需提权。

**代价**：工作区多出约 1–2 GB 目录；不该提交进 git。**备选**：把 DSH 本工作区的文件策略放宽到 `danger-full-access`，即可用标准安装位置——这是你的选择，不是我的。

---

## 6. 分阶段路线图

每阶段都有可执行验收，不通过不进入下一阶段。工作量为粗估人日，仅供排优先级。

### Phase 0 — spike + 骨架（1–1.5 人日）

**第一步不是写功能，是证明最危险的一环。**

**spike 范围**：最小 Tauri 窗口 → 枚举 COM 口 → 选择一个 → 打开 → 收到数据按 HEX 滚动显示。

**通过判据（全部满足）**：
- `cargo tauri dev`（或 `cargo run`）在本机出窗口；
- 插拔 USB 串口后端口列表能刷新；
- 115200 下能收到真实数据（本机实测在线 9 个口：COM3–COM12）。

**失败 / 切换判据（满足任一，即切回 Python + pywebview 路线，架构与五个抽象不受影响，只损失已投入时间）**：
- rustup / 工具链装不上，或 MSVC 链接失败；
- `serialport-rs` 或 Tauri 关键 crate 编译不过，且 4 小时内无解；
- 累计超过 **1 个工作日**仍出不了窗口。

**用户需要亲自验收的只有两件事**（不需要读代码）：① 插拔串口设备后列表是否刷新；② 打开一个口后是否收到真实数据。

**spike 通过后的骨架交付**：workspace 分 crate（`embhub-core` / `embhub-app` / `embhub-cli`）、`cargo test` 基线、`cargo fmt --check` + `clippy -D warnings`。

### Phase 1 — 串口最小闭环 + 固件指纹（3–5 人日）★ MVP-1
- 端口枚举（含 VID/PID/序列号）→ 打开/关闭 → 收发 → 文本/HEX/带时间戳显示 → 分片落盘。
- **`FirmwareFingerprint` 第一天就写进会话头**（Git 部分先调 `git` CLI，不做 UI）。
- 前端转 Vite + TypeScript（此时装 Node）；IPC 按 4.3 做批量 + 背压。
- 回环测试：对传 N MB，校验收发字节数一致、无丢帧。
- **验收**：① ≥1 小时高波特率收发无丢字节，有计数证据；② 落盘文件能被 `ReplaySource` 完整回放；③ 高吞吐下 UI 不假死（背压生效，且显示丢弃有提示）。

### Phase 2 — 会话模型与多设备（3–5 人日）
- `Session` + `.embproj` 工程文件；多标签；会话矩阵视图（N 端口统一时间轴）；断线重连；设备标签。
- **技术验证 spike（可能砍）**：被动监听「已被别的程序占用的串口」。Windows 上可能需要内核过滤驱动，**结论未知**，先花 0.5 人日验证；不成则砍，或降级为「提示用虚拟串口对」。
- **验收**：同时开 3 个会话互不干扰；工程文件在另一台机器可复现打开。

### Phase 3 — 波形与解码（3–4 人日）
- uPlot 流式绘图（环形缓冲 → 降采样 → 批量推送）；通道定义 UI；内建解析器：行 / 分隔符 / HEX / 定长帧 / 校验和。
- **验收**：1 kHz 数据流绘图不掉帧；解析结果可回溯到原始字节偏移。

### Phase 4 — SSH / SFTP（3–5 人日）
- **第一期不做终端仿真**（见第 1 节减法 3）：russh 命令执行 + 输出显示 + SFTP 上传下载。
- 主机密钥校验（TOFU + known_hosts）、密码/密钥认证。
- 交互式 PTY 终端（xterm.js 渲染）在本阶段收尾时补上；SSH 通道作为 `ByteSource` 复用解析与落盘管线。
- **验收**：连上一台 Linux 板子，跑命令 + 传文件，日志同样带指纹落盘。

### Phase 5 — Git + 固件溯源（3–5 人日）★ 差异化核心
- git CLI 封装：仓库状态、提交日志、diff、blame；`bin_hash` 与构建时间采集。
- **日志面板按固件版本过滤；从一条日志一键跳到对应 commit 的 diff。**
- **验收**：构造「提交 A 跑出异常日志 → 切到提交 B 不再复现」的完整闭环，全程不离开本工具。

### Phase 6 — 解析扩展与回放复用（2–3 人日）
- 解析器注册机制；**同一解析逻辑在实时与回放两条路径复用**。
- **验收**（这是本阶段唯一重要的验收）：同一份解析逻辑在实时与回放模式下产出**逐字节一致**的结果，用同一份日志做对照测试。

### Phase 7 — 烧录与调试集成（可选，3–5 人日）
- **关键洞察：不需要写新的 Rust 驱动代码**。`probe-rs` / `openocd` / `esptool` 都是现成 CLI，作为子进程编排即可。
- `probe-rs run` 的输出作为 `RttSource`（= flash + RTT/defmt 日志流）；构建产物自动入库并打指纹。
- **验收**：一键「烧录 + 起 RTT + 自动打指纹 + 开始抓日志」。

### Phase 8 — 打包发布（2–3 人日）
- `tauri build` 出 `.msi` / 便携 exe；绿色便携模式（不写注册表、单配置目录、配置可整体拷走）；按官方瘦身手段（LTO / `opt-level="s"` / `strip` / `panic=abort`）压体积；可选代码签名。
- **验收**：在干净机器上解压即用，无任何运行时依赖。

**总计粗估 25–40 人日**（不含 Phase 7）。

---

## 7. 风险与对策

| # | 风险 | 级别 | 对策 |
| --- | --- | --- | --- |
| R1 | **`serialport-rs` 的 Windows 维护者缺位**（调研已核实：README 公开招维护者） | 中 | spike 阶段即暴露；备选 `tokio-serial`，或 `windows-rs` 直接调 Win32 API |
| R2 | **工作区外写入被沙箱拒绝**，默认装法会让每次 `cargo build` 都要提权 | 高（已定性） | 已采用：`RUSTUP_HOME`/`CARGO_HOME` 落到工作区内（见 5.4）。备选是你放宽 DSH 文件策略 |
| R3 | rustup 首次安装为 GB 级下载；首次编译 5–15 分钟 | 低 | 一次性成本 |
| R4 | agent 写 Rust 迭代慢 2–3 倍（借用检查 + 每次 `cargo build`） | 中 | 接受；用「编译通过」换取一部分验收质量——但**不能替代行为验收**（见第 3 节说明） |
| R5 | 被动监听「已被占用的串口」在 Windows 可能需内核过滤驱动 | 中 | Phase 2 先 spike，不成则砍 |
| R6 | 高吞吐下 UI 卡死 / 丢数据 | 高 | 4.3 的批量 + 背压；落盘与显示解耦 |
| R7 | 范围蔓延 | 高 | 第 1 节三条减法 + MVP 判据 |
| R8 | 许可证 | 低 | Tauri/Rust 生态以 MIT/Apache-2.0 为主；**分发前逐项核实**，不抄 GPL 代码（Serial Studio 为 GPLv3/商业双许可） |
| R9 | Tauri IPC 吞吐不足以承载高波特率流 | 中 | Phase 1 的性能验收专门测这一条；不够则加 axum 本地 WS 网关 |
| R10 | 日志格式定下后难改 | 中 | Phase 1 就把格式版本号写进日志头 |

---

## 8. 项目级验收标准（SPEC 会展开）

**因为维护者不读代码，验收必须是可执行的脚本，而不是「看起来对」。**

1. `embhub-core` 可独立 `cargo test`，且 `Cargo.toml` 中**无 tauri 依赖**（可被一条测试或 CI 检查断言）。
2. 串口数据**零丢失**有可复现的测试证据（计数校验，不是「看起来正常」）。
3. 任何一份落盘日志都能回答：哪个设备、什么时候、什么固件（commit+bin hash）、用什么解析器。
4. 同一解析逻辑在实时与回放模式下结果一致。
5. `embhub-cli` 能无头跑通「打开数据源 → 抓 N 秒 → 落盘 → 回放校验」，可用于 CI 与人工验收。
6. 绿色便携：不写注册表，配置可整体拷走。
7. Windows 为主目标平台，架构上不堵死 Linux/macOS。

---

## 9. Phase 0 执行状态

| # | 事项 | 状态 | 证据 |
| --- | --- | --- | --- |
| 1 | 装 Rust 工具链（装进工作区） | ✅ 完成 | cargo/rustc 1.99.0（`stable-x86_64-pc-windows-msvc`），`<ws>\.cargo`、`<ws>\.rustup` |
| 2 | 验证 MSVC 能否链接 | ✅ 完成 | `link.exe` 找到 kernel32.lib 等；第 1 轮「Windows SDK 缺失」的担心是误判 |
| 3 | 验证 serialport-rs 能否编译 | ✅ 完成 | serialport 4.10.1，首次全量编译 11.18 s |
| 4 | 真实串口枚举 | ✅ 完成 | 枚举到 10 个端口（COM3–COM12）；**类型全为 `unknown`，VID/PID 路径未验证** |
| 5 | crate 骨架（core / cli / app） | ✅ 完成 | 代码已写，core 无 tauri 依赖 |
| 6 | WebView2 就位 | ✅ 完成 | 155.0.4283.45（注册表 + 安装目录双确认） |
| 7 | **Tauri app 编译通过** | ✅ **完成** | `cargo build -p embhub-app` exit 0（34.52 s，无需提权）；tauri 2.12.4 / wry 0.57.0 / tao 0.37.1。首次失败是缺 `icons/icon.ico`，已用 `scripts/make_icon.py` 生成 |
| 8 | `cargo test` 全绿 | ✅ **完成** | `cargo test -p embhub-core -p embhub-cli` → 2 passed，0 failed |
| 9 | **spike 窗口** | ✅ **通过** | 用户在沙箱外 `cargo run -p embhub-app` 目视确认：窗口正常、枚举出 10 个串口、「刷新端口」可用 |
| 9b | 插拔后列表刷新 / 115200 收到真实数据 | ⏸ **用户主动延后** | 手头无 USB 设备；用户后续自验，**通过则进入 Phase 1，有问题则按第 6 节切换判据回 Python 路线** |
| 10 | 写 SPEC.md | ✅ **已完成（提前）** | [`docs/SPEC.md`](SPEC.md)：契约部分与技术栈无关，不依赖 spike 结果 |
| 11 | 静态检查基线（`rustfmt` / `clippy`） | ✅ 完成 | 组件已装；`cargo fmt --all --check` exit 0，`cargo clippy --all-targets -- -D warnings` exit 0 |

### 9.1 Phase 1 增量 A：零丢失闭环（✅ 已验收，2026-10-15）

| 项 | 状态 | captain 实测证据 |
| --- | --- | --- |
| `ByteSource` 接口 + Serial/Synthetic 两实现 | ✅ | `source.rs`；`SourceError` 区分无数据与致命断开；块带单调到达时间 |
| `LogStore` 写入/回放 + 会话头 + 分片 + CRC32 | ✅ | `store.rs`；`EMBHLOG\0` / 版本 1 / 指纹占位；`open_replay` 逐字节一致 |
| `cli selftest`（无硬件证明零丢失） | ✅ | `--bytes 67108864` → 注入 == 落盘 == 回放 == 67108864，exit 0 |
| 测试基线 | ✅ | `cargo test`：core **24 passed** + cli **8 passed** |
| 静态检查 | ✅ | clippy `-D warnings` 与 fmt `--check` 均 exit 0 |
| 零新增依赖 / core 无 tauri | ✅ | `Cargo.lock` 未改动；测试 `core_has_no_tauri_dependency` 通过 |
| 不破坏 Phase 0 能力 | ✅ | `list` 列出 10 个串口；`build -p embhub-app` exit 0 |

**已记录的残留缺口**：`selftest` 未做端到端故障注入，「运行时计数对不上 → 非零退出」只有 `verdict()` 单测覆盖（captain 判定可接受：为恒真单行映射在生产代码加钩子，设计上更差）。

**实现者变更记录**：原定 `kimi-k3`，因项目限速 `并发数1 / RPM 3` 累计 6 次 attempt 均 429，经用户决定改用 `deepseek-official` 完成（详见 [`LOG.md`](../LOG.md) 第 4、5 轮）。

---

**Phase 0 遗留阻塞（已解除）**：GUI 窗口无法在 DSH 沙箱内启动——该问题已由用户在沙箱外启动验证通过。

- 已排除配置原因：数据目录已显式指向工作区内（`EBWebView` 成功生成 115 项），`--no-sandbox --disable-gpu` 也无效；
- 现象：WebView2 子进程（`msedgewebview2.exe`）被拉起，但 controller 创建失败；
- 判断：沙箱的受限令牌/命名管道/窗口站限制所致（与「pip TLS 失败」「工作区外写入被拒」同类）；
- **结论**：**GUI 必须在沙箱外启动**。

**合作方式（已确认并沿用）**：联网命令与 GUI 启动由用户在自己的终端执行；不联网的读写与编译（依赖已就位时）由 agent 直接做。

**用户在普通终端执行**（无沙箱，不会遇到上述限制）：

```powershell
. .\scripts\env.ps1                     # 必须点源；被执行策略拦时先 Set-ExecutionPolicy -Scope Process Bypass
cargo test -p embhub-core -p embhub-cli
cargo run -p embhub-app
```

> 沙箱内曾出现的 `Start-Process : 已添加项。字典中的关键字:"NO_PROXY"...` 是 DSH 环境变量大小写重复导致的 PowerShell 问题，与项目无关；直接 `cargo run` 不会碰到。

然后按第 6 节判据验收两件事：① 插拔串口后点「刷新端口」列表是否变化；② 选端口点「打开并读取」是否收到字节数与 HEX。


---

## 10. 已作废

原 v0.2 的待确认项（pywebview ① / PySide6 ②）**随 D1′ 决策失效**——两者都属 Python 路线。本路线下无需再向用户确认技术选项；需要用户参与的只有第 6 节 spike 判据里的两件事（插拔刷新、收到真实数据）。
