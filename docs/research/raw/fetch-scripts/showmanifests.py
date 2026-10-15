import json
import re
import sys

KEEP = re.compile(
    r'(tauri|electron|react|vue|svelte|angular|vite|webpack|xterm|node-pty|ssh2|serialport|'
    r'libgit2|git2|gitoxide|gix|uplot|echarts|plotly|egui|iced|qt|pyside|gtk|glfw|wgpu|'
    r'tokio|serde|clap|winit|russh|libssh|openssl|fltk|sdl|boost|cmake|esp-idf|avr|arm)',
    re.I)


def pick(text, keys):
    out = {}
    for k in keys:
        m = re.search(r'^%s\s*[:=]\s*(.+)$' % re.escape(k), text, re.M)
        if m:
            out[k] = m.group(1).strip()
    return out


def main(path):
    data = json.load(open(path, encoding='utf-8'))
    for repo, m in data.items():
        if not m:
            print('### %s :: NO MANIFEST' % repo)
            continue
        c = m['content']
        print('### %s :: %s (%s) %s' % (repo, m['file'], m['branch'], m['url']))
        if m['file'] == 'package.json':
            try:
                j = json.loads(c)
            except Exception:
                j = {}
            print('   name=%s version=%s' % (j.get('name'), j.get('version')))
            deps = dict(j.get('dependencies') or {})
            deps.update(j.get('devDependencies') or {})
            hits = {k: v for k, v in deps.items() if KEEP.search(k)}
            print('   deps(%d total) key=%s' % (len(deps), json.dumps(hits, ensure_ascii=False)[:800]))
            if j.get('engines'):
                print('   engines=%s' % json.dumps(j['engines']))
        elif m['file'] == 'Cargo.toml':
            head = pick(c, ['name', 'version', 'edition', 'license', 'rust-version'])
            print('   %s' % json.dumps(head, ensure_ascii=False))
            deps = re.findall(r'^([A-Za-z0-9_\-]+)\s*=\s*(.+)$', c, re.M)
            hits = {k: v.strip() for k, v in deps if KEEP.search(k)}
            print('   key deps=%s' % json.dumps(hits, ensure_ascii=False)[:900])
        elif m['file'] == 'pyproject.toml':
            head = pick(c, ['name', 'version', 'requires-python', 'license'])
            print('   %s' % json.dumps(head, ensure_ascii=False))
            print('   ---deps---')
            for line in c.splitlines():
                if KEEP.search(line) or re.match(r'^\s*[a-zA-Z0-9_\-]+\s*[<>=~]', line):
                    print('   | %s' % line.strip()[:120])
        elif m['file'] == 'requirements.txt':
            print('   %s' % '; '.join(l.strip() for l in c.splitlines() if l.strip())[:600])
        elif m['file'] == 'go.mod':
            print('   %s' % c[:900].replace('\n', ' | '))
        elif m['file'] == 'setup.py':
            print('   %s' % c[:700].replace('\n', ' | '))
        else:
            print('   %s' % c[:500].replace('\n', ' | '))
        print()


if __name__ == '__main__':
    main(sys.argv[1])
