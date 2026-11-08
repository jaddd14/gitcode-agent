#!/usr/bin/env python3
"""Reject credential markers without printing matching secret material."""
import pathlib
import re
import subprocess
import sys

root = pathlib.Path(__file__).resolve().parent.parent
markers = ["github" + "_pat_", "gh" + "p_", "gh" + "o_", "gh" + "s_"]
pattern = "|".join(markers)
revisions = subprocess.check_output(["git", "rev-list", "--all"], cwd=root, text=True).split()
result = subprocess.run(["git", "grep", "-lE", pattern, *revisions], cwd=root, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
if result.returncode not in (0, 1):
    sys.exit("History scan could not complete")
working_hits = []
for path in root.rglob("*"):
    if any(part in (".git", "target") for part in path.relative_to(root).parts):
        continue
    if path.is_file() and not path.is_symlink() and re.search(pattern.encode(), path.read_bytes()):
        working_hits.append(path)
print("History credential-marker matches:", len(result.stdout.splitlines()))
print("Working-tree credential-marker matches:", len(working_hits))
sys.exit(bool(result.stdout or working_hits))
