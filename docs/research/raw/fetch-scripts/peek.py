import sys
import urllib.request

UA = {'User-Agent': 'dsh-research'}
url = sys.argv[1]
n = int(sys.argv[2]) if len(sys.argv) > 2 else 3000
req = urllib.request.Request(url, headers=UA)
try:
    with urllib.request.urlopen(req, timeout=30) as r:
        txt = r.read().decode('utf-8', 'replace')
    sys.stdout.write(txt[:n])
    sys.stdout.write('\n\n[TRUNCATED total=%d]\n' % len(txt))
except Exception as e:
    sys.stdout.write('ERR %s\n' % e)
