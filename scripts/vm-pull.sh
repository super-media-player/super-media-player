#!/usr/bin/env bash
# ON the build VM: brings the project folder to the newest commit of the
# GitHub branch it follows, so what is built is always a known commit.
# `scripts/vm-build.sh connect` makes the folder a git checkout; vm-job.sh
# and vm-daily.sh run this first.
#
# Changes made on the VM (or copied over by `vm-build.sh start --copy`) are
# thrown away: the folder is only a copy for building. Files git ignores stay
# (.devlibs with the engine, dist, target, logs, notify.conf, github.conf,
# engine-versions.conf).
#
# Exit codes: 0 done, 3 the folder is not a git checkout, others failed.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
[[ -d .git ]] || exit 3

branch=$(git config --get smp.branch || echo main)
before=$(git rev-parse --short HEAD 2>/dev/null || echo none)
git fetch --quiet --tags origin "$branch"
git reset --quiet --hard "origin/$branch"
after=$(git rev-parse --short HEAD)
if [[ "$before" == "$after" ]]; then
    echo "==> code is current: $branch at $after"
else
    echo "==> code updated: $branch $before → $after"
fi
