#!/usr/bin/env bash
# Build the Linux packages on a separate build machine (a VM) over ssh, so
# the long engine build doesn't tie up this one. The VM must run the same
# Ubuntu release as the machines the packages are for (or an older one):
# programs built on a newer system need its newer glibc.
#
#   scripts/vm-build.sh setup    once: install the build packages (asks for the
#                                VM's sudo password) and Rust on the VM
#   scripts/vm-build.sh connect owner/repo [branch]
#                                once: make the VM's folder a checkout of the
#                                GitHub repository (branch: main). From then
#                                on the VM builds GitHub's newest commit of
#                                the branch, like Kelyph: push, then build.
#   scripts/vm-build.sh          start the build; it runs on the VM by itself
#                                and sends a phone notification when done
#                                (see below)
#   scripts/vm-build.sh start --copy   build this folder as it is, changes
#                                not yet pushed included (copied over; the
#                                next build from GitHub drops them again)
#   scripts/vm-build.sh sync     only update the VM's code (no build, no
#                                cron): for when build-hub runs the builds
#                                and the daily check (--copy as above)
#   scripts/vm-build.sh publish  publish the built packages as the GitHub
#                                release of the version tag (a build of a
#                                tagged commit does this by itself)
#   scripts/vm-build.sh status   show the end of the VM's build log
#   scripts/vm-build.sh fetch    copy the finished packages into dist/
#   scripts/vm-build.sh schedule   copy the source over and turn on the daily
#                                check (scripts/vm-daily.sh, 05:30 VM time):
#                                rebuilds when a bundled library has an update
#   scripts/vm-build.sh unschedule turn the daily check off
#   scripts/vm-build.sh daily    run the daily check now (--force: rebuild anyway)
#
# The VM's address comes from vm-build.conf here (git-ignored):
#   BUILD_HOST=user@host         (or an alias from ~/.ssh/config)
#   BUILD_DIR=super-media-player (folder on the VM, under its home; optional)
# ssh must log in with a key (no password prompt).
#
# Notifications: put NTFY_URL=https://ntfy.sh/<topic> in notify.conf inside
# BUILD_DIR on the VM (see scripts/notify.sh). Without it, nothing is sent.
# Releases: put github.conf there too (see scripts/publish-github.sh).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
conf="$root/vm-build.conf"
[[ -f "$conf" ]] && source "$conf"
: "${BUILD_HOST:?set BUILD_HOST in vm-build.conf}"
BUILD_DIR="${BUILD_DIR:-super-media-player}"

remote() { ssh -o BatchMode=yes "$BUILD_HOST" "$@"; }
running() { remote "pgrep -f '$BUILD_DIR/scripts/[v]m-(job|daily).sh' >/dev/null"; }
cron_line="30 5 * * * \$HOME/$BUILD_DIR/scripts/vm-daily.sh"
copy=""; [[ "${2:-}" == --copy ]] && copy=1
connected() { remote "test -d $BUILD_DIR/.git"; }

# What the VM builds: GitHub's branch, or (--copy, or not connected yet)
# this folder copied over.
update_code() {
    if [[ -z "$copy" ]] && connected; then
        local branch
        branch=$(remote "git -C $BUILD_DIR config --get smp.branch || echo main")
        if [[ -n "$(git -C "$root" status --porcelain --untracked-files=no)" ]] ||
            ! git -C "$root" merge-base --is-ancestor HEAD "origin/$branch" 2>/dev/null; then
            echo "Note: the VM builds GitHub's $branch; changes here that are not pushed"
            echo "      there are not in the build (--copy builds this folder as it is)."
        fi
        echo "==> updating the code on $BUILD_HOST from GitHub"
        remote "$BUILD_DIR/scripts/vm-pull.sh"
    else
        sync_source
    fi
}

sync_source() {
    echo "==> copying source to $BUILD_HOST:$BUILD_DIR"
    # Build outputs and the VM's own state stay on each side; excluded files
    # (the VM's engine, notify.conf, the daily check's versions) are never
    # deleted by --delete.
    rsync -az --delete \
        --exclude=/target --exclude=/dist --exclude=/.devlibs --exclude=/.git \
        --exclude=/notify.conf --exclude=/github.conf --exclude=/vm-build.conf --exclude=/build.log \
        --exclude='/engine-versions.conf*' --exclude=/daily.log --exclude=/.daily.lock \
        "$root/" "$BUILD_HOST:$BUILD_DIR/"
}

case "${1:-start}" in
connect)
    repo="${2:?usage: vm-build.sh connect owner/repo [branch]}"
    branch="${3:-main}"
    if running; then
        echo "A build is running on $BUILD_HOST; try again when it's done" >&2
        exit 1
    fi
    # The repository is public, so the VM fetches over https with no key.
    # Files the VM keeps (.devlibs, dist, target, logs, *.conf) are ignored
    # by git and stay; the copied source is replaced by the checkout.
    echo "==> $BUILD_HOST:$BUILD_DIR follows https://github.com/$repo ($branch)"
    remote "set -e; mkdir -p $BUILD_DIR; cd $BUILD_DIR
        [ -d .git ] || git init -q -b '$branch'
        git remote remove origin 2>/dev/null || true
        git remote add origin 'https://github.com/$repo.git'
        git config smp.branch '$branch'
        git fetch --quiet --tags origin '$branch'
        git checkout -q -f -B '$branch' 'origin/$branch'
        git branch -q --set-upstream-to='origin/$branch'
        echo \"    at \$(git log -1 --format='%h %s')\""
    ;;
setup)
    # Everything the engine build lists, plus what the app and the packager need.
    deps=$("$root/scripts/build-engine.sh" --deps | sed 's/^sudo apt install //')
    extra="rsync dpkg-dev libfontconfig-dev libxkbcommon-dev libaacs0 libbdplus0 libbluray2"
    echo "==> installing build packages on $BUILD_HOST (asks for its sudo password)"
    ssh -t "$BUILD_HOST" "sudo apt-get update -qq && sudo apt-get install -y $deps $extra"
    echo "==> Rust"
    remote 'command -v cargo >/dev/null || [ -x ~/.cargo/bin/cargo ] && echo "    already installed" ||
        (curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal)'
    echo "Setup done. Put NTFY_URL=... in ~/$BUILD_DIR/notify.conf on the VM for phone notifications."
    ;;
start)
    if running; then
        echo "A build is already running on $BUILD_HOST: scripts/vm-build.sh status" >&2
        exit 1
    fi
    update_code
    echo "==> starting the build"
    # The code is already updated; the job does not pull again.
    remote "cd $BUILD_DIR && SMP_NO_PULL=1 nohup setsid \"\$PWD/scripts/vm-job.sh\" >build.log 2>&1 </dev/null &"
    echo "Running on $BUILD_HOST. You'll get a notification when it's done;"
    echo "meanwhile: scripts/vm-build.sh status"
    ;;
sync)
    if running; then
        echo "A build is running on $BUILD_HOST; try again when it's done" >&2
        exit 1
    fi
    update_code
    echo "Code updated. build-hub's jobs use it from their next run."
    ;;
status)
    if running; then echo "(running)"; else echo "(not running)"; fi
    remote "tail -n 20 $BUILD_DIR/build.log 2>/dev/null || echo 'no build log yet'"
    remote "crontab -l 2>/dev/null | grep -q '$BUILD_DIR/scripts/vm-daily.sh'" &&
        { echo "--- daily check (on; last lines of daily.log):"; remote "tail -n 5 $BUILD_DIR/daily.log 2>/dev/null"; }
    ;;
schedule)
    if running; then
        echo "A build is running on $BUILD_HOST; try again when it's done" >&2
        exit 1
    fi
    update_code
    remote "(crontab -l 2>/dev/null | grep -v '$BUILD_DIR/scripts/vm-daily.sh'; echo '$cron_line') | crontab -"
    echo "Daily check on: $cron_line"
    ;;
unschedule)
    remote "crontab -l 2>/dev/null | grep -v '$BUILD_DIR/scripts/vm-daily.sh' | crontab -"
    echo "Daily check off."
    ;;
daily)
    remote "nohup setsid \"\$HOME/$BUILD_DIR/scripts/vm-daily.sh\" ${2:-} >/dev/null 2>&1 </dev/null &"
    echo "Daily check started on $BUILD_HOST; result by notification, log in ~/$BUILD_DIR/daily.log"
    ;;
publish)
    remote "$BUILD_DIR/scripts/publish-github.sh"
    ;;
fetch)
    version=$(sed -n '/^\[workspace.package\]/,/^\[/ s/^version *= *"\(.*\)"/\1/p' "$root/Cargo.toml")
    mkdir -p "$root/dist"
    rsync -av "$BUILD_HOST:$BUILD_DIR/dist/*$version*" "$root/dist/"
    ;;
*)
    sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
    exit 1
    ;;
esac
