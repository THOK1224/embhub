import io

p = r'C:\Users\24928\Documents\deepseek-harness\Integrated Tool\docs\research\2026-10-14-embedded-tooling-landscape.md'
s = io.open(p, encoding='utf-8').read()

rows = s.split('\n')
DATES = {
    'platformio/platformio-core': '2026-09-23',
    'platformio/platformio-vscode-ide': '2025-01-11',
    'espressif/esp-idf': '2026-10-12',
    'espressif/vscode-esp-idf-extension': '2026-10-14',
    'arduino/arduino-ide': '2026-10-14',
    'arduino/arduino-cli': '2026-10-12',
    'eclipse-embed-cdt/eclipse-plugins': '2026-09-09',
    'eclipse-cdt/cdt': '2026-10-13',
    'probe-rs/probe-rs': '2026-10-14',
    'jupyterlab/jupyterlab': '2026-10-14',
    'lapce/lapce': '2026-10-09',
    'zed-industries/zed': '2026-10-14',
}
n = 0
for i, ln in enumerate(rows):
    if '见 API 记录' in ln and ln.strip().startswith('|'):
        for key, d in DATES.items():
            if key in ln:
                rows[i] = ln.replace('见 API 记录', d)
                n += 1
                break
        else:
            print('NO DATE for row:', ln[:80])
# 修 Zed 行被放错列的日期
rows = [r.replace('| 91,706 | 2026-10-14（GPL-3.0/AGPL 等混合，未逐一核实） |',
                  '| 91,706 | 见 D 节（GPL-3.0/AGPL 等混合，未逐一核实） |') for r in rows]
s = '\n'.join(rows)

# 删除已过时的 2.2 节注释
s = s.replace(
    '> 注：上表标注「见 API 记录」的 6 个仓库，其 star/license/pushed_at 已按同一口径采集并保存在工作区原始数据中，但在整理本报告时未逐条回填表格，**为避免口径不一致，这 6 项在本表中不作为结论引用**；引用请以对应 API 端点 `https://api.github.com/repos/OWNER/REPO` 实时值为准（见 D 节）。\n\n',
    '> 注：Tabby / WindTerm / Electerm / WezTerm / Alacritty / Warp / Hyper / MobaXterm 中，除 MobaXterm（闭源，无 GitHub 仓库）外，star 与 `pushed_at` 均于 2026-10-14 取自 `https://api.github.com/repos/OWNER/REPO` 与 Search API。\n\n')

io.open(p, 'w', encoding='utf-8').write(s)
print('date cells filled', n, '| remaining 见 API 记录:', s.count('见 API 记录'))
