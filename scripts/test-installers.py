#!/usr/bin/env python3
"""Isolated offline installer/distribution checks; no real release or user home."""
import contextlib
import hashlib
import http.server
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import threading
import unittest
import zipfile

SCRIPTS = Path(__file__).resolve().parent
TARGETS = ('x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu', 'x86_64-apple-darwin', 'aarch64-apple-darwin', 'x86_64-pc-windows-msvc')


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='gitcode-install-test-')
        self.root = Path(self.temp.name)
        self.web = self.root / 'web'
        self.web.mkdir()
        handler = lambda *args, **kwargs: QuietHandler(*args, directory=str(self.web), **kwargs)
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.base = f'http://127.0.0.1:{self.server.server_port}'
        self.home = self.root / 'home'
        self.home.mkdir()
        self.bin = self.root / 'tools'
        self.bin.mkdir()
        self.uname = self.bin / 'uname'
        self.set_arch('Linux', 'x86_64')
        self.env = {**os.environ, 'HOME': str(self.home), 'TMPDIR': str(self.root), 'GITCODE_BASE_URL': self.base, 'GITCODE_GITHUB_API_URL': self.base + '/api', 'PATH': str(self.bin) + ':' + os.environ['PATH']}
        for key in ('GITCODE_INSTALL_DIR', 'GITCODE_VERSION', 'INSTALL_BASE_URL'):
            self.env.pop(key, None)
        self.release_dir = self.web / 'v1.2.3'
        self.release_dir.mkdir()
        sums = []
        platforms = {}
        assets = []
        for target in TARGETS:
            name = 'gitcode-' + target + ('.zip' if 'windows' in target else '.tar.gz')
            path = self.release_dir / name
            if 'windows' in target:
                with zipfile.ZipFile(path, 'w') as archive:
                    archive.writestr('gitcode.exe', b'fake-windows-binary')
            else:
                self.make_tar(path)
            sha = hashlib.sha256(path.read_bytes()).hexdigest()
            sums.append(f'{sha}  {name}\n')
            url = self.base + '/v1.2.3/' + name
            platforms[target] = {'url': url, 'sha256': sha}
            assets.append({'name': name, 'browser_download_url': url})
        (self.release_dir / 'SHA256SUMS').write_text(''.join(sums))
        assets.append({'name': 'SHA256SUMS', 'browser_download_url': self.base + '/v1.2.3/SHA256SUMS'})
        self.manifest = {'version': '1.2.3', 'date': '2026-11-08', 'platforms': platforms}
        self.save_manifest()
        api = self.web / 'api'
        (api / 'tags').mkdir(parents=True)
        release = json.dumps({'tag_name': 'v1.2.3', 'published_at': '2026-11-08T00:00:00Z', 'body': 'Release notes\nwith \t formatting and цветы', 'assets': assets})
        (api / 'latest').write_text(release)
        (api / 'tags/v1.2.3').write_text(release)

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        self.temp.cleanup()

    def set_arch(self, system, arch):
        self.uname.write_text(f'#!/bin/sh\ncase "$1" in -s) printf "{system}\\n";; -m) printf "{arch}\\n";; esac\n')
        self.uname.chmod(0o755)

    def save_manifest(self):
        (self.web / 'latest.json').write_text(json.dumps(self.manifest))

    def make_tar(self, path, kind='regular'):
        with tarfile.open(path, 'w:gz') as archive:
            info = tarfile.TarInfo('gitcode')
            data = b'#!/bin/sh\nprintf "gitcode 1.2.3\\n"\n'
            info.mode = 0o755
            if kind == 'symlink':
                info.type = tarfile.SYMTYPE
                info.linkname = str(self.root / 'outside')
                archive.addfile(info)
            else:
                info.size = len(data)
                archive.addfile(info, io.BytesIO(data))
                if kind == 'duplicate':
                    archive.addfile(info, io.BytesIO(data))
                # Must never be extracted, even when present beside a valid binary.
                evil = tarfile.TarInfo('../outside')
                evil.size = 4
                archive.addfile(evil, io.BytesIO(b'evil'))

    def run_installer(self, success=True, script=None):
        result = subprocess.run(['sh', str(script or SCRIPTS / 'install.sh')], env=self.env, capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        self.assertFalse((self.root / 'outside').exists())
        self.assertEqual(list(self.root.glob('gitcode-install.*')), [])
        return result

    def installed(self):
        return Path(self.env.get('GITCODE_INSTALL_DIR', str(self.home / '.local/bin'))) / 'gitcode'

    def test_latest_upgrade_pinned_and_spaces(self):
        self.env['GITCODE_INSTALL_DIR'] = str(self.root / 'custom bin')
        self.run_installer()
        self.assertEqual(subprocess.check_output([str(self.installed())], text=True), 'gitcode 1.2.3\n')
        self.env['GITCODE_VERSION'] = 'v1.2.3'
        self.run_installer()
        self.assertEqual(list(self.installed().parent.glob('.gitcode-install.*')), [])

    def test_all_unix_architectures(self):
        for system, arch in [('Linux', 'x86_64'), ('Linux', 'aarch64'), ('Darwin', 'arm64'), ('Darwin', 'x86_64')]:
            with self.subTest(system=system, arch=arch):
                self.set_arch(system, arch)
                self.run_installer()
        self.set_arch('Linux', 'riscv64')
        self.run_installer(False)

    def test_github_fallback_latest_and_pinned(self):
        self.env['GITCODE_BASE_URL'] = self.base + '/unavailable'
        self.run_installer()
        self.env['GITCODE_VERSION'] = '1.2.3'
        self.run_installer()

    def test_github_versioned_asset_names(self):
        api = self.web / 'api/latest'
        release = json.loads(api.read_text())
        for asset in release['assets']:
            if asset['name'].endswith('.tar.gz'):
                old = asset['name']
                new = old.replace('gitcode-', 'gitcode-v1.2.3-', 1)
                (self.release_dir / old).rename(self.release_dir / new)
                asset['name'] = new
                asset['browser_download_url'] = self.base + '/v1.2.3/' + new
        sums = self.release_dir / 'SHA256SUMS'
        sums.write_text(sums.read_text().replace('  gitcode-', '  gitcode-v1.2.3-'))
        api.write_text(json.dumps(release))
        self.env['GITCODE_BASE_URL'] = self.base + '/unavailable'
        self.run_installer()

    def test_pinned_bad_sums_never_falls_back(self):
        self.env['GITCODE_VERSION'] = '1.2.3'
        (self.release_dir / 'SHA256SUMS').write_text('')
        self.run_installer(False)
        self.assertFalse(self.installed().exists())

    def test_render_rejects_injection(self):
        env = {**self.env, 'INSTALL_BASE_URL': "https://example.com/';touch /tmp/no"}
        result = subprocess.run(['sh', str(SCRIPTS / 'render-installers.sh'), str(self.root / 'bad-render')], env=env, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / 'bad-render').exists())

    def test_checksum_mismatch_preserves_existing(self):
        self.run_installer()
        before = self.installed().read_bytes()
        self.manifest['platforms'][TARGETS[0]]['sha256'] = '0' * 64
        self.save_manifest()
        self.run_installer(False)
        self.assertEqual(before, self.installed().read_bytes())

    def test_reject_unsafe_archive_entries(self):
        for kind in ('symlink', 'duplicate'):
            with self.subTest(kind=kind):
                path = self.release_dir / ('gitcode-' + TARGETS[0] + '.tar.gz')
                self.make_tar(path, kind)
                self.manifest['platforms'][TARGETS[0]]['sha256'] = hashlib.sha256(path.read_bytes()).hexdigest()
                self.save_manifest()
                self.run_installer(False)
                self.assertFalse(self.installed().exists())

    def test_invalid_manifest_and_version(self):
        (self.web / 'latest.json').write_text('{"version":"../../oops"}')
        self.run_installer(False)
        self.env['GITCODE_VERSION'] = '../../oops'
        self.run_installer(False)

    def test_render_and_assemble(self):
        rendered = self.root / 'rendered'
        env = {**self.env, 'INSTALL_BASE_URL': self.base}
        subprocess.run(['sh', str(SCRIPTS / 'render-installers.sh'), str(rendered)], env=env, check=True)
        self.assertTrue((rendered / 'install.ps1').read_bytes().startswith(b'\xef\xbb\xbf'))
        self.env.pop('GITCODE_BASE_URL')
        self.run_installer(script=rendered / 'install.sh')
        dist = self.root / 'dist'
        subprocess.run(['sh', str(SCRIPTS / 'assemble-dist.sh'), '1.2.3', str(dist)], env=env, check=True, capture_output=True)
        manifest = json.loads((dist / 'latest.json').read_text())
        self.assertEqual(set(manifest['platforms']), set(TARGETS))
        for target, entry in manifest['platforms'].items():
            archive = dist / entry['url'].removeprefix(self.base + '/')
            self.assertEqual(hashlib.sha256(archive.read_bytes()).hexdigest(), entry['sha256'])
        subprocess.run(['sha256sum', '-c', 'SHA256SUMS'], cwd=dist, check=True, capture_output=True)
        subprocess.run(['sha256sum', '-c', 'SHA256SUMS'], cwd=dist / 'v1.2.3', check=True, capture_output=True)
        again = subprocess.run(['sh', str(SCRIPTS / 'assemble-dist.sh'), '1.2.3', str(dist)], env=env, capture_output=True)
        self.assertNotEqual(again.returncode, 0)

    def test_assemble_aborts_on_mismatch(self):
        path = self.release_dir / ('gitcode-' + TARGETS[0] + '.tar.gz')
        path.write_bytes(b'corrupt')
        dist = self.root / 'bad-dist'
        result = subprocess.run(['sh', str(SCRIPTS / 'assemble-dist.sh'), '1.2.3', str(dist)], env=self.env, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(dist.exists())
        self.assertEqual(list(self.root.glob('.gitcode-dist-*')), [])


if __name__ == '__main__':
    unittest.main()
