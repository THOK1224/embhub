# LOG — embhub 施工日志

记录每一轮做了什么、为什么这么做、验证了什么。新记录追加在**最上面**。

---

## 2026-10-16 · 第 16 轮：GUI 挤压缺陷根因修复（t17）验收通过

用户实测报的布局缺陷（接收区内容增长把发送区/事件区挤出可视区、点不到）已由 `t17` 修复并验收（`t18` 由 captain 裁决 pass）。实测：core 76 / cli 27 / **app 17**；clippy / fmt / build 全 0。

### 根因（成员分析，captain 核实后接受——并纠正了 captain 的初判）

旧 CSS 是**两个独立缺陷叠加**：

1. **重叠**：`fieldset { min-height: 0 }` 是**通吃**规则，把内容固定的发送区/事件区也变成可压缩项（body 纵向 flex 默认 `flex-shrink: 1`）→ 被压矮；又因 `overflow` 默认 `visible`，被压矮面板里的行**直接画到下一个兄弟上**。
2. **把下方顶出去**：`#rx { min-height: 110px }` 这条**硬下限**被捐给接收区的自动最小高度（≈160px）→ 接收区"再挤也不能小于 ~160px"，只能从别人身上抽高度。

**captain 的初判（"滚动容器缺 min-height: 0"）方向对但指错了元素**：关键作用点在**非滚动的 `.pane`**（滚动容器的自动最小尺寸本来就是 0）。真正的错是「`min-height: 0` 发给了不该收的固定区」+「滚动内容又吃了一条 px 下限」。

### 修复（只改约束表达，零写死高度）

`fieldset.fixed { flex: 0 0 auto }`（固定区永不收缩）· `fieldset.pane { flex: 1 1 auto; min-height: 0 }`（接收区是唯一伸缩区）· `#rx { flex: 1 1 auto; min-height: 0; min-width: 0; overflow: auto }`（删掉像素下限）+ `overflow-wrap: anywhere` · `html, body { height: 100% }` 与 body `overflow: auto` 仅作极端小窗口兜底。

**根因注释写在 `<style>` 顶部**（约 40 行），并在 `#rx` 处留下显式禁令「**不许写 min-height: <数字>px —— 那会把下方区域顶出窗口**」。captain 用正则扫全文 `min-height:\s*\d+px` **只命中注释内一处**，无任何真实像素下限。

### 持久闸门

新增 Rust 测试 `layout_invariants_prevent_the_receive_pane_from_squeezing_others`（随 `cargo test` 永远跑），钉住「谁固定/谁伸缩/谁滚动/不许像素凑高/长行可断」。成员另用对照脚本证明闸门**对修复前的 CSS 变红（9 处不满足）**。

> ⚠️ **captain 无法独立复现该 RED**：修复前的 CSS **不在版本控制里**（仓库只有一次 `init` 提交，此后全部工作未提交），取不到旧文件。这是流程缺口，已向维护者建议落实提交。

### captain 的两次自我纠错（都记下来）

1. **误判一（方法问题）**：captain 比较自检页与交付文件 CSS 得 4731 vs 4630，一度质疑"逐字一致"的说法。**逐字符定位后发现差值恰好是 101 个 CR**（自检页 CRLF、交付文件 LF）；统一换行后 **4630 == 4630 且 `-ceq` 为真** ⇒ **成员声明准确，是 captain 的比较方法错了**。
2. **新证据（成员未取得）**：成员报 headless Edge"零输出"。captain 改为**让 cmd 做文件重定向以绕开 PowerShell 管道**，拿到 2104 字节诊断 —— 真因是 **Chromium mojo `platform_channel` 需要命名管道而被沙箱拒绝（`拒绝访问 (0x5)`）**，crashpad `OpenProcess` 同被拒。这独立证实了"沙箱内无法做像素级量测"，并给出了比"零输出"明确得多的根因。

### 明确移交

「内容增长后下方仍可见可点」「800×600 仍成立」「长行不撑宽」三条的**像素级结论无人验过**，转为用户验收项 **G11**。结构层面（CSS 不变量 + Rust 闸门）已验。另交付自检页 `target\tmp\gui-layout-selfcheck.html`（CSS 与界面逐字一致）供真实浏览器一键量测。

---

## 2026-10-16 · 第 15 轮：G8–G10 用户实测通过；发现 GUI 布局缺陷（已派修）

### 用户侧验收：G8–G10 通过

维护者在普通 shell 里跑新构建，三件事都验到了：

- **G9 时间戳**：接收区出现 `[14:32:03.523] PONG fw=2.0.0 t=12711ms` 这类**墙上时钟**时间戳，与挂钟对得上（说明用的确实是精确换算，不是"首批到达时间"近似）。
- **G10 定时发送**：事件区可见「定时发送已启动：每 1000 ms 发一帧 4 字节（HEX AA5501FF）」「定时发送已停止：共发出 12 帧 / 5 帧」，启动-停止的计数自洽。
- **G8 参数**：控件与状态行显示正常（截图里是默认 `8N1 无流控`）；维护者确认通过。
  > **口径说明**：截图里状态行显示的是**默认 8N1**，所以「非默认参数真机生效」这条证据不在图里。不追问，仅标注。

**顺带核实的一件事**：p1 里 `TX 字节 124` 与 `TX 次数 18` 一开始看着对不平（若都是 4 字节帧应有 31 次写）。captain 去读 `Session::send`（session.rs:224-243）——它是**唯一**更新 tx 计数的地方，每次成功发送同时 `bytes += len` 与 `writes += 1`。据此反推：当前会话 = **5 次定时 PING（4B×5=20）+ 13 次手动 BURST 帧（8B×13=104）= 124 字节 / 18 次写**，**完全自洽**。原来看似矛盾是 captain 把事件区里属于**上一个会话**的「12 帧」条目误算进了当前会话。**计数无 bug，是 captain 算错——记下来以免下次又疑神疑鬼。**

### 新发现：GUI 布局缺陷（用户实测，已派修 t17）

用户截图显示：**接收区内容不断增长时会向下挤压发送区与「事件与错误」区**，最终下方区域被挤出可视区／无法点击（截图里「事件与错误」标题与「定时发送间隔」行重叠、下半截被窗口切掉）。

captain 初判根因：**flexbox 中滚动子项缺少 `min-height: 0`** —— 缺该约束时 flex item 的自动最小尺寸等于内容的 min-content 高度，`overflow: auto` 因此永不生效，容器只会随内容长高并把兄弟节点挤出视口。

据此派 `t17`（implementation，`coder-ds`），明确要求：
- **根因修复而非症状补丁**：必须说明"为什么"会挤压，并在**修复处写根因注释**；**禁止把某个高度写成固定数字**（那只在某一种窗口尺寸下凑对）。
- 布局不随内容增长而变；发送区与事件区**始终可见可点**；**800×600 小窗口也成立**；超长行只在接收区内横向滚动、不撑宽页面。
- 不破坏既有：app 16 条测试不得删除；core 76 / cli 27 一字不改。

> 说明：这个缺陷是**用户在交互验收中发现的**，不是评审产出的 finding。因此按 `implementation` 建任务，而不是 `repair`（后者要求挂 `sourceTaskId` + `sourceFindingIds`，硬套等于伪造来源）。

---

## 2026-10-16 · 第 14 轮：增量 F2（GUI 参数控件 + 时间戳开关 + 定时发送）验收通过

`t14` 一次 attempt 完成，**严格按 captain 的修订版契约执行**（用 `SerialFrame` 而非 `SerialConfig`、停止位只给 1/2、零自定义 parity 规则、时间戳走 `wall_time_ms_for`）。`t16`（review）由 captain 承担，裁决 **pass**（范围限定为适配层 + 静态接线）。

### 交付内容（只改 `crates/embhub-app` 2 个文件）

- `open_session` 改收配置对象 `{port,baud,dataBits,parity,stopBits,flowControl}` → `parse_frame_from_config()` → `SerialFrame::validate()` → `SerialSource::open_with_frame`。
- 四个参数选择器（**运行时统一禁用** + 状态行显示实际生效帧格式）；接收区 `[HH:MM:SS.mmm]` 时间戳开关（切换无损重渲染）；定时发送（间隔 + 起停 + 状态）。
- 事件载荷升为 `{seq, t_mono, wall_ms, hex}`，`wall_ms` 在 **Rust 侧**用 core 的精确换算得到。

### captain 独立验收

- 复跑：app **16 passed**（t10 的 8 条逐名全在 + 新增 8 条）、core 76、cli 27、clippy/fmt/check 全 0、loopback 0。
- **captain 自写正则做静态接线校验**：命令集从 5 升到 **7**（新增 `start_timed_send`/`stop_timed_send`），前后端**双向差集为空**；事件名与载荷键一致；元素 id 引用 27/定义 31/**缺失 0**。
- **停止位下拉逐行核对**：只有 `value="1"` 与 `value="2"`；全文仅两处 "1.5"——一处 CSS `line-height`，一处是**注释解释为何不提供**。把理由写在最可能被后人加回去的位置，是好习惯。
- **代码级复核定时发送**：`start_timed_send` 复用与手动发送**同一个 `build_frame`**（一致性是结构性的）；间隔 0 被拒；已在跑时先停旧定时器再起新线程；`close_session` 第一步停定时器并 join，线程另有「session 不在跑即自动退出」第二道闸。四条行为单测覆盖「停后不再发」「关会话自动停」「帧与手动一致」「间隔 0 被拒」。
- 范围：core/cli 的 mtime 停在 t13 窗口（14:14–14:15），app 为 14:21–14:22；Cargo.lock 未变。

### captain 认领/观察到的两点（不影响验收）

1. **措辞不严谨**：`stop_timed_send` 注释写「幂等：没在跑时返回明确错误」——会报错就不是幂等。**意图是对的**（不静默假装成功），但用词应改为「显式失败」或做成真正幂等。
2. **遗留物累积**：`target\debug` 下已有两个改名占用的产物（`.locked-20261015` 12.13 MB + `.locked-t14` 12.42 MB，共约 24.5 MB）。已 gitignore，但建议后续清掉，或仅在确需交付 exe 时才重命名（平时用 `cargo check` 佐证编译即可）。

### 明确移交、不予声称

**「点参数控件、看时间戳、开定时发送」三件事没有任何人点过**（沙箱内 WebView2 建窗被拦）。已转为用户验收项 **G8–G10**，写进 [`docs/ACCEPTANCE.md`](docs/ACCEPTANCE.md) 第 3 节（含"若时间戳明显偏移就是真 bug"这类可证伪判据）。

**延续 t13 的未验证项**：四项串口参数**是否真的改变硬件通信行为**仍未真机验证（无第二串口对端、不向未知设备写入）——只能由 G8 用板子验。

### 验收工具同步

- `scripts/accept.ps1`：B1 期望值更新为 76、B6 为 16，人工项提示补 G8–G10；captain 实跑 **19 passed / 0 failed**。
- `docs/ACCEPTANCE.md`：第 3 节扩为 **G1–G10**，覆盖行补上 F1/F2/C2b。

### 流程：抢单第 6、7 次

`t14` 完成时 coder-ds 立刻空闲，`t16` 又被抢（captain reassign 收回）。**已可确认抢单是空闲成员的默认行为**：不预先占住它，共享池任务必被抢。下一轮起：**建评审任务前先把下一个实现任务建好**。

---

## 2026-10-16 · 第 13 轮：增量 F1（core 串口参数 + 精确时钟）验收通过；**captain 的契约错了两次**

### t13 交付并验收（pass）

`SerialConfig` 只加了 `baud` 的历史结束：新增四个枚举（`DataBits` 5/6/7/8、`Parity` None/Odd/Even、`StopBits` 1/1.5/2、`FlowControl` None/RtsCts/XonXoff）+ `SerialFrame`（四项参数载体，Default = 8N1）；**唯一落点** `build_serial_builder(cfg, frame)` 把四个值真正链到 serialport builder，读写两条路径共用；`validate()` 对非法组合**点名冲突**。时间戳侧新增 `SessionStart { wall_ms, mono_micros }`（create_session 里背靠背取双时钟）→ `SessionSnapshot::wall_time_ms_for(t_mono)` 精确换算。

实测：core **76 passed**（+13）、cli 27、app 8；clippy / fmt 干净；`check -p embhub-app --all-targets` exit 0；回归 5 条全 0；Cargo.lock 未变。

### 一处形状偏离，captain 裁定**接受**

契约要求把四项参数加在 `SerialConfig` 上；实际交付为独立 `SerialFrame` + `open_with_frame(cfg, frame)`。原因链是硬的：`SerialConfig` 被 out-of-scope 的 cli/app 用**完整字面量**构造，Rust 无字段默认值（成员实测 `E0658`），加字段必致 `E0063`；成员先在那两处各补 `..SerialConfig::default()`，**被平台门禁以 `crates/embhub-cli/src/main.rs is out_of_scope` 拒绝**，于是改为不动那两个文件。

**captain 独立核实（未采信自述）**：cli/app 中只匹配到原有 `SerialConfig {` 字面量，**无 `..Default::default()`、无 `SerialFrame` 引用** ⇒ 临时 6 行确无残留；`build_serial_builder` 确实把四个值全链上 builder；`open(cfg)` 有明确的 8N1 文档并委托给 `open_with_frame`，**不是静默陷阱**。

**裁定理由**：「帧格式（数据位/校验/停止位）」本就是串口既有术语，与 `SerialConfig`（端点 + 超时）分层是更清晰的划分；目标（四项可配、真正生效、非法组合点名报错、有测试）全部达成；成员遇阻时**遵守平台门禁而不是绕开**——这是本轮最值得肯定的行为。**契约里的"加在 SerialConfig 上"是手段而非目的，captain 承认自己过度指定了实现形状。**

### captain 认领的两处契约错误（重要）

1. **1.5 停止位**：captain 在契约里要求支持 1/1.5/2，但 serialport 4.10.1 的 `StopBits` 只有 `One`/`Two`（成员读 `.cargo` 源码确认）。成员**拒绝静默折算**、让 `StopBits::OneAndHalf` 返回明确错误——正确处理。
2. **"parity 与 data_bits 的非法搭配"是 captain 凭空写的规则**，真实平台（serialport 枚举 / Win32 DCB / 16550）**不存在**该限制。成员**明确拒绝编造假规则**，只实现了两类真实冲突（5 数据位+2 停止位；6/7/8 数据位+1.5 停止位）——这个判断值得肯定。

据此已用 `agent_teams_amend_task` **修订 t14 契约**（记录 1 版修订）：改用 `SerialFrame`/`open_with_frame` 形状、停止位只给 1 和 2、删去 parity×data_bits 要求、时间戳指定用 `wall_time_ms_for(t_mono)`。**注意：修订时 t14 已被领取**，故 captain 另发一条消息把三处变更直接告知实现者，避免它按废止的契约干活。

### app build 的 exit 101 不是编译错误

`cargo build -p embhub-app` 报 101，真因是 **PID 12452（13:44:57 启动，用户的 GUI 交互验收实例）占用了 exe**，无法重链接。captain 核实：该进程确实在跑，且 `cargo check -p embhub-app --all-targets` exit 0 ⇒ 编译无问题。成员**没有杀掉用户的 GUI** 去凑绿 build，处理正确。

### 已知未验证项（如实保留）

四项参数**是否真的改变了硬件通信行为**未做真机验证：本机没有第二个串口对端，且我们不向未知设备写入（沿用既有约束）。当前证据只有「枚举→serialport 映射单测」+「唯一落点代码复核」。真机验证留到 t14 界面做好后，由用户用板子试非 8N1 参数。

### 流程

本次评审任务**没有发生抢单**——因为 coder-ds 当时正忙 t14。再次印证：**让成员保持有事做**是解决"共享池任务被空闲成员抢单"的正解。

---

## 2026-10-16 · 第 12 轮：G1–G7 用户实测通过（captain 独立核了落盘日志）；转向「界面完善」

### 用户侧验收结果（G1–G7）

维护者在普通 shell 里跑通了全部 7 条：端口列表填充（这次是 `COM13 [usb] VID:PID=1A86:7523 USB-SERIAL CH340`）、插拔刷新、打开/关闭会话、接收区文本↔HEX、**真机发送成功**（发 `AA55080A001000FF` → 板子回 `BURST START count=10 size=16` + 10 行 + `BURST DONE`；另有两次 `AA5501FF` → `PONG fw=2.0.0`）、RX/TX 计数、按键上报都能看到。

### captain 的独立核验：拿用户这次真实会话的落盘日志做字节级解析

会话目录 `target\debug\embhub-data\sessions\s1792129848436-0000`，用**纯 PowerShell 按格式遍历**（不碰任何 Rust 代码）：

| 项 | 值 |
| --- | --- |
| `header.bin` | 135 字节；magic `EMBHLOG`、**version=2**、`source_config = serial:COM13@115200`；账目 24+88+4+19 = **135 == 实际** ✓ |
| `seg-000000.bin` | 2487 字节；**78 条记录**、payload 总字节 **1239**、未成记录尾部 0 |
| 与界面显示对比 | 界面 RX **1239** ⇒ **磁盘上就是 1239，完全一致** |

**这是端到端真机证据：GUI → core → 落盘，一个字节不差。** 也是本项目第一次用"用户在真机上跑出来的会话"做独立核对。

### 一处必须记录的 captain 疏漏：**GUI 会话没有固件指纹**

同一次解析里，指纹段内容是：

```
not_collected = '会话创建时未提供固件指纹（未采集）'
```

也就是说**从 GUI 开出来的每一个会话都不带指纹**。SPEC 3.5 明确要求"采集时机：会话创建时"，而 B1/B2 两轮做出来的指纹能力**在产品主界面（GUI）上是隐形的**。

**根因是 captain 定 t10 契约时的疏漏**：`open_session(port, baud)` 根本没给指纹留位置。这不是实现者的问题——它严格按契约做了。

**已登记为待办项（增量 D）**：把指纹采集接进 `open_session`。维护者本轮选择了先做界面完善（增量 F），所以 D 延后。

### 三条如实记录

1. **`os error 5` 已定性**：日志里 `[13:54:52] 采集已停止（致命错误）：读取失败: 拒绝访问。(os error 5)`，维护者确认是**当时断开了板子与 PC 的连接**。因此这是预期行为，不是 bug。附带好消息：随后点「关闭会话」日志被**正常 seal**，1239 字节完整落盘，降级行为正确。
2. **维护者对"错误提示"的原则（captain 完全同意并采纳）**：「拔线后给出清晰提示，**如果你真的能做到判明当前采集终止是什么原因，你就做，不然不要做以免误导**」。据此：**不做猜原因的错误文案**，只在能**确证**原因时给具体诊断；**不做"一键重连"**（维护者认为没必要）。
3. **G7 的"看到丢弃计数变化"未获证据**：截图里显示丢字节为 **0**（只发了约 200 字节的 BURST，远不足以触发背压）。计数器的**存在**验到了；"看到它变化"没验到。但背压路径本身已由 C1/C3 的**显示 100% 丢弃**对抗测试独立证明，故不视为风险——只是把话说准。

### 下一步：增量 F（用户指定「先完善串口助手界面」）

按**层**拆两个任务，避免同时改同一批文件：

| 任务 | 内容 | 依赖 |
| --- | --- | --- |
| **t13（core）** | `SerialConfig` 扩 数据位/校验位/停止位/流控 并**真正应用到 serialport**；非法组合给具体冲突错误；另暴露精确的 `(start_wall_ms, start_mono_micros)` 对应关系 | — |
| **t14（GUI）** | 四个参数控件（`open_session` 改收配置对象）；接收区**墙上时钟时间戳开关**；**定时发送**（间隔+启停，关会话自动停） | t13 |

契约里两条防坑要求：① **运行中参数控件必须禁用**，避免"界面显示 115200 实际跑 9600"这类误导；② 时间戳**必须用精确对应关系换算**，**不得用"第一批到达时间"近似冒充会话起始**（那会引入等于沉默时长的偏移）。

明确**不做**：导出/保存接收内容到文件——那需要系统文件对话框（引入新 crate，沙箱无网络装不了），留到后续用"路径输入框 + 保存命令"的零依赖做法。

---

## 2026-10-16 · 第 11 轮：增量 C2b（GUI 串口助手面板）验收通过——**但交互部分明确移交用户**

`t10`（implementation，`coder-ds`）一次 attempt 完成；captain 建 `t12`（review）并亲自裁决 **pass**，但**范围限定为「适配层与接线」**。

### 交付内容（只动 `crates/embhub-app`，3 个文件）

| 文件 | 内容 |
| --- | --- |
| `src/main.rs` | 4 个命令 `open_session(port,baud)` / `close_session()` / `session_stats()` / `send_data(data,mode,append)`（保留 `list_ports`）；事件 `session-batch` 载荷 `{seq,t_mono,hex}`；**删掉 spike 专用的 `read_probe`**；建窗失败从裸 panic 改为明确错误 + 排查提示 + exit 1；8 个单测 |
| `ui/index.html` | 重写为串口助手：刷新端口 / 选端口与波特率 / 打开·关闭（按钮状态随会话变）/ 接收区 文本↔HEX / 清空 / 自动滚动 / 发送区 文本·HEX + none·CR·LF·CRLF / RX·TX·显示丢弃 三计数 / 错误面板。原生 HTML/JS，零构建依赖 |
| `capabilities/default.json` | 显式补 `core:event:default` |

### captain 独立验收

- 复跑：`build -p embhub-app` exit 0；clippy/fmt 干净；core **63** / cli **27**（与 t9 完全一致 ⇒ core 与 CLI 未动）；**app 8 passed**；`loopback` 与 `list` exit 0。
- **captain 自己写正则做静态接线校验**（不采信实现者的 Python 脚本）：后端注册命令集与前端 `invoke` 集**完全对称**（5 个，双向差集为空）；`emit`/`listen` 事件名一致（`session-batch`）；前端 18 个 `getElementById` 引用**全部有定义**（缺失 0）；另核实 3 个"定义但未被 getElementById 引用"的 id 均正当（`bauds` 是 `<datalist>`、`counters` 是 CSS 容器、`rxHint` 是静态提示）；无 import / require / node_modules / 外链 script ⇒ 零构建依赖成立。
- **范围用两道证据**：git status 仅 `crates/embhub-app/**`；core/src、cli/src 的 mtime 停在 t9 的时间窗（13:14–13:15），app 为 13:20–13:23。
- **代码级复核事件泵**：`pump_batches` 在会话结束后用 0 超时**排空尾部批次**再退出（否则最后几批"已落盘没上屏"）；其单测 `pump_turns_core_batches_into_events` **故意不置 stop 标志**、逼泵自己走该路径，再断言 `seq` 从 1 起严格递增——真在测那条路径。

### 明确移交、不予声称的部分（本轮最重要的一条）

**交互式 GUI 验收未完成。** DSH 沙箱内 WebView2 创建必失败（`HRESULT(0x8000FFFF)`）；实现者把 `EMBHUB_DATA_DIR` 改到沙箱允许位置后重试结果相同，据此判定为**环境限制而非代码问题**（与 Phase 0 spike 当年的现象一致，那次是用户在沙箱外启动才成功）。

实现者**如实声明"没有实际点过界面"**，没有把静态校验冒充成交互验收——处理正确。captain 同样**不声称**这一段已完成，把它拆成 7 条用户验收项 **G1–G7** 写入 [`docs/ACCEPTANCE.md`](docs/ACCEPTANCE.md)：窗口/端口列表、插拔刷新、打开会话、接收区三操作、**发 `AA5501FF` 看板子回不回 `PONG`**、RX/TX 计数对账、高流量下显示丢弃可见而 RX 不停。

### 裁定：**不需要**加 headless 自检入口

实现者提议给适配层加 headless 自检入口来"补上 GUI 的自证缺口"。**captain 否决**：适配层逻辑（命令、事件泵、DTO 映射、hex/append 解析）已由 8 个单测覆盖（泵用真回环会话），再加 headless 入口只会重复跑同一批函数，**无法覆盖真正的缺口（WebView2 渲染与 DOM 交互）**。为把"不可自证"改造成"看着可自证"而在生产代码里加测试专用入口，是花架子而不是证据——缺口应如实移交人工。

### 验收工具同步更新

- `scripts/accept.ps1`：**19 项自动检查**（新增 B6 app 测试、F1–F3 发送能力），captain 实跑 **19 passed / 0 failed**；人工项提示改为 G1–G7。
- `docs/ACCEPTANCE.md`：补充 F 组（发送能力）、重写第 3 节为 G1–G7 带操作步骤与失败处置、更新第 5 节诚实清单、第 6 节可证伪点新增 4 条 GUI 相关（含"G5 板子不回而别的串口助手能回"这种最关键的对照）。

### 流程问题（第 5 次，根因已确认）

`t12` 又被抢（t10 一完成 coder-ds 就空闲）。**根因确认**：成员空闲时共享池任务会被立即抢单。**解法**：先把下一个实现任务建好占住成员，再建评审任务。

---

## 2026-10-16 · 第 10 轮：目标转向「串口助手基本功能」；增量 C2a（发送能力 + Session）验收通过

### 目标调整

维护者明确：**当前核心目标是实现串口助手的基本功能**。据此把工作重心从"日志可追溯"转到"可用"。

差距诊断：C1 的流式管线**只能读**。SPEC 里 `ByteSource` 是只读接口，所以"发送"必须作为**另一项能力**引入，而不是往 `ByteSource` 里塞 `write`。于是拆成两个任务：

| 任务 | 内容 | 状态 |
| --- | --- | --- |
| **t9** | core：`ByteSink` 发送能力 + `Session` 最小实现 + `cli send` / `cli loopback` | ✅ 已验收 |
| **t10** | GUI：串口助手面板（打开/关闭 + 接收显示 + 发送 + 计数） | 依赖 t9，进行中 |

### t9 交付内容

| 文件 | 内容 |
| --- | --- |
| `core/src/source.rs` | 新增 `ByteSink { write_all, flush }`（`SinkError = SourceError`，因读写失败模式同构）；`SerialSource::try_clone_sink()` 用 serialport `try_clone()` 给出**共享同一物理端口**的写句柄；`SyntheticSource::loopback()` 给**回环**写句柄 |
| `core/src/session.rs`（新） | SPEC 3.4 最小实现：`start`/`stop`（幂等）/`stats`（含 tx 计数，停后仍可读）/`recv_batch`/`send` |
| `cli/src/main.rs` | `loopback --bytes N`（回环四方一致 + 逐字节比对）、`send --port <p> --data <hex:…\|text:…> [--append …] [--repeat N]` |

**`ByteSink` 单独成 trait 的理由**（实现者写在模块注释里，captain 认可）：发送不是"反过来读"——读句柄与写句柄可以是两个独立对象，失败模式也不同；并进 `ByteSource` 会逼着回放源这种"天然只能读"的实现去实现写方法。

### captain 独立验收

- 复跑 7 条 verify：core **63 passed**、cli **27 passed**、clippy/fmt 干净、`loopback --bytes 1000000` exit 0（written == read_back == logged == replay == reference == 1000000，display 16/16，逐字节一致）、`send --port NOPE` exit 1（契约要求）、`build -p embhub-app` exit 0；回归 6 条全 0。
- **captain 自加的验证**：① `loopback --bytes 0 / 1 / 1000001`（边界与非整数尺寸，实现者只跑了 1000000）；② 逐一实跑 5 种非法 `send` 输入，确认错误原因**具体到可操作**（"缺少 --data（不允许静默发送空数据）"这类），不是笼统失败；③ **测试名差集由 captain 独立重做**（上轮 50+17 逐条在新 63+27 中查到，零删除零改名，新增 23 条）。
- 代码级复核：`RecordingSink` 单测断言的是**确切字节与 flush 次数**（`AT\r\nAT\r\nAT\r\n`、12 字节、flush 3 次），不是"调用成功即通过"；失败路径的错误串带「第几次 / 已发多少字节 / 底层原因」，对 GUI 诊断友好。

### captain 明确背书的一处克制行为（值得记下）

实现者主动声明：**没有对任何真实串口写入**。本机存在 COM3..COM12，向未知设备写字节属硬件副作用（可能复位设备、使其进入 bootloader 等），未经许可不做。

**captain 背书并维持该约束。** 因此 `send` 的成功路径由「回环端到端（经 Session）」+「RecordingSink 单测」覆盖；**真实物理端口的发送未验证，是设计上的有意保留**，已转为用户侧验收项（GUI 接上后在真机点一次发送，看板子是否回帧）。

### 流程：困扰四轮的"评审任务被抢"根因找到了

前四轮（t2/t4/t6/t8）评审任务建好后都被调度器**立即派给空闲的 coder-ds**，captain 每次都得中断它再收回。本轮 **t9 完成时 t10 立刻就绪并占住了 coder-ds**，调度器没有空闲成员可抢——t11（评审）顺利直接归 captain。

**根因**：共享池任务会被**空闲成员**立即抢单。
**解法**：让成员保持有事做（任务排队），或任务一就绪就立刻 reassign。此前"提前建 + 加依赖"之所以无效，是因为任务一旦变可就绪就会被抢。

### 下一步

`t10`（GUI 串口助手面板）进行中。契约里已把适配层边界钉死：4 个命令（`open_session`/`close_session`/`session_stats`/`send_data`）+ 1 个事件（`session-batch`，载荷 `{seq, t_mono, hex}`），且 `crates/embhub-app/` 是 t10 唯一可改范围——core 与 CLI 一行不许动。

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
