#!/bin/sh
set -eu
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
version=${1:-${GITCODE_VERSION:-}}
output=${2:-$script_dir/../dist/gitcode}
# Fetch and verify into a temporary sibling before publishing a complete tree.
python3 -I - "$script_dir" "$version" "$output" <<'PY'
import datetime
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.parse
import urllib.request

scripts = pathlib.Path(sys.argv[1])
version = sys.argv[2].removeprefix('v')
output = pathlib.Path(sys.argv[3]).absolute()
api = os.environ.get('GITCODE_GITHUB_API_URL', 'https://api.github.com/repos/jaddd14/gitcode-agent/releases').rstrip('/')
base = subprocess.check_output(['sh', str(scripts / 'render-installers.sh'), '--print-base'], text=True).strip()
targets = ('x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu', 'x86_64-apple-darwin', 'aarch64-apple-darwin', 'x86_64-pc-windows-msvc')

def valid_version(value):
    if not re.fullmatch(r'[0-9][0-9A-Za-z.+-]*', value):
        raise ValueError('Invalid release version')
    return value

class SafeRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        if urllib.parse.urlsplit(newurl).scheme != 'https':
            raise ValueError('Refusing a non-HTTPS redirect')
        return super().redirect_request(request, fp, code, msg, headers, newurl)

opener = urllib.request.build_opener(SafeRedirect)

def download(url, path):
    parsed = urllib.parse.urlsplit(url)
    if (parsed.scheme != 'https' and not (parsed.scheme == 'http' and parsed.hostname in ('127.0.0.1', 'localhost', '::1'))) or parsed.username or parsed.password:
        raise ValueError('Only HTTPS downloads and loopback HTTP tests are allowed')
    with opener.open(urllib.request.Request(url, headers={'User-Agent': 'gitcode-dist'}), timeout=180) as response, path.open('wb') as destination:
        shutil.copyfileobj(response, destination)

if version:
    valid_version(version)
if output.exists():
    raise SystemExit(f'Refusing to overwrite existing distribution: {output}')
output.parent.mkdir(parents=True, exist_ok=True)
work = pathlib.Path(tempfile.mkdtemp(prefix='.gitcode-dist-', dir=output.parent))
try:
    download(api + ('/tags/v' + version if version else '/latest'), work / 'release.json')
    release = json.loads((work / 'release.json').read_text())
    released = valid_version(release['tag_name'].removeprefix('v'))
    if version and version != released:
        raise ValueError('Release version mismatch')
    version = released
    assets = {}
    for item in release['assets']:
        if item['name'] in assets:
            raise ValueError('Duplicate release asset')
        assets[item['name']] = item['browser_download_url']
    download(assets['SHA256SUMS'], work / 'upstream-sums')
    sums = {}
    for line in (work / 'upstream-sums').read_text().splitlines():
        match = re.fullmatch(r'([0-9a-fA-F]{64})[ \t]+\*?(.+)', line)
        if not match:
            raise ValueError('Malformed SHA256SUMS')
        digest, name = match.groups()
        if name in sums:
            raise ValueError('Duplicate checksum entry')
        sums[name] = digest.lower()
    tree = work / 'tree'
    version_dir = tree / ('v' + version)
    version_dir.mkdir(parents=True)
    manifest = {'version': version, 'date': release.get('published_at') or datetime.datetime.now(datetime.timezone.utc).isoformat(), 'platforms': {}}
    normalized_sums = []
    for target in targets:
        extension = '.zip' if 'windows' in target else '.tar.gz'
        normalized = 'gitcode-' + target + extension
        candidates = [normalized, 'gitcode-v' + version + '-' + target + extension]
        found = [name for name in candidates if name in assets]
        if len(found) != 1:
            raise ValueError(f'Expected exactly one release archive for {target}')
        name = found[0]
        expected = sums[name]
        path = version_dir / normalized
        download(assets[name], path)
        digest = hashlib.sha256()
        with path.open('rb') as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b''):
                digest.update(block)
        if digest.hexdigest() != expected:
            raise ValueError('SHA256 mismatch: ' + name)
        normalized_sums.append(expected + '  ' + normalized + '\n')
        manifest['platforms'][target] = {'url': base + '/v' + version + '/' + normalized, 'sha256': expected}
    (version_dir / 'SHA256SUMS').write_text(''.join(normalized_sums))
    (tree / 'SHA256SUMS').write_text(''.join(line[:66] + 'v' + version + '/' + line[66:] for line in normalized_sums))
    (tree / 'latest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    subprocess.run(['sh', str(scripts / 'render-installers.sh'), str(tree)], check=True, env={**os.environ, 'INSTALL_BASE_URL': base})
    tree.rename(output)
    print(str(output))
finally:
    shutil.rmtree(work)
PY
