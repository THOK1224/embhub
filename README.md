# embhub

面向嵌入式开发的桌面集成工具：把**串口调试**、**SSH/SFTP**、**Git** 放进同一条工作流，
并把每份日志与**当时运行的固件指纹**（commit + dirty + bin hash）绑定起来。

> **当前状态：Phase 0（spike），差最后一步。**
> ✅ 已通过：Rust 工具链、MSVC 链接、`serialport` 编译、10 个真实串口枚举、`cargo test` 全绿（2 passed）、**Tauri v2 编译通过**、WebView2 就位。
> ⬜ **未通过：窗口能否打开**——沙箱内启动时 WebView2 报 `HRESULT(0x8000FFFF) 灾难性故障`（判断为沙箱限制，非配置问题）；插拔刷新与收数据因此尚未验证。
> **下一步：在普通终端里 `cargo run -p embhub-app`**，验收两件事（见 [`docs/PLAN.md`](docs/PLAN.md) 第 9 节与 [`LOG.md`](LOG.md) 第 3 轮）。

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
.\scripts\env.ps1
# 若被执行策略拦：pwsh -ExecutionPolicy Bypass -File .\scripts\env.ps1
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

### 已实现（spike 级，会重构）

- `embhub-core::list_ports()` —— 跨平台串口枚举（端口名、类型、USB VID/PID/序列号/产品名）。
- `embhub-core::read_port()` —— 打开端口、按波特率读取、返回**字节数**与 HEX 预览。
- `embhub-cli` —— `list` / `read` 两个子命令。
- `embhub-app` —— Tauri 窗口：刷新端口、选择端口与波特率、打开读取并显示结果。

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
| [`docs/SPEC.md`](docs/SPEC.md) | **应该成为什么**：设计原则、五个模块契约、边界、可执行验收标准 |
| [`docs/PLAN.md`](docs/PLAN.md) | 实施计划 v0.3：范围、差异化、架构、八阶段、风险清单 |
| [`docs/DECISION-2026-10-14-stack-tauri-v2.md`](docs/DECISION-2026-10-14-stack-tauri-v2.md) | 技术栈决策（为什么是 Tauri v2）与 spike 判据 |
| [`docs/research/2026-10-14-embedded-tooling-landscape.md`](docs/research/2026-10-14-embedded-tooling-landscape.md) | 同类工具与技术库调研（一手来源，含未核实项清单） |
| [`LOG.md`](LOG.md) | 施工日志：每轮做了什么、决策原因、踩坑、未验证项 |

> 文档口径：**事实与推断严格分开**；未验证的一律写「未核实」。调研报告中所有「某工具没做到 X」均为采集范围内的「未检到」，不是穷尽性断言。
