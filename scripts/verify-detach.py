#!/usr/bin/env python3
"""Exercise a real terminal disconnect with a local mock provider and tmux."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import socket
import struct
import subprocess
import tempfile
import termios
import threading
import time

BINARY = os.environ.get(
    "GITCODE_TEST_BINARY", str(Path(__file__).resolve().parents[1] / "target/debug/gitcode")
)


def payload(tool_call):
    events = [{"type": "message_start", "message": {}}]
    if tool_call:
        events.append({
            "type": "content_block_start", "index": 0,
            "content_block": {
                "type": "tool_use", "id": "call_1", "name": "Bash",
                "input": {"command": "printf '%s' \"$(printf finished)\" > completion-marker"},
            },
        })
    else:
        events.extend([
            {"type": "content_block_start", "index": 0,
             "content_block": {"type": "text", "text": ""}},
            {"type": "content_block_delta", "index": 0,
             "delta": {"type": "text_delta", "text": "Отключение пережито"}},
        ])
    events.extend([
        {"type": "content_block_stop", "index": 0},
        {"type": "message_delta", "delta": {"stop_reason": "tool_use" if tool_call else "end_turn"}},
        {"type": "message_stop"},
    ])
    return "".join("data: " + json.dumps(event, ensure_ascii=False) + "\n\n" for event in events).encode()


def main():
    errors = []
    with tempfile.TemporaryDirectory(prefix="gitcode-disconnect-") as tmp:
        root = Path(tmp).resolve()
        home, project = root / "home", root / "project"
        home.mkdir(mode=0o700)
        project.mkdir()
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen(3)
        listener.settimeout(20)
        ready, disconnected = threading.Event(), threading.Event()

        def serve():
            try:
                for turn in range(2):
                    conn, _ = listener.accept()
                    conn.settimeout(10)
                    with conn:
                        data = b""
                        while b"\r\n\r\n" not in data:
                            chunk = conn.recv(65536)
                            if not chunk:
                                raise RuntimeError("incomplete request headers")
                            data += chunk
                        headers, body = data.split(b"\r\n\r\n", 1)
                        size = int(next(line.split(b":", 1)[1] for line in headers.split(b"\r\n")
                                        if line.lower().startswith(b"content-length:")))
                        while len(body) < size:
                            chunk = conn.recv(65536)
                            if not chunk:
                                raise RuntimeError("incomplete request body")
                            body += chunk
                        assert b"x-api-key: fixture-key-only" in headers.lower(), "environment lost"
                        if turn == 0:
                            ready.set()
                            assert disconnected.wait(12), "client never disconnected"
                        content = payload(turn == 0)
                        conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: " + str(len(content)).encode()
                                     + b"\r\nConnection: close\r\n\r\n" + content)
            except Exception as error:
                errors.append(repr(error))

        thread = threading.Thread(target=serve, daemon=True)
        thread.start()
        env = dict(os.environ)
        env.update(HOME=str(home), TERM="xterm-256color", ANTHROPIC_API_KEY="fixture-key-only")
        for key in ("TMUX", "TMUX_PANE", "GITCODE_TMUX_MANAGED", "GITCODE_LAUNCH_ENV",
                    "GITCODE_ASCII", "NO_COLOR", "COLORTERM"):
            env.pop(key, None)
        args = [BINARY, "--no-intro", "--base-url", f"http://127.0.0.1:{listener.getsockname()[1]}",
                "--model", "fixture", "--dangerously-skip-permissions"]

        def launch(arguments):
            pid, master = pty.fork()
            if pid == 0:
                os.chdir(project)
                os.execve(BINARY, arguments, env)
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
            return pid, master

        def sessions():
            result = subprocess.run(
                ["tmux", "list-sessions", "-F", "#{session_name}\t#{session_attached}\t#{pane_current_path}"],
                capture_output=True, text=True, check=False,
            )
            return [line.split("\t") for line in result.stdout.splitlines() if str(project) in line]

        pid, master = launch(args)
        try:
            deadline, terminal = time.time() + 15, b""
            while time.time() < deadline:
                if select.select([master], [], [], 0.1)[0]:
                    try:
                        terminal += os.read(master, 65536)
                    except OSError:
                        break
                if sessions() and b"shift+tab" in terminal:
                    break
            assert sessions(), f"managed session did not start: {terminal[-1000:]!r}"
            session_name = sessions()[0][0]
            assert subprocess.check_output(
                ["tmux", "show-options", "-t", session_name, "-v", "status"], text=True
            ).strip() == "off", "tmux status line is visible"
            screen = subprocess.check_output(
                ["tmux", "capture-pane", "-p", "-e", "-t", session_name], text=True
            )
            context_row = next((row for row in screen.splitlines() if "ctx " in row), "")
            assert "\x1b[38;5;239m▯" in context_row, f"wrong context color: {context_row!r}"
            assert "Несколько вызовов инструмента в одном" in screen, "header was drawn at the wrong width"
            for rows, columns in ((36, 120), (30, 100)):
                fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
                deadline = time.time() + 5
                while time.time() < deadline:
                    size = subprocess.check_output(
                        ["tmux", "display-message", "-p", "-t", session_name, "#{pane_width}x#{pane_height}"],
                        text=True,
                    ).strip()
                    if size == f"{columns}x{rows}":
                        break
                    time.sleep(0.1)
                assert size == f"{columns}x{rows}", f"pane did not resize: {size}"
            assert not list((home / ".gitcode").glob("launch-env-*")), "temporary environment not removed"
            # A quick reconnect can arrive before tmux notices the first terminal
            # closing. It must attach to the existing session, not start a duplicate.
            second_pid, second_master = launch([BINARY, "--continue"])
            time.sleep(1)
            assert len(sessions()) == 1, "continue started a duplicate live session"
            os.close(second_master)
            os.waitpid(second_pid, 0)
            os.write(master, b"hello\r")
            assert ready.wait(10), f"mock provider not reached: {errors}"
            os.close(master)
            master = None
            disconnected.set()
            deadline = time.time() + 15
            marker = project / "completion-marker"
            while time.time() < deadline and not marker.exists():
                time.sleep(0.1)
            assert marker.read_text() == "finished", f"tool did not run after disconnect: {errors}"
            thread.join(10)
            assert not errors, errors
            deadline = time.time() + 10
            while time.time() < deadline and not any(row[1] == "0" for row in sessions()):
                time.sleep(0.1)
            assert any(row[1] == "0" for row in sessions()), "session did not detach"
            resumed_pid, resumed_master = launch([BINARY, "--continue"])
            time.sleep(2)
            current = sessions()
            assert len(current) == 1 and current[0][1] != "0", f"failed to reconnect: {current}"
            output = subprocess.check_output(["tmux", "capture-pane", "-p", "-t", current[0][0]], text=True)
            assert "Отключение пережито" in output, "result missing after reconnect"
            os.write(resumed_master, b"\x04")
            time.sleep(0.3)
            os.write(resumed_master, b"\x04")
            time.sleep(1)
            os.close(resumed_master)
            os.waitpid(resumed_pid, 0)
            assert not sessions(), "session did not exit after Ctrl+D"
            print("disconnect, continued tool execution, environment, reattach and result: passed")
        finally:
            if master is not None:
                os.close(master)
            for row in sessions():
                subprocess.run(["tmux", "kill-session", "-t", row[0]], capture_output=True, check=False)
            try:
                os.waitpid(pid, os.WNOHANG)
            except ChildProcessError:
                pass
            listener.close()


if __name__ == "__main__":
    main()
