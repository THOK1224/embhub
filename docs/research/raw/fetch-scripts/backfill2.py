import io
import re

p = r'C:\Users\24928\Documents\deepseek-harness\Integrated Tool\docs\research\2026-10-14-embedded-tooling-landscape.md'
s = io.open(p, encoding='utf-8').read()

# (anchor substring that identifies the row, old cell text, new date)
ROWS = [
    ('zed-industries/zed', '见 API 记录（GPL-3.0/AGPL 等混合', '2026-10-14（GPL-3.0/AGPL 等混合'),
    ('serialport/serialport-rs', '4.10.2-alpha0） | Rust | 见 API 记录 |', '4.10.2-alpha0） | Rust | 2026-09-08 |'),
    ('serialport/node-serialport', '`@serialport/bindings-cpp`） | 见 API 记录 |', '`@serialport/bindings-cpp`） | 2026-01-17 |'),
    ('pyserial/pyserial', '实测） | Python | 见 API 记录 |', '实测） | Python | 2026-05-19 |'),
    ('bugst/go-serial', 'Go 1.25） | 见 API 记录 |', 'Go 1.25） | 2026-07-15 |'),
    ('Eugeny/russh', '依赖 Tokio） | 见 API 记录 |', '依赖 Tokio） | 2026-10-05 |'),
    ('libssh2/libssh2', '| C | 见 API 记录 |', '| C | 2026-10-14 |'),
    ('mscdex/ssh2', '纯 JS 无原生依赖） | 见 API 记录 |', '纯 JS 无原生依赖） | 2026-08-20 |'),
    ('golang/crypto', '`go.mod: module golang.org/x/crypto`） | 见 API 记录 |', '`go.mod: module golang.org/x/crypto`） | 2026-10-11 |'),
    ('paramiko/paramiko', '版本 5.0.0） | Python | 见 API 记录 |', '版本 5.0.0） | Python | 2026-10-10 |'),
    ('xtermjs/xterm.js', '161 文件，实测） | 见 API 记录 |', '161 文件，实测） | 2026-10-13 |'),
    ('microsoft/node-pty', '61.4 MB**，实测） | 见 API 记录 |', '61.4 MB**，实测） | 2026-10-03 |'),
    ('libgit2/libgit2', '**许可未逐字核实**） | C | 见 API 记录 |', '**许可未逐字核实**） | C | 2026-10-14 |'),
    ('rust-lang/git2-rs', 'MSRV 1.87） | Rust | 见 API 记录 |', 'MSRV 1.87） | Rust | 2026-10-05 |'),
    ('GitoxideLabs/gitoxide', 'MSRV 1.88） | Rust | 见 API 记录 |', 'MSRV 1.88） | Rust | 2026-10-14 |'),
    ('steveukx/git-js', '`name = @simple-git/repo`） | 见 API 记录 |', '`name = @simple-git/repo`） | 2026-09-26 |'),
    ('gitpython-developers/GitPython', '`gitdb>=4.0.1`） | 见 API 记录 |', '`gitdb>=4.0.1`） | 2026-10-14 |'),
    ('go-git/go-git', '`go.mod: module github.com/go-git/go-git/v6`） | 见 API 记录 |', '`go.mod: module github.com/go-git/go-git/v6`） | 2026-10-14 |'),
    ('leeoniya/uPlot', 'unpacked 0.5 MB，实测） | 见 API 记录 |', 'unpacked 0.5 MB，实测） | 2026-10-11 |'),
    ('apache/echarts', '57.5 MB**，实测） | 见 API 记录 |', '57.5 MB**，实测） | 2026-10-13 |'),
    ('emilk/egui', 'MSRV 1.95） | Rust | 见 API 记录 |', 'MSRV 1.95） | Rust | 2026-10-14 |'),
    ('plotly/plotly.py', '实测，7.1.0） | Python | 见 API 记录 |', '实测，7.1.0） | Python | 2026-10-09 |'),
]

# Tauri/Electron/Wails/Avalonia/iced 行的单元格含「未核实」，无需改
n = 0
for anchor, old, new in ROWS:
    if old not in s:
        print('MISS cell:', old[:60])
        continue
    # 找到包含 anchor 且包含 old 的行，逐个替换（每行只出现一次）
    lines = s.split('\n')
    hit = False
    for i, ln in enumerate(lines):
        if anchor in ln and old in ln:
            lines[i] = ln.replace(old, new)
            hit = True
            break
    if not hit:
        print('MISS row:', anchor)
    else:
        n += 1
    s = '\n'.join(lines)

io.open(p, 'w', encoding='utf-8').write(s)
print('updated rows', n)
print('remaining 见 API 记录:', s.count('见 API 记录'))
for m in re.finditer(r'.{50}见 API 记录.{30}', s):
    print('  ...', m.group(0).replace('\n', ' '))
