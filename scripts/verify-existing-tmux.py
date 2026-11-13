#!/usr/bin/env python3
"""Check --continue reconnects to GitCode started in an existing tmux session."""
import fcntl
import os
from pathlib import Path
import pty
import struct
import subprocess
import tempfile
import termios
import time

BINARY = os.environ.get("GITCODE_TEST_BINARY", str(Path(__file__).resolve().parents[1] / "target/debug/gitcode"))
with tempfile.TemporaryDirectory(prefix="gitcode-existing-tmux-") as tmp:
    base = Path(tmp).resolve()
    home, project = base / "home", base / "project"
    home.mkdir(mode=0o700)
    project.mkdir()
    name = f"gitcode-existing-test-{os.getpid()}"
    env = dict(os.environ, HOME=str(home), TERM="xterm-256color", ANTHROPIC_API_KEY="fixture-only")
    for key in ("TMUX", "TMUX_PANE", "GITCODE_TMUX_MANAGED", "GITCODE_LAUNCH_ENV"):
        env.pop(key, None)
    sessions, master, pid = [], None, None
    try:
        subprocess.run([
            "tmux", "new-session", "-d", "-s", name, "-c", str(project),
            "env", f"HOME={home}", "TERM=tmux-256color", "ANTHROPIC_API_KEY=fixture-only",
            BINARY, "--no-intro", "--base-url", "http://127.0.0.1:9", "--model", "fixture",
        ], check=True, capture_output=True)
        subprocess.run(["tmux", "set-option", "-t", name, "status", "on"], check=True, capture_output=True)
        deadline = time.time() + 10
        while time.time() < deadline and not list((home / ".gitcode/sessions").rglob("*.jsonl")):
            time.sleep(0.1)
        assert list((home / ".gitcode/sessions").rglob("*.jsonl")), "first session did not start"
        pid, master = pty.fork()
        if pid == 0:
            os.chdir(project)
            os.execve(BINARY, [BINARY, "--continue"], env)
        fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
        time.sleep(2)
        result = subprocess.run([
            "tmux", "list-sessions", "-F", "#{session_name}\t#{session_attached}\t#{pane_current_path}"
        ], capture_output=True, text=True, check=False)
        sessions = [line.split("\t") for line in result.stdout.splitlines() if str(project) in line]
        assert len(sessions) == 1 and sessions[0][0] == name and sessions[0][1] != "0", sessions
        assert subprocess.check_output(["tmux", "show-options", "-t", name, "-v", "status"], text=True).strip() == "on", "changed another tmux session"
        print("reattachment to an existing tmux session: passed")
    finally:
        if master is not None:
            os.close(master)
        for row in sessions:
            subprocess.run(["tmux", "kill-session", "-t", row[0]], capture_output=True, check=False)
        subprocess.run(["tmux", "kill-session", "-t", name], capture_output=True, check=False)
        if pid:
            try:
                os.waitpid(pid, 0)
            except ChildProcessError:
                pass
