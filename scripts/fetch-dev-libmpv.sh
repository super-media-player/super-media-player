#!/usr/bin/env bash
# Ubuntu/Debian without root: download libmpv and the runtime libraries it
# needs that are missing, and unpack them into .devlibs/ (git-ignored).
# With root, `sudo apt install libmpv-dev` does the same system-wide.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$root/.devlibs/debs"
cd "$root/.devlibs/debs"
missing=$(apt-cache depends libmpv2 | awk '/Depends/ {print $2}' | grep -v '^<' | while read -r p; do
    dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q "install ok installed" || echo "$p"
done)
# shellcheck disable=SC2086
apt-get download libmpv2 libmpv-dev $missing
for f in *.deb; do dpkg-deb -x "$f" "$root/.devlibs/root"; done
echo "libmpv unpacked to $root/.devlibs/root"
