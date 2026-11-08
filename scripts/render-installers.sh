#!/bin/sh
set -eu
# The updater owns the source constant; installers contain rendered copies.
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ -z "${INSTALL_BASE_URL:-}" ]; then
    INSTALL_BASE_URL=$(python3 -I - "$script_dir/../src/update.rs" <<'PY'
import pathlib
import re
import sys
text = pathlib.Path(sys.argv[1]).read_text(encoding='utf-8')
match = re.search(r'pub const INSTALL_BASE_URL:\s*&str\s*=\s*"([^"\n]+)";', text)
if not match:
    raise SystemExit('INSTALL_BASE_URL source constant not found')
print(match[1])
PY
)
fi
if [ "${1:-}" = --print-base ]; then
    printf '%s\n' "${INSTALL_BASE_URL%/}"
    exit 0
fi
output=${1:-$script_dir}
python3 -I - "$script_dir" "$output" "$INSTALL_BASE_URL" <<'PY'
import pathlib
import re
import sys
from urllib.parse import urlsplit

source, output = map(pathlib.Path, sys.argv[1:3])
base = sys.argv[3].rstrip('/')
u = urlsplit(base)
if (u.scheme != 'https' and not (u.scheme == 'http' and u.hostname in ('localhost', '127.0.0.1', '::1'))) or not u.netloc or u.username or u.password or u.query or u.fragment or re.search(r"[^A-Za-z0-9:/._~%\[\]-]", base):
    raise SystemExit('INSTALL_BASE_URL must be an HTTPS URL without credentials, query, fragment or shell metacharacters (loopback HTTP is allowed).')
output.mkdir(parents=True, exist_ok=True)
for name in ('install.sh', 'install.ps1'):
    text = (source / name).read_text(encoding='utf-8-sig')
    pattern = r"(?m)^(\s*\$?INSTALL_BASE_URL\s*=\s*)'[^']*'( # rendered-install-base)$"
    text, count = re.subn(pattern, lambda m: m[1] + "'" + base + "'" + m[2], text)
    if count != 1:
        raise SystemExit(f'{name}: expected exactly one rendered constant')
    destination = output / name
    # A BOM lets Windows PowerShell 5.1 read the Russian messages as UTF-8.
    destination.write_text(text, encoding='utf-8-sig' if name.endswith('.ps1') else 'utf-8')
    destination.chmod(0o755 if name.endswith('.sh') else 0o644)
PY
