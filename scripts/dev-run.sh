#!/usr/bin/env bash
# Run the desktop app in development, using the engine built by
# scripts/build-engine.sh when it is there, else the locally unpacked libmpv
# in .devlibs/ when the system has none (see scripts/fetch-dev-libmpv.sh).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
dev="$root/.devlibs/root/usr/lib/x86_64-linux-gnu"
engine="$root/.devlibs/engine/lib"
if [[ -e "$engine/libmpv.so.2" ]]; then
    export SMP_LIBMPV="$engine/libmpv.so.2"
    export LD_LIBRARY_PATH="$engine:$dev${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
elif [[ -e "$dev/libmpv.so.2" ]]; then
    export SMP_LIBMPV="$dev/libmpv.so.2"
    export LD_LIBRARY_PATH="$dev${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi
# Optional helpers built by scripts/fetch-whisper.sh and fetch-projectm.sh.
[[ -x "$root/.devlibs/whisper/whisper-cli" ]] && export SMP_WHISPER="$root/.devlibs/whisper/whisper-cli"
[[ -d "$root/.devlibs/projectm/lib" ]] && export SMP_EXTRA_LIBS="$root/.devlibs/projectm/lib"
[[ -d "$root/.devlibs/projectm/presets" ]] && export SMP_MILKDROP_PRESETS="$root/.devlibs/projectm/presets"
"$root/scripts/clean-builds.sh" --dev || echo "could not clean old debug files" >&2
exec cargo run --manifest-path "$root/Cargo.toml" -p super-media-player -- "$@"
