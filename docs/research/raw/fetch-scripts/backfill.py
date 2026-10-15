import io
import re

p = r'C:\Users\24928\Documents\deepseek-harness\Integrated Tool\docs\research\2026-10-14-embedded-tooling-landscape.md'
s = io.open(p, encoding='utf-8').read()

UNV = '见 API 记录'

REPL = [
    # 2.1
    ('| open-serial-port-monitor | [gthvidsten/open-serial-port-monitor](https://github.com/gthvidsten/open-serial-port-monitor) | 186 |',
     '| open-serial-port-monitor | [gthvidsten/open-serial-port-monitor](https://github.com/gthvidsten/open-serial-port-monitor) | 186 |'),
    # 2.2 rows
    ('| Tabby | [Eugeny/tabby](https://github.com/Eugeny/tabby) | 见下注 | 见下注 |',
     '| Tabby | [Eugeny/tabby](https://github.com/Eugeny/tabby) | 74,966 | MIT |'),
    ('| Electerm | [electerm/electerm](https://github.com/electerm/electerm) | 见 API 记录 | 见 API 记录 |',
     '| Electerm | [electerm/electerm](https://github.com/electerm/electerm) | 15,354 | MIT |'),
    ('| Alacritty | [alacritty/alacritty](https://github.com/alacritty/alacritty) | 见 API 记录 | Apache-2.0（仓库 LICENSE） |',
     '| Alacritty | [alacritty/alacritty](https://github.com/alacritty/alacritty) | 65,960 | Apache-2.0（仓库 LICENSE） |'),
    ('| Warp | [warpdotdev/Warp](https://github.com/warpdotdev/Warp) | 见 API 记录 |',
     '| Warp | [warpdotdev/Warp](https://github.com/warpdotdev/Warp) | 65,452 |'),
    ('| Hyper | [vercel/hyper](https://github.com/vercel/hyper) | 见 API 记录 | MIT |',
     '| Hyper | [vercel/hyper](https://github.com/vercel/hyper) | 44,735 | MIT |'),
    # Tabby 行的最近提交列
    ('插件化 + 主题/配色 | 见下注 |', '插件化 + 主题/配色 | 2026-10-12 |'),
    ('SSH/SFTP/Telnet/串口 + 本地终端 + 书签同步 | 见 API 记录 |', 'SSH/SFTP/Telnet/串口 + 本地终端 + 书签同步 | 2026-10-14 |'),
    ('故意不做 tab/分屏，交给 tmux 类复用器 | 见 API 记录 |', '故意不做 tab/分屏，交给 tmux 类复用器 | 2026-10-05 |'),
    ('AI 命令辅助等；功能清单未逐一核实） | 见 API 记录 |', 'AI 命令辅助等；功能清单未逐一核实） | 2026-10-14 |'),
    ('**维护活跃度弱，仅作历史参照**） | 见 API 记录 |', '**维护活跃度弱，仅作历史参照**） | 2026-08-21 |'),
    ('端（**维护活跃度弱，仅作历史参照**） | 见 API 记录 |', '端（**维护活跃度弱，仅作历史参照**） | 2026-08-21 |'),
    # 2.3
    ('| PlatformIO Core | [platformio/platformio-core](https://github.com/platformio/platformio-core) | 见 API 记录 |',
     '| PlatformIO Core | [platformio/platformio-core](https://github.com/platformio/platformio-core) | 9,527 |'),
    ('| PlatformIO VSCode 扩展 | [platformio/platformio-vscode-ide](https://github.com/platformio/platformio-vscode-ide) | 见 API 记录 | 见 API 记录 |',
     '| PlatformIO VSCode 扩展 | [platformio/platformio-vscode-ide](https://github.com/platformio/platformio-vscode-ide) | 1,462 | Apache-2.0 |'),
    ('| ESP-IDF | [espressif/esp-idf](https://github.com/espressif/esp-idf) | 见 API 记录 |',
     '| ESP-IDF | [espressif/esp-idf](https://github.com/espressif/esp-idf) | 19,241 |'),
    ('| ESP-IDF VSCode 扩展 | [espressif/vscode-esp-idf-extension](https://github.com/espressif/vscode-esp-idf-extension) | 见 API 记录 |',
     '| ESP-IDF VSCode 扩展 | [espressif/vscode-esp-idf-extension](https://github.com/espressif/vscode-esp-idf-extension) | 1,472 |'),
    ('| Arduino IDE 2.x | [arduino/arduino-ide](https://github.com/arduino/arduino-ide) | 见 API 记录 |',
     '| Arduino IDE 2.x | [arduino/arduino-ide](https://github.com/arduino/arduino-ide) | 3,299 |'),
    ('| arduino-cli | [arduino/arduino-cli](https://github.com/arduino/arduino-cli) | 见 API 记录 |',
     '| arduino-cli | [arduino/arduino-cli](https://github.com/arduino/arduino-cli) | 5,059 |'),
    ('| Eclipse Embedded CDT | [eclipse-embed-cdt/eclipse-plugins](https://github.com/eclipse-embed-cdt/eclipse-plugins) | 见 API 记录 |',
     '| Eclipse Embedded CDT | [eclipse-embed-cdt/eclipse-plugins](https://github.com/eclipse-embed-cdt/eclipse-plugins) | 559 |'),
    ('| Eclipse CDT | [eclipse-cdt/cdt](https://github.com/eclipse-cdt/cdt) | 见 API 记录 |',
     '| Eclipse CDT | [eclipse-cdt/cdt](https://github.com/eclipse-cdt/cdt) | 375 |'),
    ('| probe-rs | [probe-rs/probe-rs](https://github.com/probe-rs/probe-rs) | 见 API 记录 |',
     '| probe-rs | [probe-rs/probe-rs](https://github.com/probe-rs/probe-rs) | 2,974 |'),
    # 2.4
    ('| JupyterLab | [jupyterlab/jupyterlab](https://github.com/jupyterlab/jupyterlab) | 见 API 记录 | BSD-3-Clause |',
     '| JupyterLab | [jupyterlab/jupyterlab](https://github.com/jupyterlab/jupyterlab) | 15,361 | BSD-3-Clause |'),
    ('| Lapce | [lapce/lapce](https://github.com/lapce/lapce) | 见 API 记录 | Apache-2.0 |',
     '| Lapce | [lapce/lapce](https://github.com/lapce/lapce) | 38,921 | Apache-2.0 |'),
    ('| Zed | [zed-industries/zed](https://github.com/zed-industries/zed) | 见 API 记录 |',
     '| Zed | [zed-industries/zed](https://github.com/zed-industries/zed) | 91,706 |'),
    # 3.1
    ('| serialport-rs | [serialport/serialport-rs](https://github.com/serialport/serialport-rs) | 见 API 记录 |',
     '| serialport-rs | [serialport/serialport-rs](https://github.com/serialport/serialport-rs) | 754 |'),
    ('| node-serialport | [serialport/node-serialport](https://github.com/serialport/node-serialport) | 见 API 记录 |',
     '| node-serialport | [serialport/node-serialport](https://github.com/serialport/node-serialport) | 5,994 |'),
    ('| pyserial | [pyserial/pyserial](https://github.com/pyserial/pyserial) | 见 API 记录 |',
     '| pyserial | [pyserial/pyserial](https://github.com/pyserial/pyserial) | 3,579 |'),
    ('| go.bug.st/serial | [bugst/go-serial](https://github.com/bugst/go-serial) | 见 API 记录 |',
     '| go.bug.st/serial | [bugst/go-serial](https://github.com/bugst/go-serial) | 876 |'),
    # 3.2
    ('| russh | [Eugeny/russh](https://github.com/Eugeny/russh) | 见 API 记录 |',
     '| russh | [Eugeny/russh](https://github.com/Eugeny/russh) | 1,901 |'),
    ('| libssh2 | [libssh2/libssh2](https://github.com/libssh2/libssh2) | 见 API 记录 |',
     '| libssh2 | [libssh2/libssh2](https://github.com/libssh2/libssh2) | 1,593 |'),
    ('| ssh2 (Node) | [mscdex/ssh2](https://github.com/mscdex/ssh2) | 见 API 记录 |',
     '| ssh2 (Node) | [mscdex/ssh2](https://github.com/mscdex/ssh2) | 5,824 |'),
    ('| golang.org/x/crypto/ssh | [golang/crypto](https://github.com/golang/crypto) | 见 API 记录 |',
     '| golang.org/x/crypto/ssh | [golang/crypto](https://github.com/golang/crypto) | 3,351 |'),
    ('| Paramiko | [paramiko/paramiko](https://github.com/paramiko/paramiko) | 见 API 记录 |',
     '| Paramiko | [paramiko/paramiko](https://github.com/paramiko/paramiko) | 9,879 |'),
    # 3.3
    ('| xterm.js | [xtermjs/xterm.js](https://github.com/xtermjs/xterm.js) | 见 API 记录 |',
     '| xterm.js | [xtermjs/xterm.js](https://github.com/xtermjs/xterm.js) | 21,269 |'),
    ('| node-pty | [microsoft/node-pty](https://github.com/microsoft/node-pty) | 见 API 记录 |',
     '| node-pty | [microsoft/node-pty](https://github.com/microsoft/node-pty) | 2,046 |'),
    # 3.4
    ('| libgit2 | [libgit2/libgit2](https://github.com/libgit2/libgit2) | 见 API 记录 |',
     '| libgit2 | [libgit2/libgit2](https://github.com/libgit2/libgit2) | 10,624 |'),
    ('| git2-rs | [rust-lang/git2-rs](https://github.com/rust-lang/git2-rs) | 见 API 记录 |',
     '| git2-rs | [rust-lang/git2-rs](https://github.com/rust-lang/git2-rs) | 2,114 |'),
    ('| gitoxide (gix) | [GitoxideLabs/gitoxide](https://github.com/GitoxideLabs/gitoxide) | 见 API 记录 |',
     '| gitoxide (gix) | [GitoxideLabs/gitoxide](https://github.com/GitoxideLabs/gitoxide) | 12,012 |'),
    ('| simple-git | [steveukx/git-js](https://github.com/steveukx/git-js) | 见 API 记录 |',
     '| simple-git | [steveukx/git-js](https://github.com/steveukx/git-js) | 3,857 |'),
    ('| GitPython | [gitpython-developers/GitPython](https://github.com/gitpython-developers/GitPython) | 见 API 记录 |',
     '| GitPython | [gitpython-developers/GitPython](https://github.com/gitpython-developers/GitPython) | 5,185 |'),
    ('| go-git | [go-git/go-git](https://github.com/go-git/go-git) | 见 API 记录 |',
     '| go-git | [go-git/go-git](https://github.com/go-git/go-git) | 7,798 |'),
    # 3.5
    ('| uPlot | [leeoniya/uPlot](https://github.com/leeoniya/uPlot) | 见 API 记录 |',
     '| uPlot | [leeoniya/uPlot](https://github.com/leeoniya/uPlot) | 10,557 |'),
    ('| ECharts | [apache/echarts](https://github.com/apache/echarts) | 见 API 记录 |',
     '| ECharts | [apache/echarts](https://github.com/apache/echarts) | 67,508 |'),
    ('| egui / egui_plot | [emilk/egui](https://github.com/emilk/egui) | 见 API 记录 |',
     '| egui / egui_plot | [emilk/egui](https://github.com/emilk/egui) | 31,422 |'),
    ('| Plotly.py | [plotly/plotly.py](https://github.com/plotly/plotly.py) | 见 API 记录 |',
     '| Plotly.py | [plotly/plotly.py](https://github.com/plotly/plotly.py) | 18,840 |'),
    # 3.6
    ('| Tauri v2 | [tauri-apps/tauri](https://github.com/tauri-apps/tauri) | 见 API 记录 |',
     '| Tauri v2 | [tauri-apps/tauri](https://github.com/tauri-apps/tauri) | 111,887 |'),
    ('| Electron | [electron/electron](https://github.com/electron/electron) | 见 API 记录 |',
     '| Electron | [electron/electron](https://github.com/electron/electron) | 123,317 |'),
    ('| Wails | [wailsapp/wails](https://github.com/wailsapp/wails) | 见 API 记录 |',
     '| Wails | [wailsapp/wails](https://github.com/wailsapp/wails) | 36,548 |'),
    ('| Avalonia | [AvaloniaUI/Avalonia](https://github.com/AvaloniaUI/Avalonia) | 见 API 记录 |',
     '| Avalonia | [AvaloniaUI/Avalonia](https://github.com/AvaloniaUI/Avalonia) | 31,661 |'),
    ('| egui | [emilk/egui](https://github.com/emilk/egui) | 见 API 记录 |',
     '| egui | [emilk/egui](https://github.com/emilk/egui) | 31,422 |'),
    ('| iced | [iced-rs/iced](https://github.com/iced-rs/iced) | 见 API 记录 |',
     '| iced | [iced-rs/iced](https://github.com/iced-rs/iced) | 31,758 |'),
]

n = 0
for old, new in REPL:
    if old in s and old != new:
        s = s.replace(old, new)
        n += 1
    elif old not in s:
        print('NOT FOUND:', old[:90])

io.open(p, 'w', encoding='utf-8').write(s)
print('replaced', n)
print('remaining placeholders:', s.count(UNV))
for m in re.finditer(r'.{60}见 API 记录.{20}', s):
    print('  ...', m.group(0).replace('\n', ' '))
