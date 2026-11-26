#!/usr/bin/env bash
# Run a command in the smp-build container (packaging/linux/build.Containerfile)
# with this project mounted at /src. Used on the build machine.
#
#   scripts/in-container.sh '<command>'      e.g. 'scripts/package-linux.sh'
#   scripts/in-container.sh --image          build (or rebuild) the image first
#   scripts/in-container.sh --image=DATE     rebuild with that day's Ubuntu updates
#
# Rootless podman, with the caller's own user id inside, so the files it
# writes belong to the caller. Downloads (cargo registry) are kept between
# runs in ~/.cache/smp-build. SMP_* variables are passed through.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
image=localhost/smp-build
if [[ "${1:-}" == --image* ]]; then
    refresh="${1#--image}"
    refresh="${refresh#=}"
    shift
    podman build --pull=newer ${refresh:+--build-arg "REFRESH=$refresh"} -t "$image" \
        -f "$root/packaging/linux/build.Containerfile" "$root/packaging/linux"
    [[ $# -eq 0 ]] && exit 0
fi
podman image exists "$image" || { echo "No smp-build image: run scripts/in-container.sh --image" >&2; exit 1; }
cache="$HOME/.cache/smp-build"
mkdir -p "$cache/cargo-registry" "$cache/cargo-git" "$cache/home"
env_args=()
while IFS='=' read -r name _; do env_args+=(-e "$name"); done < <(env | grep -E '^SMP_[A-Z0-9_]+=' || true)
exec podman run --rm --init --userns=keep-id --security-opt label=disable \
    -v "$root:/src" \
    -v "$cache/cargo-registry:/opt/cargo/registry" \
    -v "$cache/cargo-git:/opt/cargo/git" \
    -v "$cache/home:/home/build" \
    -e HOME=/home/build -e CARGO_TERM_COLOR=never \
    "${env_args[@]}" \
    -w /src "$image" bash -c "$*"
