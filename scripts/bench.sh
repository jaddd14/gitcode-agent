#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

echo "=== gitcode reproducible benchmark (M15) ==="
echo "Host: $(uname -srm)"
echo "Rustc: $(rustc --version)"

if [ ! -x target/release/gitcode ]; then
  echo "Building release binary..."
  cargo build --release --quiet
fi

BIN="target/release/gitcode"
SIZE_BYTES=$(stat -c %s "$BIN" 2>/dev/null || stat -f %z "$BIN")
SIZE_MB=$(awk "BEGIN {printf \"%.2f\", $SIZE_BYTES / (1024 * 1024)}")
echo "Release binary size: ${SIZE_BYTES} bytes (${SIZE_MB} MiB)"

START_NS=$(date +%s%N)
for _ in 1 2 3 4 5 6 7 8 9 10; do
  "$BIN" --version >/dev/null
done
END_NS=$(date +%s%N)
ELAPSED_MS=$(( (END_NS - START_NS) / 1000000 ))
AVG_MS=$(awk "BEGIN {printf \"%.2f\", $ELAPSED_MS / 10.0}")
echo "Cold start (--version, 10 runs): total ${ELAPSED_MS} ms, avg ${AVG_MS} ms/run (budget: < 40 ms)"

HELP_START_NS=$(date +%s%N)
for _ in 1 2 3 4 5 6 7 8 9 10; do
  "$BIN" --help >/dev/null
done
HELP_END_NS=$(date +%s%N)
HELP_ELAPSED_MS=$(( (HELP_END_NS - HELP_START_NS) / 1000000 ))
HELP_AVG_MS=$(awk "BEGIN {printf \"%.2f\", $HELP_ELAPSED_MS / 10.0}")
echo "Cold start (--help, 10 runs): total ${HELP_ELAPSED_MS} ms, avg ${HELP_AVG_MS} ms/run (budget: < 40 ms)"

if [ -x /usr/bin/time ]; then
  RSS_KB=$(/usr/bin/time -f "%M" "$BIN" --version 2>&1 >/dev/null | tail -n 1 || true)
  if [ -n "$RSS_KB" ]; then
    RSS_MB=$(awk "BEGIN {printf \"%.2f\", $RSS_KB / 1024.0}")
    echo "Peak RSS (--version): ${RSS_KB} KiB (${RSS_MB} MiB, budget: < 60 MiB)"
  fi
fi
