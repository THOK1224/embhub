# embhub

面向嵌入式开发的桌面集成工具：把**串口调试**、**SSH/SFTP**、**Git** 放进同一条工作流，
并把每份日志与**当时运行的固件指纹**（commit + dirty + bin hash）绑定起来。

> **当前状态：Phase 0 已通过；Phase 1 增量 A、B1、B2、C1、C2a、C2b、F1、F2 已验收。当前目标：串口助手基本功能。**
> ✅ **Phase 0 全部通过**：Rust 工具链、MSVC 链接、Tauri v2 编译、WebView2 就位、窗口正常打开、**插拔串口后列表能刷新**、**115200 下能收到板子发来的真实数据**。
> ✅ 增量 A：`ByteSource` + `LogStore` + `cli selftest`（零丢失闭环）。证据见 [`LOG.md`](LOG.md) 第 5 轮。
> ✅ 增量 B1：`fingerprint` 固件指纹采集 + 自实现 SHA-256（bin_hash 与 Windows `Get-FileHash` 交叉校验一致）。证据见 [`LOG.md`](LOG.md) 第 7 轮。
> ✅ 增量 B2：指纹写进会话头，**日志格式升到 v2**，`selftest --repo` 端到端从磁盘读回比对；captain 另做了不经 Rust 的字节级校验（头长 558 = 24+486+4+44）。证据见 [`LOG.md`](LOG.md) 第 8 轮。
> ✅ 增量 C1：**流式管线与背压**（SPEC 4.3）。captain 用两项独立验证确认「显示可丢、落盘不可丢」：把显示队列容量设为 **0**（显示 100% 丢弃）时落盘仍零丢失；并用**纯 PowerShell 直接解析磁盘分片**数出 payload 536,870,912 == 源产出，**差 = 0**。证据见 [`LOG.md`](LOG.md) 第 9 轮。
> ✅ 增量 C2a：**发送能力 `ByteSink` + `Session` 最小实现 + `cli send`/`loopback`**。`loopback --bytes 1000000` 证明「写入 == 读回 == 落盘 == 回放」。证据见 [`LOG.md`](LOG.md) 第 10 轮。
> ✅ 增量 C2b：**GUI 串口助手面板**（打开/关闭会话、接收区文本↔HEX、发送区文本/HEX + 追加 CR/LF/CRLF、RX/TX/显示丢弃 计数）。captain 验到「能编译 + 命令/事件/元素 id 接线对称 + 适配层 8 个单测」，**交互部分无人点过**（沙箱内 WebView2 起不来），已移交为 G1–G7 用户验收项。证据见 [`LOG.md`](LOG.md) 第 11 轮。
> ⚠️ **待你验**：`.\scripts\accept.ps1` 跑自动项（19 条）＋ [`docs/ACCEPTANCE.md`](docs/ACCEPTANCE.md) 第 3 节的 **G1–G10**（含**真机发送**、**串口参数真机生效**、**定时发送停止后不再发**）。
> 全项目测试基线：**core 76 + cli 27 + app 17 = 120 passed**；`clippy -D warnings` 与 `fmt --check` 干净。

---

## 为什么值得做（一句话）

市面上的工具各占一半：Tabby 有串口 + SSH 但**没有 Git**（`tabby-git` 实测 404），
Serial Studio 有可视化解码但没有 SSH/Git，ESP-IDF 扩展有烧录但 Git 靠宿主 IDE。
**「串口 + SSH + Git + 烧录」四合一没有开源对标品**；而最关键的空白是
**日志 ↔ 固件指纹 ↔ Git 提交** 的可追溯关联——竞品结构上做不到，因为它们的 Git 是外挂的。

完整证据（68 个仓库的 API 快照 + manifest 原文）见
[`docs/research/2026-10-14-embedded-tooling-landscape.md`](docs/research/2026-10-14-embedded-tooling-landscape.md)。

---

## 目录结构

```
embhub/
├── Cargo.toml                  # workspace：core / cli / app
├── crates/
│   ├── embhub-core/            # ★ 核心层：与 UI 无关，可独立 cargo test
│   │   └── src/lib.rs          #   串口枚举 / 读取（spike 版，Phase 1 重构为 ByteSource）
│   ├── embhub-cli/             # 无头入口：list / read（脚本化、CI、人工验收）
│   └── embhub-app/             # Tauri v2 桌面外壳（只做 core 的薄适配）
│       ├── src/main.rs         #   command: list_ports / read_probe；窗口在 setup() 里建
│       ├── tauri.conf.json     #   windows 置空（窗口由代码创建，以便指定 data_directory）
│       ├── capabilities/
│       ├── icons/icon.ico      #   由 scripts/make_icon.py 生成（tauri-build 必需）
│       └── ui/index.html       #   spike 前端（原生 JS，无 Node/无构建步骤）
├── scripts/
│   ├── env.ps1                 # 开发环境变量（RUSTUP_HOME/CARGO_HOME/TMP 全落在工作区）
│   └── make_icon.py            # 生成应用图标（用 DSH 捆绑的 Python + Pillow）
├── docs/
│   ├── SPEC.md                 # 设计原则 / 五个模块契约 / 边界 / 验收标准
│   ├── PLAN.md                 # 实施计划 v0.3（架构 / 选型 / 8 个阶段 / 风险 / 验收）
│   ├── DECISION-2026-10-14-stack-tauri-v2.md   # 技术栈决策记录
│   └── research/               # 调研报告 + 原始证据（raw/）
├── LOG.md                      # 施工日志（每轮做了什么、为什么、验证了什么）
├── .gitignore                  # 排除 .cargo/ .rustup/ target/ 等
└── AGENTS.md                   # 工作区约束
```

> `docs/SPEC.md` 已先行写就——因为它的契约部分与技术栈无关，不依赖 spike 结果。若 spike 失败切回 Python 路线，第 3 节的契约照旧成立，只有「实现形态」示例需要改写。

---

## 怎么跑

### 环境前置

| 依赖 | 本机状态 |
| --- | --- |
| Rust（MSVC 工具链） | 已装，**在工作区内**：`.cargo\bin`（cargo 1.99.0 / rustc 1.99.0） |
| MSVC（VS2022） | 已装 `D:\study\vs2022\environment` |
| WebView2 Runtime | 已就位 155.0.4283.45 |
| Node / npm | **不需要**（spike 前端是静态 HTML，`withGlobalTauri` 直接用 `window.__TAURI__.core.invoke`） |

### 设置环境（每个新终端都要做一次）

```powershell
. .\scripts\env.ps1
# 必须点源（前面的点）。若被执行策略拦：先 Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass
# 不要用 pwsh -File：那是子进程，环境变量不会留在你的终端
```

这个脚本把 `RUSTUP_HOME` / `CARGO_HOME` / `TMP` / `TEMP` **全部指向工作区内**。
原因（实测，见 LOG 第 2 轮）：

- 当前 DSH 沙箱是 `workspace-write`，**工作区外写入被拒绝**，而 rustup/cargo 默认写 `%USERPROFILE%`；
- `link.exe` 用系统 TMP 会报 `LNK1104`，必须把 TMP 也指进工作区。

### 命令

```powershell
cargo test -p embhub-core -p embhub-cli      # 单元测试
cargo run --quiet -p embhub-cli -- list      # 无头：列出串口
cargo run --quiet -p embhub-cli -- read --port COM3 --seconds 3   # 无头：读取并统计字节数
cargo run -p embhub-app                      # 启动 spike 窗口
```

> 首次编译需要访问 crates.io 拉依赖。若在 DSH 沙箱会话里执行，会因 schannel 拿不到 TLS 凭据而失败
> （`SEC_E_NO_CREDENTIALS`）——**请在普通终端里跑**。依赖进了工作区内的 `CARGO_HOME` 后，后续编译无需网络。

---

## 主要能力

### 已实现

**Phase 0 spike（会重构）**

- `embhub-core::list_ports()` —— 跨平台串口枚举（端口名、类型、USB VID/PID/序列号/产品名）。
- `embhub-core::read_port()` —— 打开端口、按波特率读取、返回**字节数**与 HEX 预览。
- `embhub-app` —— Tauri 窗口：刷新端口、选择端口与波特率、打开读取并显示结果。

**Phase 1 增量 B1：固件指纹采集（已验收）**

- `embhub-core::fingerprint` —— SPEC 3.5 的 `FirmwareFingerprint`：`repo_root` / `commit`（强制 40 位 hex）/ `dirty`（`git status --porcelain` 非空）/ `bin_path` / `bin_hash`（**内容**哈希）/ `build_time` / `chip` / `probe_id`。
  取不到的字段用 `Field<T> { Value(T), Missing(String) }` 表示——**「看着有指纹、其实是空的」在类型上不成立**。
- `embhub-core::sha256` —— 自实现的流式 SHA-256（纯 std，**零新增第三方依赖**），与 Windows `Get-FileHash` 在真实文件上逐字符一致。
- `embhub-cli fingerprint --repo <dir> [--bin <file>]` —— 打印全部字段，缺失字段打印原因并汇总清单；采集失败仍以 0 退出（**不是命令失败**），仅缺 `--repo` 时以 1 退出。

**Phase 1 增量 F1/F2：串口参数、时间戳、定时发送（已验收，交互待你验）**

- **串口参数完整化**（t13）：`DataBits`(5/6/7/8)、`Parity`(None/Odd/Even)、`StopBits`(1/2)、`FlowControl`(None/RTS-CTS/XON-XOFF) 四个枚举 + `SerialFrame`，由**唯一落点** `build_serial_builder` 真正链到 serialport builder（读写两条路径共用）。非法组合（5 数据位+2 停止位等）给**点名冲突**的错误，**绝不静默降级**。
  - 已知边界：`serialport 4.x` **没有 1.5 停止位**，故 `StopBits::OneAndHalf` 会返回明确错误，界面**不提供**该选项（代码里有注释说明原因）。
- **精确墙上时钟对应**（t13）：`SessionStart { wall_ms, mono_micros }` 在会话创建点背靠背取样 → `SessionSnapshot::wall_time_ms_for(t_mono)` 精确换算。**不用"第一批到达时间"近似**（有确定性测试：断言起始点落在 `start()` 前后两次取样的区间内）。
- **GUI 参数控件 / 时间戳开关 / 定时发送**（t14）：`open_session` 改收配置对象；四个参数选择器**运行时禁用**并在状态行显示实际生效的帧格式；接收区 `[HH:MM:SS.mmm]` 时间戳开关（切换无损）；定时发送（间隔 + 起停，**复用与手动发送同一个帧构造**，关会话自动停且有第二道闸）。

  ```
  # 无硬件验证串口参数与时钟的底层映射（真机生效需用板子验，见 G8）
  .\scripts\cargo.cmd test -p embhub-core        # 76 passed，含 data_bits/parity/stop_bits/flow_control 映射
  ```

> ⚠️ **这四项参数是否真的改变硬件通信行为，尚未真机验证**（本机无第二串口对端，且不向未知设备写入）。请用 G8 验证。

**Phase 1 增量 C2b：GUI 串口助手面板（已验收，交互部分待你验）**

- 4 个 Tauri 命令：`open_session(port, baud)` / `close_session()` / `session_stats()` / `send_data(data, mode, append)`；命令与显示批次事件的**接口契约固定**，适配层全部留在 `crates/embhub-app`（core 不依赖 tauri）。
- 事件 `session-batch`，载荷 `{ seq, t_mono, hex }`（`seq` 从 1 严格递增，`hex` 大写无分隔符）；泵在采集线程结束后会**排空尾部批次**再退出（否则最后几批会"已落盘却没上屏"）。
- 界面：刷新端口 / 选端口与波特率 / 打开·关闭（按钮状态随会话变）/ 接收区 **文本↔HEX** / 清空 / 自动滚动 / 发送区 **文本·HEX** + 追加 `none·CR·LF·CRLF` / **RX·TX·显示丢弃** 三计数 / 错误面板。原生 HTML/JS，**零构建依赖**。
- 显示区只保留最近 **256 KiB**（仅影响上屏；落盘与计数不受影响，界面有明示）。
- 失败可见：无端口 / 不在端口列表 / 可能被占用 都给出**可行动原因**，不是笼统"失败"。

> **这段的交互没人点过**：DSH 沙箱内 WebView2 起不来，captain 只验到"能编译 + 接线名字全对 + 适配层 8 个单测"。打开窗口、接收、发送请按 [`docs/ACCEPTANCE.md`](docs/ACCEPTANCE.md) 第 3 节的 **G1–G7** 走一遍。

**Phase 1 增量 C2a：发送能力与 Session（已验收）**

- `embhub-core::source::ByteSink` —— **发送侧接口**（`write_all` / `flush`），刻意与只读的 `ByteSource` 分开：发送不是"反过来读"，读句柄与写句柄可以是两个独立对象（`try_clone()`），失败模式也不同。
- `SerialSource::try_clone_sink()` —— 用 serialport `try_clone()` 给出**共享同一物理端口**的写句柄。
- `SyntheticSource::loopback()` —— **回环**源：写进去的字节被同一个源读回，使"能发"这件事**无需硬件即可端到端验证**。
- `embhub-core::session` —— SPEC 3.4 最小实现：`start` / `stop`（幂等）/ `stats`（含 tx 计数，停后仍可读）/ `recv_batch` / `send`。
- `embhub-cli loopback --bytes N` —— 证明「写入 == 读回 == 落盘 == 回放」四方一致且逐字节相同。
- `embhub-cli send --port <p> --data <hex:…|text:…> [--append none|cr|lf|crlf] [--repeat N]` —— 参数非法一律先报错再动手（拒绝静默发送空数据）。

  ```
  .\scripts\cargo.cmd run --quiet -p embhub-cli -- loopback --bytes 1000000
  → written == read_back == logged == replay == reference == 1000000   逐字节一致

  .\scripts\cargo.cmd run --quiet -p embhub-cli -- send --port COM13 --data hex:AA5501FF --append none
  ```

> ⚠️ **真机发送尚未验证**：实现者正确拒绝向本机未知串口设备（COM3–COM12 多为蓝牙虚拟口）写入——那可能有真实副作用。真机发送请在 GUI 里用你的板子验证。

**Phase 1 增量 C1：流式管线与背压（已验收）**

- `embhub-core::pipeline` —— 常驻读取线程：**到达点时间戳原样搬运** → **先 append 落盘**（失败即致命并记账）→ 显示旁路按「≥8 KiB 或 ≥30 ms 先到先发」攒批，`try_send` 投入**有界**的 `sync_channel`，满即丢批次并计 `batches_dropped` / `display_bytes_dropped`。
- `stop()` 幂等（join + seal），`stats()` 停后仍可读；`wait_until_finished(timeout)`（Condvar）区分「等自然结束」与「现在就停」。
- `embhub-cli stream --bytes N [--chunk K] [--consume fast|slow|stalled] [--display-capacity C]` —— **退出码只看落盘零丢失**，显示丢弃不影响退出码。

  ```
  # 显示队列容量设 0（显示 100% 丢弃）——落盘依然零丢失
  .\scripts\cargo.cmd run --quiet -p embhub-cli -- stream --bytes 67108864 --consume stalled --display-capacity 0
  → batches_emitted 0 / display_bytes_dropped 67108864
    logged == replay == source == 67108864        exit 0
  ```

**Phase 1 增量 B2：指纹写进会话头（日志格式 v2，已验收）**

- `LogStore` 会话头升到 **v2**：`magic[8] | version | start_wall_ms | fp_len | fp_json | cfg_len | cfg`。指纹用**长度前缀的 JSON 段**存储，取代 v1 的 32 字节固定占位——后者根本装不下 `commit`(40) + `bin_hash`(64)。
- `FingerprintSection { collected, not_collected }`：不变量「**恰好一个有值**」在写入与读取两端都校验，**未采集必须带原因**。
- **v1 被明确拒绝**并提示「已废弃，需 v2」，绝不按 v2 误解析；未知的未来版本单独分支处理。
- `embhub-cli selftest --repo <dir> [--bin <file>]`：采集 → 写进 v2 头 → 落盘 seal → **从磁盘读回** → 逐字段比对，不一致非零退出。

  ```
  .\scripts\cargo.cmd run --quiet -p embhub-cli -- selftest --bytes 1000000 --repo . --bin docs\SPEC.md
  → 版本=2；✓ 指纹逐字段比对：会话头读回 == 采集值（8/8 字段）
  ```

**Phase 1 增量 A：零丢失闭环（已验收）**

- `embhub-core::source` —— `ByteSource` 接口（SPEC 3.1）+ `SerialSource` 与 `SyntheticSource` 两个实现；`SourceError` 区分「暂时无数据」与「致命断开」；块自带单调到达时间。
- `embhub-core::store` —— `LogStore`（SPEC 3.3）：会话头（`EMBHLOG\0` + 版本 + 时间 + 来源配置 + **指纹段**；A 阶段为 v1 的 32 字节占位，B2 起为 v2）、分片 append-only、每条记录带 CRC32、`seal`、`open_replay` 精确回放。
- `embhub-core::crc` —— 自实现的 CRC32（**零新增第三方依赖**）。
- `embhub-cli selftest [--bytes N] [--seed S]` —— 不依赖硬件就能证明「注入 == 落盘 == 回放读回」：

  ```
  .\scripts\cargo.cmd run --quiet -p embhub-cli -- selftest --bytes 67108864
  → 注入 67108864 == 落盘 67108864 == 回放读回 67108864，逐字节比对一致
  ```

- 测试基线（增量 A 交付时）：core **24 passed** + cli **8 passed**；`clippy -D warnings` 与 `fmt --check` 干净。
- **全项目当前基线（含 B1）：core 35 passed + cli 10 passed。**

### 规划中（见 PLAN）

| 阶段 | 内容 |
| --- | --- |
| Phase 1 ★ | 串口最小闭环 + **固件指纹写进日志头** + 分片落盘 + 回放 |
| Phase 2 | `Session` 模型 + `.embproj` 工程文件 + 多设备会话矩阵 |
| Phase 3 | uPlot 流式波形 + 内建解析器（行/HEX/定长帧/校验和） |
| Phase 4 | SSH/SFTP（russh），先命令执行 + 文件传输，后补交互式终端（xterm.js） |
| Phase 5 ★ | Git 溯源：**按固件版本过滤日志 + 一键跳到对应 commit** |
| Phase 6 | 解析扩展与回放复用（同一逻辑实时/回放结果逐字节一致） |
| Phase 7 | 烧录与调试集成（编排 `probe-rs` / `openocd` / `esptool` 子进程，不写驱动） |
| Phase 8 | `tauri build` 打包 + 绿色便携 |

### 三条硬约束（不要违反）

1. **`embhub-core` 不得依赖 tauri**——它是可独立测试、可 CLI 化的核心层。
2. **数据通道必须批量 + 有背压**：显示可以丢帧，**落盘一个字节都不能丢**。
3. **固件指纹从第一天写进日志头**：事后补不回来（现场日志往往只剩一个文件）。

---

## 文档

| 文件 | 作用 |
| --- | --- |
| [`docs/ACCEPTANCE.md`](docs/ACCEPTANCE.md) | **验收清单**：15 条自动项 + 3 条人工项的原始命令、期望输出、通过标准，以及"什么情况下应推翻我的结论" |
| [`docs/SPEC.md`](docs/SPEC.md) | **应该成为什么**：设计原则、五个模块契约、边界、可执行验收标准 |
| [`docs/PLAN.md`](docs/PLAN.md) | 实施计划 v0.3：范围、差异化、架构、八阶段、风险清单 |
| [`docs/DECISION-2026-10-14-stack-tauri-v2.md`](docs/DECISION-2026-10-14-stack-tauri-v2.md) | 技术栈决策（为什么是 Tauri v2）与 spike 判据 |
| [`docs/research/2026-10-14-embedded-tooling-landscape.md`](docs/research/2026-10-14-embedded-tooling-landscape.md) | 同类工具与技术库调研（一手来源，含未核实项清单） |
| [`LOG.md`](LOG.md) | 施工日志：每轮做了什么、决策原因、踩坑、未验证项 |

> 文档口径：**事实与推断严格分开**；未验证的一律写「未核实」。调研报告中所有「某工具没做到 X」均为采集范围内的「未检到」，不是穷尽性断言。
