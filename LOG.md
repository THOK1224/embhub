# LOG — embhub 施工日志

记录每一轮做了什么、为什么这么做、验证了什么。新记录追加在**最上面**。

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
| **出窗口** | ⬜ **未通过** | 沙箱内 `HRESULT(0x8000FFFF)`；需在沙箱外验证 |
| **插拔后列表刷新** | ⬜ 未验证 | 依赖窗口 |
| **115200 收到真实数据** | ⬜ 未验证 | 依赖窗口 |

### 本轮的诚实补充

- 我**无法在沙箱内程序化验证窗口**，所以「出窗口」这条必须由用户在沙箱外启动后目视确认。我不把「编译通过」当作「窗口可用」。
- 用户上次拒绝提权是因为要关机；本轮明确授权了一次联网构建。**GUI 启动没有再请求提权**，改为请用户在普通终端执行——这既符合已确认的合作方式，也顺便构成一次天然对照实验（沙箱外能起来 ⇒ 沙箱就是原因）。

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
# 1) 设置环境（若脚本被执行策略拦，用下面第二行）
.\scripts\env.ps1
pwsh -ExecutionPolicy Bypass -File .\scripts\env.ps1

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
