#!/usr/bin/env bash
# Keeps only the newest build of SMP on the build VM. scripts/vm-job.sh runs
# it; by hand it works anywhere.
#
# Usage: clean-builds.sh [--dry-run]
#          after a build: deletes every older version's packages from dist/,
#          keeping the newest .deb and the files of the same version
#        clean-builds.sh --before-build VERSION [--dry-run]
#          before a build: when VERSION isn't the one target/ was last
#          compiled for, deletes target/, so old compiled code doesn't pile up.
#          The packages of the last version stay until the new ones are done.
#        clean-builds.sh --dev [--dry-run]
#          on the development computer, after the version was bumped: every
#          version leaves its own copy of the app's debug files (289 GB in
#          five days once), so when the version isn't the one they were last
#          cleaned for, SMP's own crates are cleaned from target/debug. Other
#          crates stay compiled. scripts/dev-run.sh runs it.
# The engine (.devlibs) and dist/cache are kept.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

dry=0 before="" dev=0
while (($#)); do
    case "$1" in
    --dry-run) dry=1 ;;
    --dev) dev=1 ;;
    --before-build) before="${2:?--before-build needs a version}"; shift ;;
    *) echo "Unknown option $1 (see the top of this script)" >&2; exit 2 ;;
    esac
    shift
done

remove() {
    local size
    size=$(du -sh -- "$1" 2>/dev/null | cut -f1)
    if ((dry)); then echo "would delete: $1 ($size)"; else echo "deleting: $1 ($size)"; rm -rf -- "$1"; fi
}

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

if [[ -n "$before" ]]; then
    stamp=target/.built-version
    if [[ -d target && "$(cat "$stamp" 2>/dev/null)" != "$before" ]]; then remove target; fi
    ((dry)) || { mkdir -p target && echo "$before" >"$stamp"; }
    exit 0
fi

shopt -s nullglob
debs=(dist/super-media-player_*.deb)
((${#debs[@]})) || { echo "nothing to clean: no packages in dist/"; exit 0; }
newest=$(ls -1t "${debs[@]}" | head -n 1)
keep=${newest#dist/super-media-player_}
keep=${keep%_*.deb}
echo "keeping $keep"
for f in "${debs[@]}" dist/super-media-player-*-linux-*.tar.gz; do
    v=${f#dist/super-media-player[_-]}
    case "$f" in
    *.deb) v=${v%_*.deb} ;;
    *) v=${v%-linux-*.tar.gz} ;;
    esac
    [[ "$v" == "$keep" ]] || remove "$f"
done
