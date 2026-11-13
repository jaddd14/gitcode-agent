#!/usr/bin/env python3
"""Native Windows smoke: drop a terminal while the agent is executing a turn."""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import time
import uuid

if len(sys.argv) > 1 and sys.argv[1] == "--launch":
    # The outer psmux pane supplies a real console; emulate launching directly
    # from Windows Terminal so GitCode starts its own managed session.
    os.environ.pop("PSMUX_SESSION", None)
    os.environ["PSMUX_ALLOW_NESTING"] = "1"
    raise SystemExit(subprocess.call(sys.argv[2:]))

BINARY = Path(os.environ.get("GITCODE_TEST_BINARY", Path(__file__).resolve().parents[1] / "target/release/gitcode.exe")).resolve()
PSMUX = os.environ.get("GITCODE_TEST_PSMUX", "psmux.exe")


def main():
    with tempfile.TemporaryDirectory(prefix="gitcode-win-detach-") as temporary:
        root = Path(temporary).resolve()
        home = root / "home"
        project = root / "project"
        home.mkdir()
        project.mkdir()
        data_dir = root / "psmux"
        data_dir.mkdir()
        env = os.environ.copy()
        env.update(HOME=str(home), USERPROFILE=str(home), PSMUX_DATA_DIR=str(data_dir),
                   PSMUX_NO_WARM="1", ANTHROPIC_API_KEY="fixture-key-only", GITCODE_ASCII="1")
        for key in ("TMUX", "TMUX_PANE", "PSMUX_SESSION", "PSMUX_TARGET_SESSION"):
            env.pop(key, None)
        env["PATH"] = str(Path(PSMUX).resolve().parent) + os.pathsep + env.get("PATH", "")
        outer = "gitcode-test-outer-" + uuid.uuid4().hex[:12]
        resume = "gitcode-test-resume-" + uuid.uuid4().hex[:12]
        errors = []
        ready = threading.Event()
        disconnected = threading.Event()
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen(3)
        listener.settimeout(40)

        def mux(*args, check=True):
            return subprocess.run([PSMUX, *args], cwd=project, env=env, capture_output=True,
                                  text=True, encoding="utf-8", errors="replace", timeout=20, check=check)

        def names():
            result = mux("list-sessions", "-F", "#{session_name}", check=False)
            return result.stdout.splitlines() if result.returncode == 0 else []

        def wait_for(predicate, description, timeout=40):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                value = predicate()
                if value:
                    return value
                if errors:
                    raise AssertionError(f"{description}: {errors}")
                time.sleep(0.2)
            raise AssertionError(f"timed out: {description}; sessions={names()}; errors={errors}")

        def serve():
            try:
                for turn in range(2):
                    conn, _ = listener.accept()
                    with conn:
                        conn.settimeout(20)
                        data = b""
                        while b"\r\n\r\n" not in data:
                            chunk = conn.recv(65536)
                            if not chunk:
                                raise RuntimeError("incomplete request headers")
                            data += chunk
                        headers, body = data.split(b"\r\n\r\n", 1)
                        size = int(next(line.split(b":", 1)[1] for line in headers.split(b"\r\n") if line.lower().startswith(b"content-length:")))
                        while len(body) < size:
                            chunk = conn.recv(65536)
                            if not chunk:
                                raise RuntimeError("incomplete request body")
                            body += chunk
                        assert b"x-api-key: fixture-key-only" in headers.lower(), "environment was not preserved"
                        if turn == 0:
                            ready.set()
                            assert disconnected.wait(30), "terminal did not disconnect"
                        events = [{"type": "message_start", "message": {}}]
                        if turn == 0:
                            events.append({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "call_1", "name": "Bash", "input": {"command": "echo finished > completion-marker"}}})
                        else:
                            events.extend([{"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
                                           {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "disconnect survived"}}])
                        events.extend([{"type": "content_block_stop", "index": 0},
                                       {"type": "message_delta", "delta": {"stop_reason": "tool_use" if turn == 0 else "end_turn"}},
                                       {"type": "message_stop"}])
                        content = "".join("data: " + json.dumps(event) + "\n\n" for event in events).encode()
                        conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: " + str(len(content)).encode() + b"\r\nConnection: close\r\n\r\n" + content)
            except Exception as error:
                errors.append(repr(error))

        threading.Thread(target=serve, daemon=True).start()
        args = [str(BINARY), "--no-intro", "--base-url", f"http://127.0.0.1:{listener.getsockname()[1]}",
                "--model", "fixture", "--dangerously-skip-permissions"]
        def launch_outer(session, arguments):
            mux("new-session", "-d", "-s", session, "-c", str(project), "--", sys.executable,
                str(Path(__file__).resolve()), "--launch", *arguments)

        try:
            launch_outer(outer, args)
            managed = wait_for(lambda: next((n for n in names() if n.startswith("gitcode-") and n not in (outer, resume)), None), "managed session")
            wait_for(lambda: "shift+tab" in mux("capture-pane", "-p", "-t", managed, check=False).stdout.lower(), "interactive UI")
            mux("send-keys", "-t", managed, "-l", "hello")
            mux("send-keys", "-t", managed, "Enter")
            assert ready.wait(30), f"provider not reached: {errors}"
            mux("kill-session", "-t", outer)
            disconnected.set()
            marker = project / "completion-marker"
            wait_for(lambda: marker.exists() and marker.read_text().strip() == "finished", "tool completion after terminal closed")
            wait_for(lambda: "disconnect survived" in mux("capture-pane", "-p", "-t", managed, check=False).stdout, "final result")
            launch_outer(resume, [str(BINARY), "--continue"])
            wait_for(lambda: mux("display-message", "-p", "-t", managed, "#{session_attached}", check=False).stdout.strip() not in ("", "0"), "reattachment")
            assert len([n for n in names() if n.startswith("gitcode-") and n not in (outer, resume)]) == 1
            mux("send-keys", "-t", managed, "C-d")
            wait_for(lambda: managed not in names(), "clean session exit")
            assert not errors, errors
            print("Windows disconnect, continued work, reattachment and clean exit: passed")
        finally:
            disconnected.set()
            listener.close()
            for name in names():
                if name.startswith("gitcode-"):
                    mux("kill-session", "-t", name, check=False)


if __name__ == "__main__":
    main()
