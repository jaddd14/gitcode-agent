#!/usr/bin/env python3
"""Smoke-test native installers against an isolated, locally served release fixture."""

import argparse
import functools
import hashlib
import http.server
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import tempfile
import threading
import zipfile


class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_args):
        pass


def run(command, env, cwd, success=True):
    result = subprocess.run(command, env=env, cwd=cwd, capture_output=True,
                            text=True, encoding="utf-8", errors="replace", timeout=180)
    if (result.returncode == 0) != success:
        raise AssertionError(f"Unexpected status {result.returncode}: {command}\n"
                             f"{result.stdout}\n{result.stderr}")
    return result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    scripts = Path(__file__).resolve().parent
    repo = scripts.parent
    windows = os.name == "nt"
    if windows:
        target = "x86_64-pc-windows-msvc"
    else:
        machine = platform.machine().lower()
        arch = {"x86_64": "x86_64", "amd64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}[machine]
        target = arch + ("-apple-darwin" if platform.system() == "Darwin" else "-unknown-linux-gnu")
    with tempfile.TemporaryDirectory(prefix="gitcode-installer-smoke-") as temp:
        root = Path(temp)
        home = root / "home"
        home.mkdir()
        web = root / "dist"
        web.mkdir()
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(("GITCODE_", "ANTHROPIC_", "OPENAI_"))}
        env.update(HOME=str(home), USERPROFILE=str(home), LOCALAPPDATA=str(home / "local"),
                   TMPDIR=str(root), TEMP=str(root), TMP=str(root), NO_COLOR="1")
        version_output = run([str(binary), "--version"], env, root).strip()
        version = version_output.split()[1].removeprefix("v")
        release = web / ("v" + version)
        release.mkdir()
        name = "gitcode-" + target + (".zip" if windows else ".tar.gz")
        archive = release / name
        if windows:
            with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as package:
                package.write(binary, "gitcode.exe")
                package.write(repo / "LICENSE", "LICENSE")
                package.write(repo / "README.md", "README.md")
        else:
            with tarfile.open(archive, "w:gz") as package:
                package.add(binary, arcname="gitcode")
                package.add(repo / "LICENSE", arcname="LICENSE")
                package.add(repo / "README.md", arcname="README.md")
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        (release / "SHA256SUMS").write_text(f"{digest}  {name}\n", encoding="utf-8")
        handler = functools.partial(Handler, directory=str(web))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            base = f"http://127.0.0.1:{server.server_port}"
            manifest = {"version": version, "date": "2026-01-01T00:00:00Z", "platforms": {
                target: {"url": f"{base}/v{version}/{name}", "sha256": digest}}}
            manifest_path = web / "latest.json"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            api = web / "api"
            (api / "tags").mkdir(parents=True)
            metadata = json.dumps({"tag_name": "v" + version, "assets": [
                {"name": asset, "browser_download_url": f"{base}/v{version}/{asset}"}
                for asset in (name, "SHA256SUMS")]})
            (api / "latest").write_text(metadata, encoding="utf-8")
            (api / "tags" / ("v" + version)).write_text(metadata, encoding="utf-8")
            env["GITCODE_GITHUB_API_URL"] = base + "/api"
            env["INSTALL_BASE_URL"] = base
            bash = shutil.which("bash")
            if not bash:
                raise RuntimeError("bash is required to render the installer fixtures")
            run([bash, (scripts / "render-installers.sh").as_posix(), web.as_posix()], env, root)
            shells = ["sh"]
            if windows:
                shells = ["powershell.exe", "pwsh.exe"]
                for shell in shells:
                    if not shutil.which(shell):
                        raise RuntimeError(f"Required Windows installer runtime missing: {shell}")
            for index, shell in enumerate(shells):
                destination = root / f"installed bin {index}"
                env["GITCODE_INSTALL_DIR"] = str(destination)
                installed = destination / ("gitcode.exe" if windows else "gitcode")
                if windows:
                    # Installation updates USER PATH; restore the actual registry value even on failure.
                    script = str(web / "install.ps1").replace("'", "''")
                    command = [shell, "-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command",
                               "$ErrorActionPreference = 'Stop'; "
                               "$savedPath = [Environment]::GetEnvironmentVariable('Path', 'User'); "
                               f"try {{ & '{script}' }} "
                               "finally { [Environment]::SetEnvironmentVariable('Path', $savedPath, 'User') }"]
                else:
                    command = [shell, str(web / "install.sh")]
                for scenario in ("latest", "pinned-upgrade", "fallback-latest", "fallback-pinned"):
                    env.pop("GITCODE_VERSION", None)
                    env.pop("GITCODE_BASE_URL", None)
                    if "pinned" in scenario:
                        env["GITCODE_VERSION"] = "v" + version
                    if "fallback" in scenario:
                        env["GITCODE_BASE_URL"] = base + "/unavailable"
                    run(command, env, root)
                    assert hashlib.sha256(installed.read_bytes()).hexdigest() == hashlib.sha256(binary.read_bytes()).hexdigest()
                    assert run([str(installed), "--version"], env, root).strip() == version_output
                    print(f"PASS {shell}: {scenario}", flush=True)
                env.pop("GITCODE_VERSION", None)
                env.pop("GITCODE_BASE_URL", None)
                manifest["platforms"][target]["sha256"] = "0" * 64
                manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
                before = installed.read_bytes()
                run(command, env, root, success=False)
                assert installed.read_bytes() == before, "Checksum failure changed the installed executable"
                assert not list(destination.glob(".gitcode-install*")), "Staging files leaked"
                manifest["platforms"][target]["sha256"] = digest
                manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
                print(f"PASS {shell}: checksum mismatch preserves installation", flush=True)
        finally:
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == "__main__":
    main()
