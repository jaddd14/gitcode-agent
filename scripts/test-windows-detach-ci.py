#!/usr/bin/env python3
"""Run the Windows disconnect smoke with a pinned, checksum-verified psmux."""
import hashlib
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import urllib.request
import zipfile

URL = "https://github.com/psmux/psmux/releases/download/v3.4.0/psmux-v3.4.0-windows-x64.zip"
SHA256 = "a66b6be7c13c6fa0dc3660a267b9e34eaa91a4eb7f07fbd55a0468018bd0f721"

if sys.platform != "win32":
    raise SystemExit("This acceptance test requires native Windows")
request = urllib.request.Request(URL, headers={"User-Agent": "gitcode-ci"})
with urllib.request.urlopen(request, timeout=90) as response:
    archive = response.read(32 * 1024 * 1024 + 1)
if len(archive) > 32 * 1024 * 1024 or hashlib.sha256(archive).hexdigest() != SHA256:
    raise SystemExit("psmux archive failed size or SHA-256 validation")
with tempfile.TemporaryDirectory(prefix="gitcode-psmux-test-") as directory:
    executable = Path(directory) / "psmux.exe"
    with zipfile.ZipFile(io.BytesIO(archive)) as package:
        executable.write_bytes(package.read("psmux.exe"))
    env = dict(os.environ, GITCODE_TEST_PSMUX=str(executable))
    subprocess.run([sys.executable, "-I", "scripts/verify-windows-detach.py"], env=env, check=True)
