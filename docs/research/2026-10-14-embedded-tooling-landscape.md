# 嵌入式开发桌面一体化工具：GitHub 同类工具与技术库调研

> 调研对象：一个纯软件形态的桌面端一体化工作台（串口助手 + SSH/SFTP + Git），面向嵌入式开发（MCU/RTOS 调试、日志抓取、烧录、固件管理）。
> 调研日期记录：2026-10-14（按任务约定）。所有量化数据（star / fork / license / 最近提交）通过 GitHub REST API 或官方 registry API 于该日抓取。

---

## 0. 调研方法与可信度说明

**只用一手来源**，分四类：

| 来源类型 | 具体地址 | 用途 |
| --- | --- | --- |
| GitHub Search API | `https://api.github.com/search/repositories` | 批量取 `stargazers_count` / `license.spdx_id` / `language` / `pushed_at` / `archived` |
| GitHub REST API | `https://api.github.com/repos/OWNER/REPO` | 单仓库详情（含 `open_issues_count`、`created_at`） |
| raw 源码文件 | `https://raw.githubusercontent.com/OWNER/REPO/BRANCH/<manifest>` | 判定技术栈：`package.json` / `Cargo.toml` / `pyproject.toml` / `go.mod` / `pom.xml` / `setup.py` / `CMakeLists.txt` |
| 官方 registry / 官网 | `registry.npmjs.org`、`pypi.org`、厂商官网（mobatek / vofa.plus / st.com / segger.com）、官方文档站（tauri.app、electronjs.org） | 包体积、许可、闭源确认、跨平台支持 |

**口径约定（重要）**：

- `license` 列为 GitHub API 返回的 SPDX 标识。`NOASSERTION` 表示 GitHub 无法自动识别，**不等于无许可证**，需要人工看 LICENSE 文件；`None`/`—` 表示仓库未声明许可证（法律风险项，见 D 节）。
- `最近提交` = API 的 `pushed_at`（含任意分支的 push，不完全等于默认分支的 commit 时间）。
- `技术栈` 来自上面第 3 类来源的实际文件内容；判定不了的一律写「未核实」，不猜。
- 「口碑」在本报告中量化为可核查的代理指标：star 量级 + `pushed_at` 活跃度 + `archived` 状态 + license 明确性。**本报告没有做用户满意度抽样，主观口碑一律标注为判断而非事实。**
- **「某工具没做到 X」是本轮采集范围内的「未检到」，不是穷尽性断言。** C 节的每条「没做到」都基于对应仓库的 README/manifest/官网页面，凡属推断的措辞均已显式标注。

---

## 1. 结论速览

1. **「串口 + SSH + Git」三合一，目前没有成熟开源对标品。** 最强的一体化开源产品 Tabby（多协议终端：内置 `tabby-ssh`、`tabby-serial` 等插件，见 <https://github.com/Eugeny/tabby>）与 Serial Studio（多协议遥测仪表盘：UART/BLE/MQTT/Modbus/CAN，见 <https://github.com/Serial-Studio/Serial-Studio>）各占一半：**Tabby 有串口 + SSH 但没有 Git（`tabby-git` 实测 404）**，Serial Studio 有串口可视化但没有 SSH/Git，二者的交集为空。
2. **嵌入式侧的「烧录 + RTT 日志流」已经被 probe-rs 用「可嵌入的库」形态解决**（<https://github.com/probe-rs/probe-rs> README 明确列出 DAPLink/STLink/JLink/FTDI/ESP32 USB-JTAG/WLink/Blackmagic probe，以及 `probe-rs run` 做 flash + RTT/defmt 日志流），这是自研工具最容易吃到的一块现成能力。
3. **技术栈选择上，三件事的「最主流」组合是：Rust（Tauri + serialport-rs/tokio-serial + russh + git2/gitoxide）或 TypeScript（Electron/Tauri + node-serialport + ssh2 + simple-git/isomorphic-git）。** 若目标是安装包小、内存低、原生模块（串口/PTY）集成顺手，Rust 路线的证据链最完整（见 3.7 节）。
4. **差异化的最大空位不在「有没有串口」，而在「关联」**：日志 ↔ 源码位置、固件版本 ↔ Git commit、多设备会话编排、协议解析脚本化。现有工具里没有一个把「抓到的一段串口日志」和「某个 Git 提交/固件 bin」建立可追溯关系。

---

## 2. 调研范围 A：同类 / 邻近工具

### 2.1 串口 / 串行调试与可视化（7 个 + 1 个闭源上位机）

| 工具 | 仓库 / 来源 | Star | License | 技术栈（依据） | 核心功能 | 最近提交 |
| --- | --- | --- | --- | --- | --- | --- |
| Serial Studio | [Serial-Studio/Serial-Studio](https://github.com/Serial-Studio/Serial-Studio) | 7,237 | `NOASSERTION`；`CMakeLists.txt` 头部自述 **GPLv3 或商业双许可** | C++ / CMake / Qt（`CMakeLists.txt`）；自带 Pro 模块的双许可声明 | 跨平台遥测仪表盘：UART、BLE、MQTT、Modbus、CAN Bus 等 | 2026-10-13 |
| serialplot | [hyOzd/serialplot](https://github.com/hyOzd/serialplot) | 1,111 | GPL-3.0（`CMakeLists.txt` 头部 + API） | C++ / CMake / Qt | 实时串口数据绘图（软件示波器形态） | 2025-05-28 |
| SerialTest | [wh201906/SerialTest](https://github.com/wh201906/SerialTest) | 1,667 | GPL-3.0（源码）；README 说明 `demo/`、翻译、文档为 MIT，`qcustomplot`/`qdarkstyle` 沿用各自许可 | C++ / Qt5 / qmake / QCustomPlot（README 构建步骤要求 `qtbase5-dev`、`libqt5serialport5-dev`） | 数据收发 + 实时绘图 + 快捷指令面板 + 文件收发；串口 / 蓝牙 SPP 客户端与服务端 / BLE / TCP 客户端与服务端 / UDP | 2024-06-17 |
| NinjaTerm | [gbmhunter/NinjaTerm](https://github.com/gbmhunter/NinjaTerm) | 120 | GPL-3.0 | TypeScript（API `language`）+ Electron（应用形态，未核实具体依赖） | 串口终端 | 2026-09-07 |
| QtSerialMonitor | [mich-w/QtSerialMonitor](https://github.com/mich-w/QtSerialMonitor) | 270 | GPL-3.0 | C++ / Qt（API `language`，未见 manifest） | Qt 串口监视 + 数据绘图 | 2022-03-22 |
| open-serial-port-monitor | [gthvidsten/open-serial-port-monitor](https://github.com/gthvidsten/open-serial-port-monitor) | 186 | GPL-3.0 | C#（API `language`，未见 `.csproj`） | 监听已打开串口上的流量（抓包式监视） | 2020-01-19（**已基本停更**） |
| komport-qt6 | [ironcold/komport-qt6](https://github.com/ironcold/komport-qt6) | 1 | GPL-2.0 | C++ / Qt（API `language`） | 轻量串口通信 + VT100/VT102 终端仿真（**长尾小项目，仅作参考**） | 2026-10-14 |
| VOFA+ | 官网 <https://www.vofa.plus/>（上位机本体**闭源商业**，需购买密钥；插件框架部分开源为 [je00/Vodka](https://github.com/je00/Vodka)，472 star / MIT / 最近提交 2021-08-02） | 不适用（非 GitHub 仓库） | 本体闭源商业；Vodka 框架 MIT | 未核实（闭源） | 插件驱动的上位机：协议解析、2D/3D 可视化、图形化参数下发 | 官网版权页显示 2018–2026 |

**可借鉴点**

- Serial Studio：**协议/数据源可插拔 + 一个项目文件描述整条数据流水线**，把「驱动 → 解析 → 仪表盘」做成配置而非硬编码；这正好是「一体化工作台」里串口模块应有的抽象层级。
- SerialTest：**面板化 + 布局可保存**、**连接参数按端口名/设备 ID 存库复用**、**收发与 UI 解耦**（README 明确「UI update is separated from Data reception」，关掉 realtime 可提速）、**1:1 内存占用**、**不写注册表、单配置文件**（便携）。这几条几乎可以直接抄成产品需求。
- serialplot / QtSerialMonitor：实时绘图的工程细节（多通道、X 轴模式、滚动/保持）。
- open-serial-port-monitor：**「被动监听已被别的程序占用的串口」**这个功能点很独特（嵌入式现场常见：IDE 已经占着口，还想看日志），值得列入需求。

**局限**

- 除 Tabby/WezTerm 外，**没有一个串口工具自带 SSH 或 Git 能力**。
- SerialTest 最近提交停在 2024-06，Qt5 技术栈偏旧；NinjaTerm 只有 120 star，功能单薄。
- Serial Studio 的双许可（GPLv3 + 商业）意味着**若要复用其 Pro 代码必须付费**；仅参考其架构思路则无碍。
- VOFA+ 闭源，无法复用代码，只能作为交互与协议设计的参照。

### 2.2 终端 / SSH 客户端（含多协议集成，7 个）

| 工具 | 仓库 / 来源 | Star | License | 技术栈（依据） | 核心功能 | 最近提交 |
| --- | --- | --- | --- | --- | --- | --- |
| Tabby | [Eugeny/tabby](https://github.com/Eugeny/tabby) | 74,966 | MIT | Angular 15 + Electron 43 + webpack 5（`package.json` 依赖实测） | 多协议终端：本地 shell、SSH、Telnet、Serial、SFTP 等；插件化 + 主题/配色 | 2026-10-12 |
| WindTerm | [kingToolbox/WindTerm](https://github.com/kingToolbox/WindTerm) | 32,433 | **未声明（API `license = None`）** | C（API `language`）；仓库无 `package.json`/`Cargo.toml`（已实测） | 终端 + SSH/SFTP/Telnet/Serial + 会话管理 + 远程监控条 | 2025-03-11 |
| Electerm | [electerm/electerm](https://github.com/electerm/electerm) | 15,354 | MIT | Node.js + Electron（API `language = JavaScript`） | SSH/SFTP/Telnet/串口 + 本地终端 + 书签同步 | 2026-10-14 |
| WezTerm | [wezterm/wezterm](https://github.com/wezterm/wezterm) | 29,228 | 仓库根 `LICENSE.md` 为 **MIT**（API 返回 `NOASSERTION`，因仓库内含多许可文件） | Rust（`Cargo.toml`），实测依赖含 `git2 0.20`、`libssh-rs 0.3.8`、`clap 4` | GPU 加速跨平台终端 + 多路复用；**内置 SSH 客户端、内置串口支持、Lua 配置** | 2026-10-12 |
| Alacritty | [alacritty/alacritty](https://github.com/alacritty/alacritty) | 65,960 | Apache-2.0（仓库 LICENSE） | Rust，`edition 2024` / MSRV `1.85.0`（`Cargo.toml`） | 纯 GPU 终端仿真器，故意不做 tab/分屏，交给 tmux 类复用器 | 2026-10-05 |
| Warp | [warpdotdev/Warp](https://github.com/warpdotdev/Warp) | 65,452 | **AGPL-3.0-only**（`Cargo.toml` 实测声明） | Rust（`Cargo.toml`）；客户端 UI 是否开源未核实 | 现代终端（块状输出、AI 命令辅助等；功能清单未逐一核实） | 2026-10-14 |
| MobaXterm | 官网 <https://mobaxterm.mobatek.net/>（**闭源商业**，Home Edition 免费、Professional 收费） | 不适用 | 闭源商业 | 未核实（闭源） | 单 exe 一体化远程工具箱：SSH/X11/RDP/VNC/FTP/Mosh，标签式终端、内嵌 X server、连上 SSH 自动弹出图形化 SFTP 浏览器 | 官网版权页 2008–2026 |
| Hyper | [vercel/hyper](https://github.com/vercel/hyper) | 44,735 | MIT | Node.js + Electron（API `language = JavaScript`） | 基于 Web 技术的终端（**维护活跃度弱，仅作历史参照**） | 2026-08-21 |

> 注：Tabby / WindTerm / Electerm / WezTerm / Alacritty / Warp / Hyper / MobaXterm 中，除 MobaXterm（闭源，无 GitHub 仓库）外，star 与 `pushed_at` 均于 2026-10-14 取自 `https://api.github.com/repos/OWNER/REPO` 与 Search API。

**可借鉴点**

- Tabby 与 MobaXterm 是「一体化」的两个成熟范式：Tabby = **插件化 + 多协议 + 现代 UI**；MobaXterm = **单文件绿色 + 会话驱动 + 连接即带出附属面板（SSH → SFTP 浏览器）**。「SSH 连接自动带出文件传输面板」这个交互对嵌入式场景（连上 Linux 板子就要传固件）几乎是刚需。
- WindTerm：会话管理 + 远程监控条（CPU/内存/网络），把「连接」和「观察」放在一个界面。
- WezTerm：**用 Lua 做配置/脚本化**、自带 SSH；**串口支持在源码中得到核实**——`pty/src/serial.rs` 文件头注释为「This module implements a serial port based tty」（实测 `https://raw.githubusercontent.com/wezterm/wezterm/main/pty/src/serial.rs`）。证明「终端框架内建串口」在工程上可行。
- Alacritty：反面参考——**刻意不做多面板**，把复杂度交给外部工具；自研工作台不能走这条路。

**局限**

- Tabby **确实内建串口**：仓库内存有内置插件包 `tabby-serial`（`package.json` 中 `"keywords": ["tabby-builtin-plugin"]`，实测于 `https://raw.githubusercontent.com/Eugeny/tabby/master/tabby-serial/package.json`），SSH 同理（`tabby-ssh`）。但它**没有 Git 插件**：`tabby-git/package.json` 返回 404（实测），即版本管理能力缺失（这一条是核实结论，不再是推测）。
- WindTerm **未声明许可证**：32k star 却拿不到许可授权，是典型的「不能抄代码、只能借鉴产品」案例。
- WezTerm/Alacritty 是终端，不是嵌入式工具：没有烧录、没有 RTT、没有固件/Git 概念。
- MobaXterm 闭源且 Windows-only（官网定位 Windows 桌面），无法作为跨平台基座。

### 2.3 嵌入式 IDE / 集成工作台（8 个）

| 工具 | 仓库 / 来源 | Star | License | 技术栈（依据） | 核心功能 | 最近提交 |
| --- | --- | --- | --- | --- | --- | --- |
| PlatformIO Core | [platformio/platformio-core](https://github.com/platformio/platformio-core) | 9,527 | Apache-2.0（`setup.py` 头部实测） | Python（`setup.py`） | 跨平台嵌入式构建/上传/库管理/单元测试 CLI 引擎 | 2026-09-23 |
| PlatformIO VSCode 扩展 | [platformio/platformio-vscode-ide](https://github.com/platformio/platformio-vscode-ide) | 1,462 | Apache-2.0 | TypeScript（VSCode 扩展，`package.json`） | 在 VSCode 里提供 PlatformIO 的构建/上传/串口监视器/调试集成 | 2025-01-11 |
| ESP-IDF | [espressif/esp-idf](https://github.com/espressif/esp-idf) | 19,241 | Apache-2.0（仓库 LICENSE） | C + CMake（`CMakeLists.txt`：`project(esp-idf C CXX ASM)`） | 乐鑫官方 IoT 开发框架（构建系统 + 组件管理） | 2026-10-12 |
| ESP-IDF VSCode 扩展 | [espressif/vscode-esp-idf-extension](https://github.com/espressif/vscode-esp-idf-extension) | 1,472 | Apache-2.0 | TypeScript（VSCode 扩展，`package.json`） | 一站式：芯片/端口选择、配置、构建、烧写、**串口监视**、GDB 调试 | 2026-10-14 |
| Arduino IDE 2.x | [arduino/arduino-ide](https://github.com/arduino/arduino-ide) | 3,299 | AGPL-3.0 | Electron 应用；`package.json` 实测 `name = arduino-ide`、`version = 2.3.11`、`engines.node = >=18.17.0 <21`，eslint-plugin-react 表明 React UI | 图形 IDE：编辑器 + 板卡/库管理 + 串口绘图仪 + 串口监视 + 上传 | 2026-10-14 |
| arduino-cli | [arduino/arduino-cli](https://github.com/arduino/arduino-cli) | 5,059 | GPL-3.0 | Go（API `language`；仓库根 `package.json` 仅为发布脚本，已实测） | 命令行编译/上传/板卡与库管理引擎（被 IDE 2.x 复用） | 2026-10-12 |
| Eclipse Embedded CDT | [eclipse-embed-cdt/eclipse-plugins](https://github.com/eclipse-embed-cdt/eclipse-plugins) | 559 | EPL-2.0（`pom.xml` 头部实测） | Java / Maven / Eclipse 插件（`pom.xml`） | Eclipse 上的 ARM/RISC-V 嵌入式工具链集成（GDB 调试、模板工程） | 2026-09-09 |
| Eclipse CDT | [eclipse-cdt/cdt](https://github.com/eclipse-cdt/cdt) | 375 | EPL-2.0（`pom.xml` 头部实测） | Java / Maven（`pom.xml`） | 通用 C/C++ IDE 框架（Embedded CDT 的底座） | 2026-10-13 |
| probe-rs | [probe-rs/probe-rs](https://github.com/probe-rs/probe-rs) | 2,974 | MIT OR Apache-2.0（crates 生态惯例，仓库 LICENSE 未逐字核实） | Rust（`Cargo.toml`；根 `package.json` 为工具链脚本，已实测） | 现代嵌入式调试工具集：库 + CLI + VSCode 扩展 + GDB server；支持 DAPLink/STLink/JLink/FTDI/ESP32 USB-JTAG/WLink/Blackmagic；ARM/RISC-V/Xtensa，SWD/JTAG；**`probe-rs run` = flash + RTT/defmt 日志流** | 2026-10-14 |
| OpenOCD | [openocd-org/openocd](https://github.com/openocd-org/openocd) | 2,352 | `NOASSERTION`（实际为 GPL-2.0-or-later，**未逐字核实**） | C（API `language`） | 通用片上调试器 / 烧录器，配合 GDB 使用 | 2026-10-13 |
| STM32CubeIDE | 官网 <https://www.st.com/en/development-tools/stm32cubeide.html>（**闭源免费**，ST 官方开发工具） | 不适用 | 闭源 | 未核实（闭源，基于 Eclipse/CDT 生态） | STM32 一站式集成开发环境（CubeMX 配置 + 编译 + 调试） | 不适用 |
| SEGGER Ozone | 官网 <https://www.segger.com/products/development-tools/ozone-j-link-debugger/>（**闭源商业**，J-Link PLUS/Ultra/Pro、J-Trace PRO 附带免费许可） | 不适用 | 商业许可（perpetual） | 未核实（闭源） | 图形化调试器 + 性能分析：源码级 C/C++/Rust 调试、RTOS 插件、RTT、指令 trace、代码剖析与覆盖率、脚本自动化 | 官网 news 2026-07 |
| RT-Thread Studio | 未找到可信的官方开源仓库（GitHub 检索「rtthread-studio」无官方仓库命中；RT-Thread 主仓为 [RT-Thread/rt-thread](https://github.com/RT-Thread/rt-thread)） | 未核实 | 未核实 | 未核实 | RT-Thread 官方 IDE（**开源部分未核实**） | 未核实 |

**可借鉴点**

- **ESP-IDF VSCode 扩展**是「一体化」最贴近本项目的成熟样本：单一面板里完成 目标芯片/串口选择 → 配置 → 构建 → 烧写 → 串口监视 → 调试。
- **Arduino IDE 2.x 的架构**值得直接学：**Electron 外壳（IDE）+ 独立 CLI 引擎（arduino-cli 负责编译上传）+ 内建串口监视/绘图**。把「重活」放进可单独调用的 CLI/库，UI 只做编排，这对自研工具的可测试性与脚本化极关键。
- **probe-rs**：把「烧录 + RTT 日志」做成了可嵌入的 Rust 库和 `probe-rs run` 子命令，是「日志抓取」这一模块最省力的现成方案。
- **Ozone**：脚本化 + 事件回调（下载/复位/暂停/断点前后触发动作）→ 自动化测试。这是「烧录流程可编排」的产品级参考。

**局限**

- PlatformIO / ESP-IDF 扩展 / Eclipse Embedded CDT 都是**寄生在别人 IDE 里的插件**，不是独立工作台；它们的串口监视与 Git 依赖宿主 IDE（VSCode 的 Git、串口监视器分别由 VSCode 与扩展提供）。
- Arduino IDE 2.x 的 Git 能力没有（未核实到内建版本管理），串口绘图仪侧重教学场景。
- STM32CubeIDE / Ozone / RT-Thread Studio 闭源或开源部分不明确，不能复用代码。
- OpenOCD + GDB 组合对普通嵌入式用户门槛高（配置文件、命令脚本），这正是 probe-rs 想取代的对象。

### 2.4 「多面板 + 内嵌终端/版本控制」可借鉴工作台（3 个，仅取相关者）

| 工具 | 仓库 | Star | License | 技术栈（依据） | 与本项目的相关性 | 最近提交 |
| --- | --- | --- | --- | --- | --- | --- |
| JupyterLab | [jupyterlab/jupyterlab](https://github.com/jupyterlab/jupyterlab) | 15,361 | BSD-3-Clause | TypeScript + Python（`package.json` + Jupyter 生态） | **多面板 dock 布局的事实标准**（可拖拽、可拆分、可保存工作区）；其布局系统是最值得研究的 UX 参照 | 2026-10-14 |
| Lapce | [lapce/lapce](https://github.com/lapce/lapce) | 38,921 | Apache-2.0 | Rust（`Cargo.toml` 实测含 `git2 0.21` + `vendored-libgit2`/`vendored-openssl`） | 编辑器内嵌 Git 的原生实现样本；**注意它选了 libgit2 而非 gitoxide** | 2026-10-09 |
| Zed | [zed-industries/zed](https://github.com/zed-industries/zed) | 91,706 | 见 D 节（GPL-3.0/AGPL 等混合，未逐一核实） | Rust（`Cargo.toml`） | GPU 渲染的高性能多面板编辑器；可借鉴其「终端面板 / 协作面板」组织方式 | 2026-10-14 |

---

## 3. 调研范围 B：关键能力的技术库候选

> 表中的 `语言`/`license` 均来自 API 或仓库 manifest 实测；`活跃度` 依据 `pushed_at`（采集日 2026-10-14）。

### 3.1 串口

| 库 | 仓库 | Star | License | 语言 | 最近提交 | 一句话评价 |
| --- | --- | --- | --- | --- | --- | --- |
| serialport-rs | [serialport/serialport-rs](https://github.com/serialport/serialport-rs) | 754 | **MPL-2.0**（`Cargo.toml` 实测，版本 4.10.2-alpha0） | Rust | 2026-09-08 | Rust 生态最通用的跨平台串口库（阻塞式 I/O + 端口枚举）；README 明确 **「looking for maintainers, especially for Windows」**，是选型风险点 |
| tokio-serial | [berkowski/tokio-serial](https://github.com/berkowski/tokio-serial) | 267 | MIT | Rust | 2026-06-15 | serialport-rs 之上的异步封装，适合 Tauri/Tokio 后端做多设备并发读写 |
| node-serialport | [serialport/node-serialport](https://github.com/serialport/node-serialport) | 5,994 | MIT | TypeScript/JS（monorepo，核心 `@serialport/bindings-cpp`） | 2026-01-17 | Electron 路线的事实标准；`package.json` 实测 `engines.node >= 20.0.0`；**原生模块需 electron-rebuild** |
| pyserial | [pyserial/pyserial](https://github.com/pyserial/pyserial) | 3,579 | BSD-3-Clause（`pyproject.toml` 实测） | Python | 2026-05-19 | 最省事的串口库；自带 `miniterm` / `list_ports` 命令入口，适合原型与脚本化 |
| go.bug.st/serial | [bugst/go-serial](https://github.com/bugst/go-serial) | 876 | BSD-3-Clause | Go（`go.mod` module 名 `go.bug.st/serial`，Go 1.25） | 2026-07-15 | Go 生态事实标准（Wails 路线适用）；纯 Go + `golang.org/x/sys`，无 CGO |

### 3.2 SSH / SFTP

| 库 | 仓库 | Star | License | 语言 | 最近提交 | 一句话评价 |
| --- | --- | --- | --- | --- | --- | --- |
| russh | [Eugeny/russh](https://github.com/Eugeny/russh) | 1,901 | Apache-2.0 | Rust（`Cargo.toml` 依赖 Tokio） | 2026-10-05 | 纯 Rust 异步 SSH2 客户端/服务端，README/示例**直接覆盖 interactive PTY 与 SFTP client/server**，Tauri 路线首选 |
| ssh2-rs | [rust-lang/ssh2-rs](https://github.com/rust-lang/ssh2-rs) | 569 | Apache-2.0 | Rust（libssh2 绑定） | 2026-10-06 | libssh2 的成熟 Rust 绑定，功能全但需处理 C 依赖/交叉编译 |
| libssh2 | [libssh2/libssh2](https://github.com/libssh2/libssh2) | 1,593 | BSD-3-Clause | C | 2026-10-14 | C 生态通用底座，被大量工具间接使用；自行绑定成本高 |
| ssh2 (Node) | [mscdex/ssh2](https://github.com/mscdex/ssh2) | 5,824 | MIT | JS（`package.json` 实测 `version 1.17.0`，纯 JS 无原生依赖） | 2026-08-20 | Electron 路线最省事：无需编译原生模块，PTY/SFTP 都有 |
| golang.org/x/crypto/ssh | [golang/crypto](https://github.com/golang/crypto) | 3,351 | BSD-3-Clause | Go（`go.mod: module golang.org/x/crypto`） | 2026-10-11 | Go 官方 SSH 实现；Wails 路线默认选择 |
| Paramiko | [paramiko/paramiko](https://github.com/paramiko/paramiko) | 9,879 | **LGPL-2.1**（`pyproject.toml` 实测，版本 5.0.0） | Python | 2026-10-10 | 经典 Python SSH 库；**LGPL 对闭源分发有约束**，商业桌面产品需评估 |

### 3.3 内嵌终端 / PTY

| 库 | 仓库 | Star | License | 语言 | 最近提交 | 一句话评价 |
| --- | --- | --- | --- | --- | --- | --- |
| xterm.js | [xtermjs/xterm.js](https://github.com/xtermjs/xterm.js) | 21,269 | MIT | TypeScript（npm `@xterm/xterm` 6.0.0，unpacked 5.6 MB，161 文件，实测） | 2026-10-13 | Web/Electron/Tauri 前端终端渲染器的实际标准 |
| node-pty | [microsoft/node-pty](https://github.com/microsoft/node-pty) | 2,046 | MIT | C++/TypeScript（npm 1.1.0，**unpacked 61.4 MB**，实测） | 2026-10-03 | Node 侧 PTY 绑定（`forkpty`）；README 明确 **Windows 需 conpty，且要求 Windows 10 1809 (build 18309)+，已移除 winpty 支持** |
| ConPTY（实现） | [microsoft/terminal](https://github.com/microsoft/terminal) | 105,162 | MIT | C++ | 2026-10-13 | Windows 伪控制台 API 的官方实现与事实规范来源（**是终端应用本身，不是可直接依赖的库**） |
| ttyd | [tsl0922/ttyd](https://github.com/tsl0922/ttyd) | 12,498 | MIT | C | 2026-08-12 | 把终端共享到 Web（C + libwebsockets）；**未核实其内部是否使用 wslay/ConPTY**；可作「终端即服务」的架构参考 |

> 补充：WezTerm 自研的 **portable-pty** crate（`wezterm/wezterm` 的 `pty` 子 crate）是 Rust 路线里跨平台 PTY 的现成方案；本报告中其许可证与版本**未单独核实**，使用前请查 `https://github.com/wezterm/wezterm/tree/main/pty` 的 crate 元数据。

### 3.4 Git

| 库 | 仓库 | Star | License | 语言 | 最近提交 | 一句话评价 |
| --- | --- | --- | --- | --- | --- | --- |
| libgit2 | [libgit2/libgit2](https://github.com/libgit2/libgit2) | 10,624 | GPL-2.0 **with linking exception**（`libgit2` 惯例；仓库根 `package.json` 为其绑定包，**许可未逐字核实**） | C | 2026-10-14 | 最成熟的 Git 内核；带链接例外，可被闭源产品链接 |
| git2-rs | [rust-lang/git2-rs](https://github.com/rust-lang/git2-rs) | 2,114 | MIT OR Apache-2.0（`Cargo.toml` 实测，git2 0.21.0，MSRV 1.87） | Rust | 2026-10-05 | libgit2 的 Rust 绑定，**Lapce 的 `Cargo.toml` 明确依赖 `git2 0.21`（实测）**；支持 `vendored-libgit2` 免系统依赖 |
| gitoxide (gix) | [GitoxideLabs/gitoxide](https://github.com/GitoxideLabs/gitoxide) | 12,012 | MIT OR Apache-2.0（`Cargo.toml` 实测，0.60.0，edition 2024，MSRV 1.88） | Rust | 2026-10-14 | 纯 Rust 重写，无 C 依赖、交叉编译友好；API 面仍在演进，功能覆盖需自行确认 |
| simple-git | [steveukx/git-js](https://github.com/steveukx/git-js) | 3,857 | MIT | TypeScript（npm 名 `simple-git`，仓库 `package.json` 实测 `name = @simple-git/repo`） | 2026-09-26 | **包装 git 命令行**：行为与用户本地 git 完全一致，代价是必须要求用户装 git |
| isomorphic-git | 未在本轮采集 | 未核实 | 未核实（候选） | JS | 未核实 | 纯 JS 实现、无需系统 git 的候选方案；**本轮完全未核实，正式选型前必须自行验证** |
| GitPython | [gitpython-developers/GitPython](https://github.com/gitpython-developers/GitPython) | 5,185 | BSD-3-Clause | Python（`requirements.txt` 实测 `gitdb>=4.0.1`） | 2026-10-14 | 同样是包装 git 命令；Python 路线适用 |
| go-git | [go-git/go-git](https://github.com/go-git/go-git) | 7,798 | Apache-2.0（仓库 LICENSE） | Go（`go.mod: module github.com/go-git/go-git/v6`） | 2026-10-14 | 纯 Go Git 实现，Wails 路线可选 |

### 3.5 波形 / 图表

| 库 | 仓库 | Star | License | 语言 | 最近提交 | 一句话评价 |
| --- | --- | --- | --- | --- | --- | --- |
| uPlot | [leeoniya/uPlot](https://github.com/leeoniya/uPlot) | 10,557 | MIT | TypeScript/JS（npm 1.6.32，unpacked 0.5 MB，实测） | 2026-10-11 | **流式高速时序绘图**，体积小；串口实时波形的最优选之一 |
| ECharts | [apache/echarts](https://github.com/apache/echarts) | 67,508 | Apache-2.0 | TypeScript（npm 6.1.0，**unpacked 57.5 MB**，实测） | 2026-10-13 | 图表类型最全、生态最大，但为「通用图表」而非高吞吐流式波形，包体偏重 |
| egui / egui_plot | [emilk/egui](https://github.com/emilk/egui) | 31,422 | MIT OR Apache-2.0（`Cargo.toml` 实测，0.36.2，MSRV 1.95） | Rust | 2026-10-14 | 纯 Rust 即时模式 GUI，配 `egui_plot` 可画波形；适合「单语言、无 WebView」方案 |
| Plotly.py | [plotly/plotly.py](https://github.com/plotly/plotly.py) | 18,840 | MIT（`pyproject.toml` 实测，7.1.0） | Python | 2026-10-09 | 交互式科学图表标准件；桌面内嵌需配 WebView/Qt WebEngine，Python 路线适用 |

> 另可参考：`mich-w/QtSerialMonitor`、`wh201906/SerialTest`、`hyOzd/serialplot` 均使用 **QCustomPlot** 做实时绘图——若走 Qt 路线，这是 Embedded/工控领域的成熟选择（许可未在本轮核实，QCustomPlot 为 GPL/商业双许可，使用前必查）。

### 3.6 桌面壳框架对比

| 框架 | 仓库 | Star | License | 技术栈 | 跨平台（官方口径） | 体积 / 内存（可核实部分） | 原生模块集成难度 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Tauri v2 | [tauri-apps/tauri](https://github.com/tauri-apps/tauri) | 111,887 | MIT 或 Apache-2.0（README「Licenses」节） | Rust 后端 + 系统 WebView（Windows: WebView2；macOS: WKWebView；Linux: WebKitGTK）+ 任意前端 | README：Windows 10+、macOS 10.15+、Linux（v2 需 webkit2gtk 4.1）、iOS 9+/Android 7+ | 官方未给数字，只称「tiny, blazingly fast binaries」；<https://v2.tauri.app/concept/size/> 提供 LTO/`opt-level="s"`/`strip`/`panic=abort` 与 2.4+ 的 `removeUnusedCommands` 瘦身手段 | **最低**：串口/SSH/PTY 直接写 Rust，经 command 暴露给前端；无需 Node 原生模块重编译 |
| Electron | [electron/electron](https://github.com/electron/electron) | 123,317 | MIT | Node.js + 捆绑 Chromium | 官方 README（`v44.7.2` 分支）为平台支持权威来源 | 未核实（**未使用未经证实的体积数字**）；npm `electron` 包 `unpackedSize` 仅 1.1 MB，只是安装器，不代表运行时体积 | **较高**：串口/PTY 等原生模块需对 Electron 的 ABI 重新编译（`electron-rebuild`）；好处是 node-serialport/ssh2/node-pty 生态成熟 |
| Wails | [wailsapp/wails](https://github.com/wailsapp/wails) | 36,548 | MIT | Go 后端 + 系统 WebView | 未核实 | 未核实 | 中：Go 侧接 `go.bug.st/serial`、`x/crypto/ssh` 很方便；WebView 与系统绑定带来的差异需实测 |
| Avalonia | [AvaloniaUI/Avalonia](https://github.com/AvaloniaUI/Avalonia) | 31,661 | MIT | .NET / C# XAML | 未核实（官网未逐字核实） | 未核实 | 中：.NET 串口 `System.IO.Ports`、SSH 有 SSH.NET；仓库根无 manifest（已实测），技术栈依据为项目性质而非文件证据 |
| PySide6 / Qt | PyPI [PySide6](https://pypi.org/project/PySide6/) | 不适用（PyPI） | **LGPL-3.0-only OR GPL-2.0-only OR GPL-3.0-only**（PyPI `info.license` 实测，6.12.0） | Python 绑定 Qt6 | PyPI 分类器标注 Windows/macOS/X11 | **可核实的官方体积**：Windows wheel `PySide6-Essentials 78.3 MB` + `PySide6-Addons 38.6 MB` + `shiboken6 1.2 MB`（PyPI，6.12.0）；→ 打包体积压力大 | 低：`pyserial`/`paramiko`/`GitPython` 直接调用；但 LGPL 动态链接合规需设计 |
| egui | [emilk/egui](https://github.com/emilk/egui) | 31,422 | MIT OR Apache-2.0（`Cargo.toml` 实测） | Rust 即时模式 GUI（可配 wgpu/glow） | 未核实 | 未核实（Rust 静态二进制，通常无需 WebView/运行时） | 最低：串口/SSH/Git 与 UI 同进程同语言 |
| iced | [iced-rs/iced](https://github.com/iced-rs/iced) | 31,758 | MIT（未逐字核实） | Rust 保留模式 GUI；`Cargo.toml` 实测含 `wgpu`、`tokio` feature | 未核实 | 未核实 | 低：同 egui，但生态与控件数量少于 egui |

**框架选型结论（判断，非事实）**：若「串口 + PTY + SSH + 烧录进程管理」是核心，**Tauri v2 + Rust 库链**的集成摩擦最小（原生模块无需跨 ABI 重编译、安装包体量最可控）；若团队只会 TypeScript 且接受更大体积，**Electron + node-serialport + ssh2 + node-pty** 是成熟度最高、坑最有文档可查的组合。

### 3.7 「串口 + SSH + Git」三件事的库推荐（综合结论）

| 语言路线 | 串口 | SSH/SFTP | Git | 终端渲染/PTY | 备注 |
| --- | --- | --- | --- | --- | --- |
| **Rust + Tauri v2（推荐）** | `serialport-rs` + `tokio-serial` | `russh`（或 `ssh2-rs`/libssh2） | `git2-rs`（成熟，Lapce/WezTerm 在用）或 `gitoxide`（纯 Rust，交叉编译友好） | `xterm.js` + WezTerm `portable-pty`（或 ConPTY 直连） | 三件事都是同语言原生库，无 ABI 重编译问题；风险：serialport-rs 官方在招维护者 |
| **TypeScript + Electron** | `node-serialport`（`engines.node >= 20`） | `ssh2`（纯 JS，无原生依赖） | `simple-git`（包装 CLI）或 `isomorphic-git`（未核实） | `@xterm/xterm` + `node-pty`（Windows 需 conpty / Win10 1809+） | 生态最成熟、文档最多；代价：原生模块重编译、打包体积、内存开销 |
| **Python + PySide6/Qt** | `pyserial`（BSD-3） | `paramiko`（**LGPL-2.1**，注意合规） | `GitPython`（BSD-3，包装 CLI） | Qt 自带终端控件（或 QTermWidget，未核实） | 开发最快，但 PySide6 Windows wheel 实测约 118 MB（三包合计），分发沉重 |
| **Go + Wails** | `go.bug.st/serial` | `golang.org/x/crypto/ssh` | `go-git` | `xterm.js` 前端 | 纯 Go，无 CGO；生态与第三方控件相对少 |

---

## 4. 调研范围 C：差异化机会（基于 A 节事实）

以下每条都先给「谁做到了什么」，再给「谁没做到」，最后给机会点。

**C1. 「串口 + SSH + Git + 构建/烧录」四合一不存在**

- 做到了：Tabby（<https://github.com/Eugeny/tabby>）把本地 shell / SSH / Telnet / **Serial** / SFTP 放进一个插件化应用；MobaXterm 把 SSH/RDP/VNC/FTP 放进一个 exe，并做到「连上 SSH 自动带出图形化 SFTP 浏览器」（官网 features 页）。
- 没做到：两者都**没有 Git 面板、没有烧录、没有固件概念**。反过来，PlatformIO / ESP-IDF VSCode 扩展 / Arduino IDE 2.x 有构建烧录与串口监视，但**没有 SSH/SFTP，Git 能力完全依赖宿主 IDE（VSCode）**。
- 机会点：**唯一把「连板子（串口/JTAG）」「连主机（SSH）」「管代码与固件（Git + 产物）」放在同一条工作流里**的产品位是空的。

**C2. 多设备 / 多会话编排**

- 做到了：SerialTest 支持 TCP/UDP/蓝牙服务端被多设备连接，并可**逐设备单独启停收发**（README 实测）；ESP-IDF 扩展一次只对「一个已选芯片 + 一个已选串口」操作。
- 没做到：在本次采集的样本里，**没有工具提供「N 个设备/端口 的会话矩阵」视图**（例如同时抓 3 块板子的 RTT、按设备打标签、统一时间轴对齐）。RTOS/多 MCU 联调场景下这是刚需。（这是「未检到」而非「不存在」的断言，见 D 节口径说明。）
- 机会点：**设备会话编排器**——设备即「工作区对象」，一个会话=端口/波特率/协议解析脚本/日志落盘策略/关联固件版本的组合。

**C3. 日志与源码、Git 提交的关联（最大空位）**

- 做到了：probe-rs（<https://github.com/probe-rs/probe-rs>）`probe-rs run` 把 flash + RTT/defmt 日志流接到终端，**日志里能带源码位置**（defmt 的卖点）；Ozone 提供源码级调试 + RTT + trace + 覆盖率（官网 key features）。
- 没做到：在本次采集样本中，**没有一个工具在抓日志时自动记录固件指纹并据此检索**——即日志落盘后与「当时跑的是哪个 commit / 哪个 bin」失去绑定。（同样是「未检到」，不是穷尽性断言。）
- 机会点：抓日志时写入**固件指纹（commit + dirty 标记 + bin hash）**，日志面板支持「按固件版本过滤 / 与 git blame 关联 / 一键 checkout 到出问题的 commit」。这一条同时用上了本项目的 Git 集成，是竞品结构上做不到的（它们的 Git 是外挂的）。

**C4. 协议解析脚本化（可复用的解析层）**

- 做到了：Serial Studio 支持多协议（UART/BLE/MQTT/Modbus/CAN）与可配置的数据流；VOFA+ 用「插件 + 协议」扩展（官网）；Arduino IDE 2.x / ESP-IDF 扩展只给固定串口监视器。
- 没做到：Serial Studio 的项目文件是配置形态，**不是通用脚本**；大多数串口助手只能十六进制/文本显示，遇到自定义二进制帧协议就得自己拉个 Python 解析。
- 机会点：**把帧解析做成用户脚本（JS/Lua/WASM 沙箱）**，同一份解析脚本可在「实时监视」和「离线日志回放」两种模式下复用；配合 C3 的固件指纹，形成「协议+固件+日志」三者可复现的组合。

**C5. 跨平台与「绿色便携」**

- 做到了：SerialTest 明确「不写注册表 + 单配置文件 + 可从当前目录读取配置」；MobaXterm 单 exe 便携；Tabby/WezTerm/Alacritty 跨三平台。
- 没做到：闭源三件套中 MobaXterm 官网明确面向 Windows 桌面；STM32CubeIDE / Ozone 虽然官网标注跨平台，但**其体积与启动开销本轮未核实**；ESP-IDF 扩展被 VSCode 绑定。
- 机会点：**同一份配置（设备+协议脚本+会话）跨 Windows/Linux/macOS 可携带**，对「实验室机器 + 个人笔记本 + CI 机器」三段式嵌入式工作流有实际价值。

**C6. 免装依赖的烧录/调试（可选扩展）**

- 做到了：probe-rs 支持 DAPLink/STLink/JLink/FTDI/ESP32 USB-JTAG/WLink/Blackmagic，覆盖 ARM/RISC-V/Xtensa（README 实测）。
- 没做到：把「烧录 + RTT 日志」当作「串口会话的另一种数据源」统一起来，并在 UI 上呈现的统一产品仍很少。
- 机会点：**数据源抽象**——串口、RTT、TCP、文件回放都是「字节流来源」，共用同一套解析/绘图/落盘管线。

**C7. 风险反面清单（不要做）**

- 不要把 Git 做成「又一个 Git GUI」：Lapce/VS Code 的 Git 面板已经足够好，本项目的 Git 价值在 **C3 的关联性**。
- 不要一上来做全功能终端仿真：Tabby/WezTerm/Alacritty/WindTerm 已经很卷，SSH 模块定位应是「连开发板子/服务器跑命令 + 传固件」。
- 不要忽略许可证：Serial Studio 是 GPLv3/商业双许可；WindTerm 未声明许可；paramiko 是 LGPL-2.1（见 D 节）。

---

## 5. 调研范围 D：不确定与未核实项（务必在后续工作中补齐）

**D1. 明确「未核实」的事实项**

1. **RT-Thread Studio**：未在 GitHub 找到可信的官方仓库；其开源范围、许可证、技术栈**全部未核实**。
2. **STM32CubeIDE / SEGGER Ozone / MobaXterm / VOFA+ 本体**：已确认为闭源（依据各自官网），但**具体功能清单细节、内部技术栈未核实**（闭源不可查证）。
3. **libgit2 的许可证**：本报告写「GPL-2.0 with linking exception」是依据其惯例，**本轮未逐字核实 LICENSE 文件**；引用前请查 <https://github.com/libgit2/libgit2/blob/main/COPYING>。
4. **OpenOCD 许可证**：API 返回 `NOASSERTION`，实际通常为 GPL-2.0-or-later，**未逐字核实**。
5. **正文表格中仅出现在附录 A、未在正文引用的仓库**（如 `microsoft/vscode`、`microsoft/terminal`、`NixOS/nix`、`net-ssh/net-ssh` 等）：这些是采集时作为对照或噪音一并抓取的，**其数值见附录 A，正文未对它们做任何结论**。
6. **isomorphic-git**：识别为候选但**完全未核实**（star/license/维护状态均无）。
7. **QCustomPlot 许可证**：多个 Qt 串口工具在用，但**本轮未核实**（通常为 GPL/商业双许可）。
8. **WezTerm 的 `portable-pty` crate 单独版本与许可**：未单独核实。
9. **Tauri / Electron / Wails / Avalonia / egui / iced 的体积与内存对比**：官方文档未提供可比较的量化数据，**本报告中相关单元格一律为「未核实」**。未采用任何博客/第三方跑分。
10. **「口碑」**：本报告没有用户满意度调查数据，所有口碑表述都是基于 star/活跃度/license 明确性的推断，**不是事实**。
11. **Warp 客户端 UI 开源范围、Tabby 的 Git 能力**：未核实。

**D2. 方法论限制**

- GitHub 匿名 API 限额 60 次/小时（本次采集过程中实际触发过限流），因此部分数据改用 **GitHub Search API**（配额独立，10 次/分钟）批量获取；Search API 不返回 `created_at`、`open_issues_count` 字段，相关列未使用。
- `pushed_at` 反映的是「任意分支最后一次 push」，**不等于活跃开发**；对 Archived 仓库未单独核对（本轮采集的仓库中未见 `archived = true`）。
- 所有 star 数都是**采集时点的快照**，会随时间变化。

**D3. 采集过程中修正的错误（保留以便审计）**

- 若干初始仓库名不存在（`wez/wezterm`、`alexcrichton/ssh2-rs`、`ttyd/ttyd`、`qtproject/pyside-setup` 等），经 GitHub 搜索纠正为 `wezterm/wezterm`、`rust-lang/ssh2-rs`、`tsl0922/ttyd`；PySide6 改用 **PyPI 官方元数据**核实（仓库归属不稳定）。
- 初稿曾写「Tabby 串口支持边缘、Git 能力大概率缺失」——这是推测，已改为以 raw 文件为证：`tabby-serial/package.json`、`tabby-ssh/package.json` 取到且标注 `tabby-builtin-plugin`，`tabby-git/package.json` 返回 404。
- 初稿曾写「WezTerm 内置串口支持」，已补上证据文件 `pty/src/serial.rs`（文件头注释说明为 serial-port-based tty）。
- 判定为噪音、未纳入报告的检索命中：大量同名但无关的仓库（如 `zszszszsz/.config`、`chrisneagu/FTC-Skystone-...`），已剔除，不作为证据。

**D4. 原始数据位置**

本次采集的原始 JSON 与脚本一并归档在 `docs/research/raw/`：

| 文件 | 内容 |
| --- | --- |
| [`raw/github-repos-2026-10-14.json`](raw/github-repos-2026-10-14.json) | 68 个仓库的 API 原始字段（star / fork / license / language / pushed_at / archived） |
| [`raw/manifests-2026-10-14.json`](raw/manifests-2026-10-14.json) | 各仓库 manifest 原文（用于判定技术栈） |
| [`raw/fetch-scripts/`](raw/fetch-scripts/) | 本次采集脚本（GitHub API 批量抓取、manifest 抓取、npm/PyPI 查询） |

附录 A 给出全部 68 个仓库的完整量化表，可与上文的结论逐条对照。

---

## 附录 A：本次采集到的全部仓库量化数据（采集日 2026-10-14）

来源：`https://api.github.com/repos/OWNER/REPO` 与 `https://api.github.com/search/repositories`。`license` 为 GitHub 自动识别的 SPDX 标识，`—` 表示仓库未声明许可证。

| 仓库 | Star | License | 语言 | 最近提交 |
| --- | --- | --- | --- | --- |
| [microsoft/vscode](https://github.com/microsoft/vscode) | 193,777 | MIT | TypeScript | 2026-10-14 |
| [electron/electron](https://github.com/electron/electron) | 123,317 | MIT | C++ | 2026-10-14 |
| [tauri-apps/tauri](https://github.com/tauri-apps/tauri) | 111,887 | Apache-2.0 | Rust | 2026-10-14 |
| [microsoft/terminal](https://github.com/microsoft/terminal) | 105,162 | MIT | C++ | 2026-10-13 |
| [zed-industries/zed](https://github.com/zed-industries/zed) | 91,706 | NOASSERTION | Rust | 2026-10-14 |
| [Eugeny/tabby](https://github.com/Eugeny/tabby) | 74,966 | MIT | TypeScript | 2026-10-12 |
| [apache/echarts](https://github.com/apache/echarts) | 67,508 | Apache-2.0 | TypeScript | 2026-10-13 |
| [alacritty/alacritty](https://github.com/alacritty/alacritty) | 65,960 | Apache-2.0 | Rust | 2026-10-05 |
| [warpdotdev/Warp](https://github.com/warpdotdev/Warp) | 65,452 | AGPL-3.0 | Rust | 2026-10-14 |
| [vercel/hyper](https://github.com/vercel/hyper) | 44,735 | MIT | TypeScript | 2026-08-21 |
| [lapce/lapce](https://github.com/lapce/lapce) | 38,921 | Apache-2.0 | Rust | 2026-10-09 |
| [wailsapp/wails](https://github.com/wailsapp/wails) | 36,548 | MIT | Go | 2026-10-14 |
| [kingToolbox/WindTerm](https://github.com/kingToolbox/WindTerm) | 32,433 | — | C | 2025-03-11 |
| [iced-rs/iced](https://github.com/iced-rs/iced) | 31,758 | MIT | Rust | 2026-10-13 |
| [AvaloniaUI/Avalonia](https://github.com/AvaloniaUI/Avalonia) | 31,661 | MIT | C# | 2026-10-14 |
| [emilk/egui](https://github.com/emilk/egui) | 31,422 | Apache-2.0 | Rust | 2026-10-14 |
| [wezterm/wezterm](https://github.com/wezterm/wezterm) | 29,228 | NOASSERTION | Rust | 2026-10-12 |
| [xtermjs/xterm.js](https://github.com/xtermjs/xterm.js) | 21,269 | MIT | TypeScript | 2026-10-13 |
| [espressif/esp-idf](https://github.com/espressif/esp-idf) | 19,241 | Apache-2.0 | C | 2026-10-12 |
| [plotly/plotly.py](https://github.com/plotly/plotly.py) | 18,840 | MIT | Python | 2026-10-09 |
| [jupyterlab/jupyterlab](https://github.com/jupyterlab/jupyterlab) | 15,361 | BSD-3-Clause | TypeScript | 2026-10-14 |
| [electerm/electerm](https://github.com/electerm/electerm) | 15,354 | MIT | JavaScript | 2026-10-14 |
| [arduino/Arduino](https://github.com/arduino/Arduino) | 14,667 | NOASSERTION | Java | 2025-10-11 |
| [tsl0922/ttyd](https://github.com/tsl0922/ttyd) | 12,498 | MIT | C | 2026-08-12 |
| [RT-Thread/rt-thread](https://github.com/RT-Thread/rt-thread) | 12,282 | Apache-2.0 | C | 2026-10-14 |
| [GitoxideLabs/gitoxide](https://github.com/GitoxideLabs/gitoxide) | 12,012 | Apache-2.0 | Rust | 2026-10-14 |
| [libgit2/libgit2](https://github.com/libgit2/libgit2) | 10,624 | NOASSERTION | C | 2026-10-14 |
| [leeoniya/uPlot](https://github.com/leeoniya/uPlot) | 10,557 | MIT | JavaScript | 2026-10-11 |
| [paramiko/paramiko](https://github.com/paramiko/paramiko) | 9,879 | LGPL-2.1 | Python | 2026-10-10 |
| [platformio/platformio-core](https://github.com/platformio/platformio-core) | 9,527 | Apache-2.0 | Python | 2026-09-23 |
| [go-git/go-git](https://github.com/go-git/go-git) | 7,798 | Apache-2.0 | Go | 2026-10-14 |
| [Serial-Studio/Serial-Studio](https://github.com/Serial-Studio/Serial-Studio) | 7,237 | NOASSERTION | C | 2026-10-13 |
| [serialport/node-serialport](https://github.com/serialport/node-serialport) | 5,994 | MIT | TypeScript | 2026-01-17 |
| [mscdex/ssh2](https://github.com/mscdex/ssh2) | 5,824 | MIT | JavaScript | 2026-08-20 |
| [gitpython-developers/GitPython](https://github.com/gitpython-developers/GitPython) | 5,185 | BSD-3-Clause | Python | 2026-10-14 |
| [arduino/arduino-cli](https://github.com/arduino/arduino-cli) | 5,059 | GPL-3.0 | Go | 2026-10-12 |
| [steveukx/git-js](https://github.com/steveukx/git-js) | 3,857 | MIT | TypeScript | 2026-09-26 |
| [pyserial/pyserial](https://github.com/pyserial/pyserial) | 3,579 | NOASSERTION | Python | 2026-05-19 |
| [golang/crypto](https://github.com/golang/crypto) | 3,351 | BSD-3-Clause | Go | 2026-10-11 |
| [arduino/arduino-ide](https://github.com/arduino/arduino-ide) | 3,299 | AGPL-3.0 | TypeScript | 2026-10-14 |
| [probe-rs/probe-rs](https://github.com/probe-rs/probe-rs) | 2,974 | Apache-2.0 | Rust | 2026-10-14 |
| [openocd-org/openocd](https://github.com/openocd-org/openocd) | 2,352 | NOASSERTION | C | 2026-10-13 |
| [rust-lang/git2-rs](https://github.com/rust-lang/git2-rs) | 2,114 | Apache-2.0 | Rust | 2026-10-05 |
| [microsoft/node-pty](https://github.com/microsoft/node-pty) | 2,046 | NOASSERTION | TypeScript | 2026-10-03 |
| [Eugeny/russh](https://github.com/Eugeny/russh) | 1,901 | Apache-2.0 | Rust | 2026-10-05 |
| [wh201906/SerialTest](https://github.com/wh201906/SerialTest) | 1,667 | GPL-3.0 | C++ | 2024-06-17 |
| [libssh2/libssh2](https://github.com/libssh2/libssh2) | 1,593 | BSD-3-Clause | C | 2026-10-14 |
| [espressif/vscode-esp-idf-extension](https://github.com/espressif/vscode-esp-idf-extension) | 1,472 | Apache-2.0 | TypeScript | 2026-10-14 |
| [platformio/platformio-vscode-ide](https://github.com/platformio/platformio-vscode-ide) | 1,462 | Apache-2.0 | JavaScript | 2025-01-11 |
| [hyOzd/serialplot](https://github.com/hyOzd/serialplot) | 1,111 | GPL-3.0 | C++ | 2025-05-28 |
| [bugst/go-serial](https://github.com/bugst/go-serial) | 876 | BSD-3-Clause | Go | 2026-07-15 |
| [serialport/serialport-rs](https://github.com/serialport/serialport-rs) | 754 | NOASSERTION | Rust | 2026-09-08 |
| [rust-lang/ssh2-rs](https://github.com/rust-lang/ssh2-rs) | 569 | Apache-2.0 | Rust | 2026-10-06 |
| [eclipse-embed-cdt/eclipse-plugins](https://github.com/eclipse-embed-cdt/eclipse-plugins) | 559 | EPL-2.0 | C | 2026-09-09 |
| [je00/Vodka](https://github.com/je00/Vodka) | 472 | MIT | QML | 2021-08-02 |
| [eclipse-cdt/cdt](https://github.com/eclipse-cdt/cdt) | 375 | EPL-2.0 | Java | 2026-10-13 |
| [mich-w/QtSerialMonitor](https://github.com/mich-w/QtSerialMonitor) | 270 | GPL-3.0 | C++ | 2022-03-22 |
| [berkowski/tokio-serial](https://github.com/berkowski/tokio-serial) | 267 | MIT | Rust | 2026-06-15 |
| [gthvidsten/open-serial-port-monitor](https://github.com/gthvidsten/open-serial-port-monitor) | 186 | GPL-3.0 | C# | 2020-01-19 |
| [gbmhunter/NinjaTerm](https://github.com/gbmhunter/NinjaTerm) | 120 | GPL-3.0 | TypeScript | 2026-09-07 |
| [ironcold/komport-qt6](https://github.com/ironcold/komport-qt6) | 1 | GPL-2.0 | C++ | 2026-10-14 |
| [win001/Serial_UART_PyQt_APP](https://github.com/win001/Serial_UART_PyQt_APP) | 0 | MIT | Python | 2025-11-05 |

**已核实但与主题无关的检索命中（不作为结论，仅供排除）**：`microsoft/winget-cli`（26,496 / MIT）、`NixOS/nix`（17,864 / LGPL-2.1）、`net-ssh/net-ssh`（1,024 / MIT / Ruby）、`brgl/busybox`（94 / 2019-04-25，非官方 mirror）。

**附录 B：非 GitHub 一手来源（闭源或非代码分发的关键事实）**

| 对象 | 来源 URL | 核实到的内容 |
| --- | --- | --- |
| MobaXterm | <https://mobaxterm.mobatek.net/> | 闭源商业（Home 免费 / Professional 收费）；单 exe；SSH/X11/RDP/VNC/FTP/Mosh；连上 SSH 自动弹出图形化 SFTP 浏览器；Windows 定位 |
| VOFA+ | <https://www.vofa.plus/> | 上位机本体闭源商业（官网提供密钥购买/离线激活）；插件框架开源指向 <https://github.com/je00/Vodka> |
| STM32CubeIDE | <https://www.st.com/en/development-tools/stm32cubeide.html> | ST 官方闭源免费 IDE（页面为产品页，功能细节未逐一核实） |
| SEGGER Ozone | <https://www.segger.com/products/development-tools/ozone-j-link-debugger/> | 闭源商业（perpetual 许可；J-Link PLUS/Ultra/Pro、J-Trace PRO 附赠许可）；Windows/macOS/Linux；源码级 C/C++/Rust 调试、RTOS 插件、RTT、trace、profiling、脚本自动化 |
| PySide6 | <https://pypi.org/pypi/PySide6/json> | 6.12.0；license 字段 `LGPL-3.0-only OR GPL-2.0-only OR GPL-3.0-only`；`requires_python >=3.10,<3.16`；Windows wheel：Essentials 78.3 MB + Addons 38.6 MB + shiboken6 1.2 MB |
| Tauri 体积 | <https://v2.tauri.app/concept/size/> | 官方只给瘦身手段（LTO / `opt-level="s"` / `strip` / `panic="abort"` / `removeUnusedCommands`），**未给出体积数字** |
| Tauri 平台支持 | <https://github.com/tauri-apps/tauri> README | Windows 10+ / macOS 10.15+ / Linux（v1 需 webkit2gtk 4.0，v2 需 4.1）/ iOS 9+ / Android 7+ |
| Electron 平台支持 | <https://github.com/electron/electron/blob/main/README.md> | 官方支持矩阵以该 README（抓取时为 v44.7.2 分支）为准 |
| npm 包体积 | <https://registry.npmjs.org/> | `@xterm/xterm` 6.0.0 unpacked 5.6 MB；`node-pty` 1.1.0 unpacked 61.4 MB；`uplot` 1.6.32 0.5 MB；`echarts` 6.1.0 57.5 MB；`electron` 44.7.2 仅 1.1 MB（安装器，非运行时体积） |

