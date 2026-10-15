# USART1 串口助手测试程序 —— 协议与使用说明

板子：**STM32G474VET6**（LQFP100，512 KB Flash / 128 KB RAM）
串口：**USART1**，`PB6 = TX`、`PB7 = RX`，**115200 8N1**，收发全部走 **DMA**
固件版本：`2.0.0`（2.x = 极简协议：无 CRC、无序号、板子回复为纯文本）

> **协议极简到什么程度**
> - 上位机 → 板子：`AA 55` + `CMD` + 负载 + `FF`。**没有长度字段、没有序号、没有 CRC。**
> - 板子 → 上位机：**不是帧，就是普通文本行**（每条以 CR LF 结尾），串口助手按文本看即可。

---

## 1. 硬件与 CubeMX 配置现状

| 项目 | 配置 | 出处 |
| --- | --- | --- |
| MCU | STM32G474VET6，LQFP100 | `stm32g474-test.ioc` |
| 时钟 | HSE 8 MHz → PLL → **SYSCLK 170 MHz**，PCLK2 = 170 MHz | `main.c` `SystemClock_Config()` |
| USART1 | 异步，115200 8N1，无校验/无流控，时钟源 PCLK2 | `usart.c` `MX_USART1_UART_Init()` |
| USART1 引脚 | `PB6` = USART1_TX，`PB7` = USART1_RX（AF7） | `usart.c` |
| TX DMA | `DMA1_Channel1`，请求 `USART1_TX` | `usart.c` |
| RX DMA | `DMA1_Channel2`，请求 `USART1_RX`（固件里改成循环模式） | `usart.c` + `serial_link.c` |
| 中断 | `USART1_IRQn`、`DMA1_Channel1/2_IRQn` 抢占 0 / 子 0，分组 4；SysTick 优先级 15 | `usart.c` / `dma.c` / `.ioc` |
| 按键 | `KEY0 = PE12`、`KEY1 = PE13`、`KEY2 = PE14`，**另一端直连 GND**，无外部上拉 → 固件开 **内部上拉**，按下为低 | 原理图 + `keys.c` |

接线（USB-TTL 转换器，**3.3 V TTL 电平，不要接 RS-232 电平**）：

```
板子 PB6 (TX) ----> 转换器 RXD
板子 PB7 (RX) <---- 转换器 TXD
板子 GND      ----  转换器 GND
```

> 如果用的是 Nucleo 板载 ST-Link 的“虚拟串口”，它引到的是别的 USART/LPUART，不是 PB6/PB7，请用独立 USB-TTL。

按键部分 CubeMX 里没有配，`keys.c` 自己使能 GPIOE 时钟并把 PE12/PE13/PE14 配成输入 + 上拉。
**以后若在 CubeMX 里也配这三个脚，Pull 必须选 Pull-up**，否则悬空会乱触发。

---

## 2. 上位机 → 板子的帧

```
AA 55 | CMD | PAYLOAD... | FF
|帧头 | 命令 |   负载      |帧尾
```

| 项 | 规则 |
| --- | --- |
| 帧头 | 固定 `AA 55` |
| 命令 | 1 字节，见 §3 |
| 负载 | 可空。**长度由帧尾决定**，所以负载里不能出现 `0xFF` |
| 帧尾 | 固定 `FF` |
| 负载上限 | **64 字节**，超过则整帧丢弃并回一行错误 |
| 帧间噪声 | 无所谓，噪声不会匹配 `AA 55`，会被直接忽略 |
| 无 | 长度字段、序号、CRC —— 都不需要 |

`AA AA 55 ...` 这种重复帧头能正确重同步（第二个 `AA` 当成新的帧头）。

---

## 3. 命令表

回复全部是**文本行**（以 `CR LF` 结尾）。多字节参数一律**小端**。

### 3.1 命令

| CMD | 名称 | 请求负载 | 板子回复（文本行） |
| --- | --- | --- | --- |
| `0x01` | PING | 无 | `PONG fw=2.0.0 t=12345ms` |
| `0x02` | ECHO | 任意 ≤64 字节（不含 `FF`） | 原样回显这些字节，末尾补 `CR LF`；空负载回 `ECHO empty` |
| `0x03` | INFO | 无 | `INFO devid=0x0469 revid=0x2003 sysclk=170000000Hz uid=<24位hex> t=12345ms rst=0x04000000` |
| `0x04` | STATS | 无 | `STATS rx=3 badlen=0 unk=0 txlines=5 ring=0 uart=0 tmo=0 keydrop=0` |
| `0x05` | CLEAR | 无 | `STATS CLEARED` |
| `0x06` | KEYS | 无 | `KEYS mask=0x00 KEY0=1 KEY1=0 KEY2=0` |
| `0x07` | HEARTBEAT | `[间隔毫秒 u16]` | `HEARTBEAT every 1000ms` 或 `HEARTBEAT off` |
| `0x08` | BURST | `[行数 u16][每行字符数 u16]` | `BURST START count=10 size=16`，随后 10 行 `BURST <i> <字符>`，最后 `BURST DONE` |

字段说明：

- **INFO**：`devid`/`revid` 来自 `DBGMCU`；`sysclk` 是 `HAL_RCC_GetSysClockFreq()`；
  `uid` 是芯片 96 位唯一 ID（`0x1FFF7590` 起，打印成 `UIDw0 UIDw1 UIDw2` 三个 32 位 hex）；
  `t` 是开机毫秒；`rst` 是上次复位标志（`RCC_CSR` 的 bit25..31）。
- **STATS**：`rx` 合法帧数、`badlen` 负载超限被丢的帧数、`unk` 命令字不认识的帧数、
  `txlines` 已发文本行数、`ring` RX 环满丢弃字节数、`uart` UART 错误回调次数、
  `tmo` 发送等待超时次数、`keydrop` 按键事件队列满丢弃数。
- **HEARTBEAT**：`0` = 关闭；否则 10..60000 ms。开启后周期发 `TICK <计数> t=<毫秒>ms`。
- **BURST**：行数 1..1000，每行字符数 0..64。用于测连续收发/显示。中途发新命令不会打断本轮，
  但再发一条 BURST 会重置参数。

### 3.2 错误回复（同样是文本行）

| 情况 | 回复 |
| --- | --- |
| 命令字不认识 | `ERR unknown cmd 0x99` |
| 负载长度不对（如 HEARTBEAT 给了 3 字节） | `ERR bad payload len=3 (HEARTBEAT needs 2 bytes)` |
| 参数越界（如心跳间隔 5 ms） | `ERR bad param 5 (0 or 10..60000 ms)` |
| 负载一直不给帧尾、超过 64 字节 | `ERR frame too long (payload max 64)` |

### 3.3 板子主动上报（无需请求，全是文本行）

按键状态一变就自动发一行：

```
KEY0 DOWN count=1 t=1234ms      # 按下（已消抖）
KEY0 UP count=1 hold=244ms      # 松开，hold = 本次按住时长
KEY0 LONG count=1 t=2234ms      # 按住超过 1000 ms（一次按住只报一次）
```

心跳：`TICK 1 t=1234ms`

按键行为：**20 ms 消抖**（一次物理按键只产生一对 DOWN/UP）、**长按 1 s** 补一条 LONG、
扫描在 SysTick 中断里以 1 kHz 运行（主循环在做长时间 BURST 发送时也不会漏按键）。

---

## 4. 在串口助手上怎么操作

1. 打开串口：**115200 / 8 / N / 1**，无流控。
2. 打开串口后按一下板子复位，应看到开机横幅（纯文本）：

   ```
   [BOOT] stm32g474-test fw 2.0.0  USART1 115200 8N1 + DMA  (PB6=TX PB7=RX)
   [BOOT] send frame: AA 55 <CMD> [payload] FF   (payload must NOT contain 0xFF)
   [BOOT] reply is plain text, one line per message, CR LF terminated
   [BOOT] cmds: 01 PING 02 ECHO 03 INFO 04 STATS 05 CLEAR 06 KEYS 07 HEARTBEAT 08 BURST
   [BOOT] keys: KEY0=PE12 KEY1=PE13 KEY2=PE14, active low, press -> text line
   ```

3. 接收区用**文本/ASCII**方式；发送区切 **HEX 发送**，粘下面的帧即可：

| 操作 | 要粘贴的十六进制 |
| --- | --- |
| PING | `AA 55 01 FF` |
| ECHO "Hello" | `AA 55 02 48 65 6C 6C 6F FF` |
| INFO | `AA 55 03 FF` |
| STATS | `AA 55 04 FF` |
| CLEAR | `AA 55 05 FF` |
| KEYS | `AA 55 06 FF` |
| 心跳开 1000 ms | `AA 55 07 E8 03 FF` |
| 心跳关 | `AA 55 07 00 00 FF` |
| BURST 10 行 × 16 字符 | `AA 55 08 0A 00 10 00 FF` |

> 注意：发送时**不要勾“加回车换行”**（那会多发 `0D 0A`，虽然会被当噪声忽略，但没必要）。
> 帧尾就是最后一个 `FF`，不多不少。

4. 可选命令行工具（离线生成帧 / 直接收发）：

```powershell
python tools\uart_proto.py selftest        # 帧编解码自检（不需要硬件）
python tools\uart_proto.py frames          # 打印上面那张表
python tools\uart_proto.py send COM8 ping  # 需要 pip install pyserial
python tools\uart_proto.py listen COM8     # 持续打印板子的文本行
```

---

## 5. 验收步骤

| # | 操作 | 期望结果 |
| --- | --- | --- |
| 1 | 打开串口，复位板子 | 收到 5 行 `[BOOT]` 文本 |
| 2 | 发 `AA 55 01 FF` | 收到 `PONG fw=2.0.0 t=<数字>ms`，连发几次 `t` 递增 |
| 3 | 发 `AA 55 02 48 65 6C 6C 6F FF` | 收到 `Hello` |
| 4 | 发 `AA 55 02 FF`（空负载回显） | 收到 `ECHO empty` |
| 5 | 发 `AA 55 03 FF` | 收到 `INFO ...`，`devid=0x0469`、`sysclk=170000000Hz` |
| 6 | 发 `AA 55 04 FF` → `AA 55 05 FF` → 再发 `AA 55 04 FF` | 第二次统计里 `rx` 等计数从 0 重新累计 |
| 7 | 发 `AA 55 07 E8 03 FF` 后静置 5 s | 先收到 `HEARTBEAT every 1000ms`，随后约 5 行 `TICK n t=...ms`，`n` 连续递增 |
| 8 | 发 `AA 55 07 00 00 FF` | 收到 `HEARTBEAT off`，之后不再有 `TICK` |
| 9 | 发 `AA 55 08 0A 00 10 00 FF` | 收到 `BURST START count=10 size=16` + 10 行 `BURST i <16 字符>`（i 从 0 连续）+ `BURST DONE` |
| 10 | 发 `AA 55 99 FF`（未知命令） | 收到 `ERR unknown cmd 0x99`，且 `STATS` 里 `unk` +1 |
| 11 | 只发 `AA 55 07 FF`（心跳缺参数） | 收到 `ERR bad payload len=0 (HEARTBEAT needs 2 bytes)` |
| 12 | 发 `AA 55 03` 后接 70 个非 `FF` 字节 | 收到 `ERR frame too long (payload max 64)`，之后仍能正常响应 |
| 13 | 发一段纯噪声 `11 22 33` | 无回复；随后发 PING 仍正常（噪声被忽略） |
| 14 | 按一下 KEY0 并松开 | 依次收到 `KEY0 DOWN count=1 t=...ms` 和 `KEY0 UP count=1 hold=...ms` |
| 15 | 快速点 KEY1 十下 | 10 对 DOWN/UP，`count` 从 1 递增到 10，无多余事件 |
| 16 | 按住 KEY2 约 2 秒再松开 | 三条：`KEY2 DOWN` → `KEY2 LONG`（时刻≈按下+1000ms）→ `KEY2 UP`（`hold`≈2000ms） |
| 17 | 三键同按后发 `AA 55 06 FF` | `KEYS mask=0x07`，三个 `count` 都 +1 |
| 18 | BURST 1000 行期间快速点一下 KEY0 | BURST 行收齐后仍能收到 KEY0 的 DOWN/UP 两行 |

---

## 6. 编译与烧录（CMake + OpenOCD）

```powershell
cmake --preset Debug          # 配置
cmake --build --preset Debug  # 编译，自动生成 .hex/.bin 并打印体积
cmake --build --preset flash  # 用 ST-Link + OpenOCD 烧录（烧完 verify + reset）
```

产物在 `build/Debug/`：`stm32g474-test.elf` / `.hex` / `.bin` / `.map`。

底层烧录命令（由 `CMakeLists.txt` 生成，可换调试器/速度）：

```
openocd -f interface/stlink.cfg -f target/stm32g4x.cfg -c "adapter speed 4000" \
        -c "program <elf> verify reset exit"
```

| 目标 | 作用 |
| --- | --- |
| `cmake --build build/Debug --target flash` | 烧 `.elf`（program + verify + reset） |
| `cmake --build build/Debug --target flash-bin` | 烧 `.bin` 到 `0x08000000` |
| `cmake --build build/Debug --target erase` | 整片擦除 |
| `cmake --build build/Debug --target reset` | 复位运行 |
| `cmake --build build/Debug --target size` | 打印体积 |
| `cmake --build --preset debug-server` | 前台起 OpenOCD GDB 服务（:3333） |

可覆盖的 cache 变量：`OPENOCD_EXECUTABLE`、`OPENOCD_INTERFACE`（默认 `interface/stlink.cfg`）、
`OPENOCD_TARGET_CFG`（默认 `target/stm32g4x.cfg`）、`OPENOCD_ADAPTER_SPEED`（默认 4000 kHz）。

```powershell
cmake --preset Debug -DOPENOCD_INTERFACE=interface/cmsis-dap.cfg
cmake --build --preset flash
```

---

## 7. 实现结构

新增 4 个模块。CubeMX 生成的文件里只有 `main.c`（两处 `USER CODE`）和
`stm32g4xx_it.c`（`SysTick_Handler` 里一行 `Keys_Scan()`）被加了用户代码；
`usart.c` / `dma.c` / `gpio.c` **一行没动**。

| 文件 | 职责 | 依赖 |
| --- | --- | --- |
| `Core/Inc/proto_frame.h` + `Core/Src/proto_frame.c` | 接收帧解析：`AA 55 CMD [payload] FF` 的逐字节状态机 | **无 HAL**，可单独编译 |
| `Core/Inc/serial_link.h` + `Core/Src/serial_link.c` | 链路：循环 DMA + 空闲中断收、软件环形缓冲、阻塞式 DMA 发、统计 | HAL |
| `Core/Inc/proto_app.h` + `Core/Src/proto_app.c` | 命令分发 + 文本回复 + 按键/心跳/BURST 调度 | 上两者 + keys |
| `Core/Inc/keys.h` + `Core/Src/keys.c` | 按键：GPIO 初始化、1 kHz 采样、20 ms 消抖、长按、事件队列 | HAL |
| `Core/Src/main.c` | `ProtoApp_Init()` / `ProtoApp_Poll()` | — |
| `tools/uart_proto.py` | 上位机参考实现：组帧、示例帧、自检、串口收发 | Python 3 |

数据流：

```
USART1 RX ──DMA(循环)──> s_rx_dma[256] ──IDLE/HT/TC 回调──> s_rx_ring[1024]（ISR 写 / 主循环读）
   ──Proto_DecoderFeed() 逐字节──> 完整帧 ──handle_command()──> 文本行
   ──SerialLink_Send()（NORMAL DMA + 忙标志，发完才返回）

PE12/13/14 ──SysTick 1kHz──> Keys_Scan() 消抖/长按 ──> 事件队列[8] ──主循环──> 文本行
```

### 关键取舍

1. **接收帧去掉长度/序号/CRC**：负载长度交给固定的 `FF` 帧尾，一帧最长 66 字节。
   代价是负载里不能出现 `FF`（命令参数都是短整数，不受影响）；换来的是
   串口助手按 HEX 手输一帧只要 4~8 个字节，不用算 CRC。
2. **板子发送就是普通文本**：串口助手对“帧头帧尾/CRC/序号”没有处理能力，发文本反而是
   最容易被人和工具消费的形式；`INFO`/`STATS` 这类回复直接可读，按键事件也直接是一行字。
3. **RX 仍是二进制帧**：这样上位机一条命令就是固定几个字节，不会出现"文本协议要敲很久"的问题；
   而且噪声只会被忽略，不需要转义。
4. **RX 用循环 DMA + 空闲中断**：DMA 怎么切包都不影响组帧（见 `serial_link.c` 里的说明）；
   在用户文件里改 DMA 模式，CubeMX 重新生成也不会丢。
5. **按键扫描放在 SysTick 中断**：主循环里的发送是阻塞的，一次 BURST 能占住几百毫秒；
   扫描放中断后才不会漏掉快速按键。
6. **`proto_frame` 与 HAL 解耦**：纯 C 状态机，可以单独编译/在 PC 上验证。

---

## 8. 已知限制

- **负载里不能含 `0xFF`**：它是帧尾。需要传任意二进制时，这条协议不适合（可以改用 `ECHO` 做小数据回环）。
- **发送是阻塞的**：主循环是唯一发送者，`SerialLink_Send()` 等 DMA 发完才返回。
  BURST 期间如果上位机同时猛发数据，RX 环（1024 字节）可能溢出，丢弃字节数在 `STATS` 的 `ring` 里能看到。
- **没有重传/确认**：这是测试协议，不是可靠传输层。
- **没有帧内字节转义**：帧里可能出现任意字节（不过这版板子只回文本，所以线上不会再出现"帧内部有换行"的问题）。
- **无 CRC**：写错一个字节不会有校验报错，只会得到 `ERR unknown cmd` 或参数错误。
- **心跳/BURST 与阻塞发送共享主循环**：BURST 进行中心跳间隔会被拉长。
- **`STATS` 计数不持久**：复位即清零。

---

## 9. 排错

| 现象 | 排查方向 |
| --- | --- |
| 复位后看不到 `[BOOT]` 文本 | 接线（PB6→RXD、PB7→TXD、共地）、波特率、是否用了板载 ST-Link 虚拟串口 |
| 有横幅但发帧没反应 | 发送区是否切到 **HEX 发送**；帧是否以 `FF` 结尾；是否勾了“加回车换行”导致多出字节（会被忽略，不影响） |
| 回 `ERR unknown cmd 0xNN` | 命令字写错，或帧头没对上导致解析错位（正确帧头是 `AA 55`） |
| 回 `ERR frame too long` | 负载超过 64 字节，或者忘了发帧尾 `FF` |
| 回 `ERR bad payload len=N` | 该命令要求的负载长度不对（HEARTBEAT 2 字节、BURST 4 字节） |
| 收不到按键上报 | 不按时是否也没有上报？若一直乱上报，检查 PE12/13/14 是否悬空（必须内部上拉，CubeMX 里若配了这三个脚要选 Pull-up） |
| 编译报 `non constant or forward reference address expression for section .ARM.extab` | 链接脚本里的 `(READONLY)` 需要 GCC 11+；本工程已按脚本自带注释移除 |
