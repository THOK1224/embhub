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


def main(repos, out_path, chunk=8):
    found = {}
    for i in range(0, len(repos), chunk):
        part = repos[i:i + chunk]
        q = ' '.join('repo:%s' % r for r in part)
        url = 'https://api.github.com/search/repositories?per_page=100&q=' + urllib.parse.quote(q)
        try:
            data = api(url)
        except Exception as e:
            print('SEARCH ERR batch %d: %s' % (i, e), flush=True)
            time.sleep(7)
            continue
        for it in data.get('items', []):
            found[it['full_name'].lower()] = it
        got = {it['full_name'].lower() for it in data.get('items', [])}
        for r in part:
            if r.lower() not in got:
                print('MISSING: %s' % r, flush=True)
        time.sleep(7)
    slim = {}
    for k, v in found.items():
        slim[k] = {
            'full_name': v['full_name'],
            'html_url': v['html_url'],
            'stars': v['stargazers_count'],
            'forks': v['forks_count'],
            'open_issues': v['open_issues_count'],
            'license': (v.get('license') or {}).get('spdx_id'),
            'license_name': (v.get('license') or {}).get('name'),
            'language': v.get('language'),
            'pushed_at': v.get('pushed_at'),
            'created_at': v.get('created_at'),
            'archived': v.get('archived'),
            'description': v.get('description'),
            'topics': v.get('topics'),
            'default_branch': v.get('default_branch'),
        }
    old = {}
    try:
        old = json.load(open(out_path, encoding='utf-8'))
    except Exception:
        pass
    old.update(slim)
    json.dump(old, open(out_path, 'w', encoding='utf-8'), ensure_ascii=False, indent=2)
    for k in sorted(slim):
        v = slim[k]
        print('%-42s %7d %-10s %-12s %s' % (v['full_name'], v['stars'], str(v['language']),
                                            str(v['license']), (v['pushed_at'] or '')[:10]), flush=True)
    print('total file entries: %d' % len(old))


if __name__ == '__main__':
    repos = json.load(open(sys.argv[1], encoding='utf-8'))
    main(repos, sys.argv[2])
