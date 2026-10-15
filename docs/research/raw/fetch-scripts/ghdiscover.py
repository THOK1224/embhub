import json
import sys
import time
import urllib.parse
import urllib.request

UA = {'User-Agent': 'dsh-research', 'Accept': 'application/vnd.github+json'}


def api(url):
    req = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(req, timeout=40) as r:
        return json.load(r)


def search_q(q, label, per_page=5, sort='stars'):
    url = ('https://api.github.com/search/repositories?per_page=%d&sort=%s&order=desc&q=%s'
           % (per_page, sort, urllib.parse.quote(q)))
    try:
        data = api(url)
    except Exception as e:
        print('=== %s : ERR %s' % (label, e), flush=True)
        return []
    rows = []
    for it in data.get('items', []):
        rows.append({
            'full_name': it['full_name'],
            'html_url': it['html_url'],
            'stars': it['stargazers_count'],
            'language': it.get('language'),
            'license': (it.get('license') or {}).get('spdx_id'),
            'pushed_at': it.get('pushed_at'),
            'archived': it.get('archived'),
            'description': it.get('description'),
        })
    print('=== %s [n=%s]' % (label, data.get('total_count')), flush=True)
    for r in rows:
        print('  %-42s %7d  %-12s %-12s %s  %s' % (
            r['full_name'], r['stars'], str(r['language']), str(r['license']),
            (r['pushed_at'] or '')[:10], (r['description'] or '')[:90]), flush=True)
    return rows


QUERIES = [
    ('ninjaterm serial', 'ninjaterm'),
    ('serial monitor terminal qt', 'serial-monitor-qt'),
    ('MinTerm serial', 'minterm'),
    ('btools serial', 'btools'),
    ('arduino serial monitor', 'arduino-serial-monitor'),
    ('serial port monitor', 'serial-port-monitor'),
    ('wezterm', 'wezterm'),
    ('tokio serial', 'tokio-serial'),
    ('ssh2 rust bindings', 'ssh2-rs'),
    ('ttyd', 'ttyd'),
    ('pyside6 shiboken', 'pyside'),
    ('rt-thread studio', 'rtthread-studio'),
    ('serial studio', 'serial-studio'),
]

if __name__ == '__main__':
    out = {}
    for q, label in QUERIES:
        out[label] = search_q(q, label)
        time.sleep(7)
    json.dump(out, open(sys.argv[1], 'w', encoding='utf-8'), ensure_ascii=False, indent=2)
    print('DONE')
