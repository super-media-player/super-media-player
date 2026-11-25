#!/usr/bin/env bash
# Build whisper.cpp's whisper-cli (speech recognition, for "Subtitles from
# speech") into .devlibs/whisper/, without root. Uses the GPU through Vulkan
# when the Vulkan headers and glslc are installed
# (sudo apt install libvulkan-dev glslc), else the CPU.
#
# scripts/dev-run.sh and scripts/package-linux.sh pick it up from there.
# The speech models are downloaded by the player itself when first used.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
version=1.7.6
sha256=166140e9a6d8a36f787a2bd77f8f44dd64874f12dd8359ff7c1f4f9acb86202e
dest="$root/.devlibs/whisper"
work="$root/.devlibs/build-whisper"

for tool in cmake c++ curl; do
    command -v "$tool" >/dev/null || { echo "needs $tool (sudo apt install cmake build-essential curl)" >&2; exit 1; }
done
gpu=OFF
if command -v glslc >/dev/null && [[ -e /usr/include/vulkan/vulkan.h ]]; then
    gpu=ON
else
    echo "Vulkan headers or glslc not found: building for the CPU only."
    echo "(For the GPU: sudo apt install libvulkan-dev glslc, then run this again.)"
fi

rm -rf "$work" && mkdir -p "$work"
cd "$work"
curl -fsSL -o src.tar.gz "https://github.com/ggml-org/whisper.cpp/archive/refs/tags/v$version.tar.gz"
echo "$sha256  src.tar.gz" | sha256sum -c --quiet
tar xzf src.tar.gz
cd "whisper.cpp-$version"
cmake -B build -DCMAKE_BUILD_TYPE=Release -DGGML_VULKAN=$gpu -DGGML_NATIVE=OFF \
    -DWHISPER_BUILD_TESTS=OFF -DWHISPER_BUILD_SERVER=OFF -DWHISPER_SDL2=OFF \
    -DCMAKE_INSTALL_RPATH='$ORIGIN' -DCMAKE_BUILD_WITH_INSTALL_RPATH=ON >/dev/null
cmake --build build -j"$(nproc)" --target whisper-cli >/dev/null
rm -rf "$dest" && mkdir -p "$dest"
cp build/bin/whisper-cli "$dest/"
find build -name '*.so*' -exec cp -P {} "$dest/" \;
rm -rf "$work"
echo "whisper-cli $version (GPU: $gpu) installed to $dest"
