#!/usr/bin/env -S python3 -I
"""Capture the existing release UI in isolated, uniquely named tmux sessions.

Run with python3 -I scripts/verify-ui.py. No builds or real-provider calls.
Only testdata/ui/tmux-* files are written inside the checkout.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
PERIODS = ("session", "today", "7days", "30days", "all")


def tmux(*args):
    return subprocess.run(["tmux", *map(str, args)], check=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout.decode()


def seed(home, workspace):
    sessions = list((home / ".gitcode/sessions").glob("*/*.jsonl"))
    if len(sessions) != 1:
        raise RuntimeError(f"Expected one isolated session, found {len(sessions)}")
    session_id = sessions[0].stem
    now = int(time.time() * 1000)
    records = []
    for i, days in enumerate((0, 0, 1, 3, 6, 8, 13, 20, 29, 45)):
        records.append(dict(timestamp_ms=now - days * 86400000,
                            session_id=session_id if i < 3 else "ui-other-session",
                            project=str(workspace), provider="ui-fixture",
                            model="ui-priced" if i % 2 == 0 else "ui-unpriced",
                            agent="main" if i % 3 else "ui-reviewer",
                            input_tokens=1000 + i * 100, output_tokens=200 + i * 10,
                            cache_read_tokens=500, cache_write_tokens=100,
                            reasoning_tokens=20, estimated=i == 1,
                            cost_usd=0.001 + i * 0.0001 if i % 2 == 0 else None,
                            latency_ms=1000, tokens_per_sec=200.0 + i * 10))
    ledger = home / ".gitcode/usage.jsonl"
    ledger.write_text("".join(json.dumps(r) + "\n" for r in records))
    ledger.chmod(0o600)
    return records


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--widths", nargs="+", type=int, default=[60, 80, 100, 140, 40])
    parser.add_argument("--settle", type=float, default=0.16)
    args = parser.parse_args()
    binary = ROOT / "target/release/gitcode"
    if not binary.is_file():
        parser.error("Existing target/release/gitcode is required; this script never builds")
    out = ROOT / "testdata/ui"
    out.mkdir(parents=True, exist_ok=True)
    manifest = {"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "version": subprocess.check_output([str(binary), "--version"], text=True).strip(),
                "normalization": "Temporary root suffix replaced with same-width X characters; ANSI removed only in .txt",
                "captures": [], "checks": [], "seed": []}
    for width in args.widths:
        height = 12 if width == 40 else 36
        session = "gitcode-ui-" + uuid.uuid4().hex
        target = session + ":0.0"
        with tempfile.TemporaryDirectory(prefix="gitcode-ui-") as temp:
            base = Path(temp)
            home, workspace = base / "home", base / "workspace"
            home.mkdir(mode=0o700)
            workspace.mkdir()
            env = ["env", "-i", "HOME=" + str(home), "PATH=/usr/local/bin:/usr/bin:/bin",
                   "LANG=C.UTF-8", "TERM=tmux-256color", "COLORTERM=truecolor",
                   "XDG_CONFIG_HOME=" + str(home / ".config"),
                   "XDG_DATA_HOME=" + str(home / ".local/share"),
                   "XDG_CACHE_HOME=" + str(home / ".cache"), str(binary),
                   "--base-url", "http://127.0.0.1:9", "--model", "ui-fixture"]
            started = False
            try:
                tmux("new-session", "-d", "-s", session, "-x", width, "-y", height,
                     "-c", workspace, shlex.join(env))
                started = True
                tmux("set-option", "-t", session, "status", "off")
                tmux("resize-window", "-t", target, "-x", width, "-y", height)
                actual = tmux("display-message", "-p", "-t", target, "#{pane_width}x#{pane_height}").strip()
                if actual != f"{width}x{height}":
                    raise RuntimeError(f"Unexpected pane dimensions: {actual}")
                tmux("set-window-option", "-t", session, "history-limit", "10000")
                time.sleep(1.0)

                def keys(*values):
                    tmux("send-keys", "-t", target, *values)
                    time.sleep(args.settle)

                def text(value):
                    tmux("send-keys", "-t", target, "-l", "--", value)
                    time.sleep(args.settle)

                def capture(name, history=False):
                    prefix = f"tmux-{name}-{width}" + ("x12" if height == 12 else "")
                    flag = ["-S", "-"] if history else []
                    colored = tmux("capture-pane", "-t", target, "-p", "-e", *flag)
                    normalized_root = "/tmp/gitcode-ui-" + "X" * len(base.name.removeprefix("gitcode-ui-"))
                    colored = colored.replace(str(base), normalized_root)
                    plain = ANSI.sub("", colored)
                    (out / (prefix + ".ansi")).write_text(colored)
                    (out / (prefix + ".txt")).write_text(plain)
                    manifest["captures"].append(prefix)
                    return plain

                def check(name, passed):
                    manifest["checks"].append(dict(width=width, name=name, passed=passed))

                startup = capture("startup")
                check("startup prompt", "gitcode" in startup and "shift+tab" in startup)
                text("Первая строка\\")
                keys("Enter")
                text("Вторая строка — проверяем перенос\\")
                keys("Enter")
                text("Третья строка")
                multiline = capture("multiline")
                check("multiline input", all(s in multiline for s in ("Первая", "Вторая", "Третья")))
                keys("C-c")
                text("/")
                popup = capture("popup")
                check("slash popup", "Команды" in popup)
                text("usa")
                filtered = capture("popup-filtered")
                check("filtered popup", "/usage" in filtered and "/help" not in filtered)
                keys("C-c")
                for state in ("empty", "seeded"):
                    if state == "seeded":
                        records = seed(home, workspace)
                        if not manifest["seed"]:
                            manifest["seed"] = [dict(r, session_id="<CURRENT>" if r["session_id"] != "ui-other-session" else r["session_id"], project="<UI_TEMP>/workspace") for r in records]
                    text("/usage")
                    keys("Enter")
                    for index, period in enumerate(PERIODS):
                        if index:
                            keys("Tab")
                        usage = capture(f"usage-{state}-{period}")
                        check(f"usage {state} {period} card", "Использование" in usage)
                        expected_requests = (3, 2, 5, 9, 10)[index] if state == "seeded" else 0
                        check(f"usage {state} {period} request count", f"Запросы: {expected_requests} ·" in usage)
                        if width != 40:
                            check(f"usage {state} {period} full frame", "╭" in usage and "╰" in usage)
                        if state == "seeded" and width != 40:
                            check(f"usage seeded {period} data", "ui-priced" in usage and "ui-reviewer" in usage)
                    keys("t")
                    capture(f"usage-{state}-all-cost")
                    keys("Escape")
                for command in ("help", "context", "model"):
                    text("/" + command)
                    keys("Enter")
                    capture(command)
                    capture(command + "-scrollback", history=True)
                    if command == "model":
                        keys("Down")
                        capture("model-down")
                keys("C-d")
            finally:
                if started:
                    subprocess.run(["tmux", "kill-session", "-t", session],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    manifest["failed_checks"] = sum(not c["passed"] for c in manifest["checks"])
    (out / "tmux-verification.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({"captures": len(manifest["captures"]),
                      "failed_checks": manifest["failed_checks"],
                      "manifest": str(out / "tmux-verification.json")}, ensure_ascii=False))


if __name__ == "__main__":
    main()
