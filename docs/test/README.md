# stm32g474-test

STM32G474VET6 上的 **USART1 串口助手测试固件**。协议刻意做成最简：

- **上位机 → 板子**：`AA 55 | CMD | 负载 | FF` —— 没有长度字段、没有序号、没有 CRC
- **板子 → 上位机**：**普通文本行**（每条以 CR LF 结尾），串口助手按文本看就行
- 覆盖：连通性、回环回显、芯片信息、收发统计、三个按键（PE12/PE13/PE14）按下即上报、
  周期心跳、连续发送（BURST）

- **硬件**：STM32G474VET6（LQFP100，512 KB Flash / 128 KB RAM）
- **串口**：USART1，`PB6 = TX` / `PB7 = RX`，**115200 8N1**，收发全走 DMA
- **按键**：`KEY0 = PE12`、`KEY1 = PE13`、`KEY2 = PE14`，直连 GND（内部上拉，按下为低），
  20 ms 消抖 + 1 s 长按判定
- **上位机**：任意串口助手（HEX 发送 / 文本接收），或 `tools/uart_proto.py`

完整的帧规则、命令表、逐字节示例、验收步骤和排错表见 **[docs/PROTOCOL.md](docs/PROTOCOL.md)**。

## 快速开始

```powershell
# 编译（Ninja 和 arm-none-eabi-gcc 需要在 PATH 里）
cmake --preset Debug
cmake --build --preset Debug

# 用 ST-Link + OpenOCD 烧录（烧完自动校验并复位）
cmake --build --preset flash
```

产物在 `build/Debug/`：`stm32g474-test.elf` / `.hex` / `.bin` / `.map`。

接好 USB-TTL（PB6→转换器 RXD，PB7→转换器 TXD，共地），串口助手开 **115200 8N1 无流控**，
复位板子先看到 5 行 `[BOOT]` 文本；发送区切 **HEX**，发 `AA 55 01 FF` 就会回 `PONG fw=2.0.0 t=...ms`；
直接按板上的 KEY0/KEY1/KEY2，接收区会实时出现 `KEY0 DOWN ...` 这样的文本行。

## 目录

```
Core/Inc|Src/proto_frame.[ch]   接收帧解析：AA 55 CMD [payload] FF（不依赖 HAL）
Core/Inc|Src/serial_link.[ch]   USART1 链路：循环 DMA + 空闲中断收、环形缓冲、DMA 发、统计
Core/Inc|Src/proto_app.[ch]     命令分发 + 文本回复 + 按键/心跳/BURST 调度
Core/Inc|Src/keys.[ch]          KEY0/KEY1/KEY2 (PE12/PE13/PE14)：1 kHz 采样、消抖、长按、事件队列
Core/Src/main.c                 只加了两处 USER CODE：ProtoApp_Init() / ProtoApp_Poll()
Core/Src/stm32g4xx_it.c         只在 SysTick_Handler 的 USER CODE 块里加了 Keys_Scan()
tools/uart_proto.py             上位机参考实现：组帧、示例帧、自检、串口收发
docs/PROTOCOL.md                协议与使用说明（主文档）
CMakeLists.txt                  用户源码 + .hex/.bin 生成 + OpenOCD 烧录目标
CMakePresets.json               Debug / Release 配置预设，flash / erase / size / debug-server 构建预设
LOG.md                          每一轮施工记录与决策原因（按新约定应移到 docs/，见 LOG 里的说明）
```

CubeMX 生成的 `usart.c` / `dma.c` / `gpio.c` 未做任何修改。

## CMake 目标一览

| 目标 | 作用 |
| --- | --- |
| `cmake --build --preset Debug` / `Release` | 编译并生成 `.hex`/`.bin`、打印体积 |
| `flash` / `flash-bin` | OpenOCD 烧录 `.elf` / `.bin` 到 `0x08000000`，带校验与复位 |
| `erase` | 整片擦除 |
| `reset` | 复位运行 |
| `size` | 显示固件体积 |
| `debug-server` | 前台启动 OpenOCD GDB 服务（:3333） |

可覆盖的 cache 变量：`OPENOCD_EXECUTABLE`、`OPENOCD_INTERFACE`（默认 `interface/stlink.cfg`）、
`OPENOCD_TARGET_CFG`（默认 `target/stm32g4x.cfg`）、`OPENOCD_ADAPTER_SPEED`（默认 4000 kHz）。

```powershell
cmake --preset Debug -DOPENOCD_INTERFACE=interface/cmsis-dap.cfg
cmake --build --preset flash
```
