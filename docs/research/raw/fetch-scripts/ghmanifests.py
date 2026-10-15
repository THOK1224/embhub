import json
import sys
import time
import urllib.request

UA = {'User-Agent': 'dsh-research'}
BRANCHES = ['master', 'main', 'develop', 'dev']
CANDIDATES = ['package.json', 'Cargo.toml', 'requirements.txt', 'setup.py',
              'pyproject.toml', 'go.mod', 'pom.xml', 'build.gradle', 'CMakeLists.txt',
              'pubspec.yaml', '*.csproj']


def get(url):
    req = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read().decode('utf-8', 'replace')


def fetch_manifest(full):
    for b in BRANCHES:
        for f in CANDIDATES:
            if f == '*.csproj':
                continue
            url = 'https://raw.githubusercontent.com/%s/%s/%s' % (full, b, f)
            try:
                txt = get(url)
            except Exception:
                continue
            return {'branch': b, 'file': f, 'url': url, 'content': txt[:4000]}
    return None


if __name__ == '__main__':
    repos = json.load(open(sys.argv[1], encoding='utf-8'))
    out = {}
    for full in repos:
        m = fetch_manifest(full)
        out[full] = m
        print('%-45s %s' % (full, (m or {}).get('file', 'NONE')), flush=True)
    json.dump(out, open(sys.argv[2], 'w', encoding='utf-8'), ensure_ascii=False, indent=2)
    print('DONE')
