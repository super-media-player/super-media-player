#!/usr/bin/env bash
# Cleans old compiled code on the development computer.
#
# Usage: clean-builds.sh --dev [--dry-run]
#          on the development computer, after the version was bumped: every
#          version leaves its own copy of the app's debug files (289 GB in
#          five days once), so when the version isn't the one they were last
#          cleaned for, SMP's own crates are cleaned from target/debug. Other
#          crates stay compiled. scripts/dev-run.sh runs it.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

dry=0 dev=0
while (($#)); do
    case "$1" in
    --dry-run) dry=1 ;;
    --dev) dev=1 ;;
    *) echo "Unknown option $1 (see the top of this script)" >&2; exit 2 ;;
    esac
    shift
done

if ((dev)); then
    version=$(sed -n '/^\[workspace.package\]/,/^\[/ s/^version *= *"\(.*\)"/\1/p' Cargo.toml)
    stamp=target/debug/.cleaned-version
    [[ -d target/debug && "$(cat "$stamp" 2>/dev/null)" != "$version" ]] || exit 0
    echo "new version $version: cleaning SMP's own debug files"
    # cargo waits here while another build uses the folder.
    cargo clean --offline -p super-media-player -p player-core -p mpv-sys $( ((dry)) && echo --dry-run) 2>&1 | tail -n 2
    ((dry)) || echo "$version" >"$stamp"
    exit 0
fi
echo "Usage: clean-builds.sh --dev [--dry-run]" >&2
exit 2
