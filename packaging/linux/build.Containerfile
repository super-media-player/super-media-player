# The build environment for every Linux package of Super Media Player
# (scripts/in-container.sh runs commands in it, on the build machine, with
# rootless podman; scripts/vm-build.sh image builds it):
#
#   Ubuntu 22.04: programs built on a newer system need its newer glibc, so
#   the packages built here run on Ubuntu 22.04 / Mint 21 / Debian 12 /
#   Fedora 36 and everything newer;
#   the headers of every library the engine (FFmpeg + libplacebo + libmpv)
#   and the app link against, and the encoders clips need (x264, LAME, Opus);
#   meson from pip (mpv needs a newer one than 22.04 has);
#   Rust; dpkg-deb, rpmbuild and the AppImage tool for the packages;
#   the Windows cross-build: clang/lld 14, the x86_64-pc-windows-msvc Rust
#   target and cargo-xwin (which downloads Microsoft's CRT and Windows SDK
#   into $HOME/.cache/cargo-xwin on first use), NSIS for the installer and
#   7-Zip for unpacking the Windows libmpv.
#
# The source is mounted at /src; nothing of it is baked in.
FROM docker.io/library/ubuntu:22.04

ARG RUST_VERSION=1.99.1
ARG CARGO_XWIN_VERSION=0.23.1
ARG MESON_VERSION=1.7.2
# appimagetool and its runtime (AppImage/appimagetool release 1.9.1),
# checked against the SHA-256 taken when the version was pinned.
ARG APPIMAGETOOL_URL=https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage
ARG APPIMAGETOOL_SHA256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
# The runtime at the front of every AppImage (type2-runtime 20251108); without
# it appimagetool would download whatever is newest each time.
ARG RUNTIME_URL=https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64
ARG RUNTIME_SHA256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d

# scripts/vm-daily.sh passes the date, so each day's image has that day's
# Ubuntu security updates (the packages bundle some of these libraries).
ARG REFRESH=none
ENV DEBIAN_FRONTEND=noninteractive LANG=C.UTF-8
RUN echo "packages as of ${REFRESH}" && apt-get update && apt-get upgrade -y \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl git file xz-utils unzip rsync python3 python3-pip python3-jinja2 \
      build-essential pkg-config cmake ninja-build nasm patchelf gpg gpg-agent dirmngr \
      libgnutls28-dev libdav1d-dev libxml2-dev zlib1g-dev \
      libx264-dev libmp3lame-dev libopus-dev \
      libdrm-dev libva-dev libegl-dev libgl-dev libgles-dev libgbm-dev hwdata \
      libx11-dev libxext-dev libxrandr-dev libxinerama-dev libxss-dev libxpresent-dev \
      libwayland-dev wayland-protocols libxkbcommon-dev \
      libasound2-dev libpulse-dev \
      libass-dev libbluray-dev libdvdnav-dev libdvdread-dev libcdio-paranoia-dev \
      liblua5.2-dev libuchardet-dev liblcms2-dev libjpeg-dev \
      libfontconfig-dev libchromaprint1 libaacs0 libbdplus0 \
      libvulkan-dev \
      dpkg-dev rpm desktop-file-utils \
      clang lld llvm nsis p7zip-full librsvg2-bin zip \
 && rm -rf /var/lib/apt/lists/*

RUN pip3 install --no-cache-dir "meson==${MESON_VERSION}" && meson --version

# The AppImage tool is an AppImage itself; containers have no FUSE.
ENV APPIMAGE_EXTRACT_AND_RUN=1
RUN curl -fsSLo /usr/local/bin/appimagetool "${APPIMAGETOOL_URL}" \
 && echo "${APPIMAGETOOL_SHA256}  /usr/local/bin/appimagetool" | sha256sum -c - \
 && chmod 755 /usr/local/bin/appimagetool \
 && curl -fsSLo /usr/local/share/appimage-runtime-x86_64 "${RUNTIME_URL}" \
 && echo "${RUNTIME_SHA256}  /usr/local/share/appimage-runtime-x86_64" | sha256sum -c -

# Rust for every user (the container runs as the build machine user's id).
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain "${RUST_VERSION}" \
      --target x86_64-pc-windows-msvc \
 && cargo install --locked cargo-xwin --version "${CARGO_XWIN_VERSION}" \
 && rm -rf /opt/cargo/registry /opt/cargo/git \
 && chmod -R a+rwX /opt/rustup /opt/cargo && rustc --version

# clang-cl, lld-link, llvm-lib, llvm-rc (Ubuntu names them only in here).
ENV PATH=$PATH:/usr/lib/llvm-14/bin
WORKDIR /src
