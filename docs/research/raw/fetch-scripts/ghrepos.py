import json
import sys
import time
import urllib.request

UA = {'User-Agent': 'dsh-research', 'Accept': 'application/vnd.github+json'}


def api(url):
    req = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(req, timeout=40) as r:
        return json.load(r)


def repo(full):
    d = api('https://api.github.com/repos/' + full)
    return {
        'full_name': d['full_name'],
        'html_url': d['html_url'],
        'stars': d['stargazers_count'],
        'forks': d['forks_count'],
        'open_issues': d['open_issues_count'],
        'license': (d.get('license') or {}).get('spdx_id'),
        'license_name': (d.get('license') or {}).get('name'),
        'language': d.get('language'),
        'pushed_at': d.get('pushed_at'),
        'created_at': d.get('created_at'),
        'archived': d.get('archived'),
        'fork': d.get('fork'),
        'description': d.get('description'),
        'topics': d.get('topics'),
        'default_branch': d.get('default_branch'),
        'size_kb': d.get('size'),
    }


if __name__ == '__main__':
    repos = json.load(open(sys.argv[1], encoding='utf-8'))
    out = {}
    for full in repos:
        try:
            out[full.lower()] = repo(full)
            d = out[full.lower()]
            print('OK  %-45s %7d %-12s %-12s %s' % (full, d['stars'], str(d['language']),
                                                    str(d['license']), (d['pushed_at'] or '')[:10]), flush=True)
        except Exception as e:
            print('ERR %-45s %s' % (full, e), flush=True)
        time.sleep(1)
    json.dump(out, open(sys.argv[2], 'w', encoding='utf-8'), ensure_ascii=False, indent=2)
    print('saved %d' % len(out))
