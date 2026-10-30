# Build environment for the Linux agent, desktop app and packages (deploy/linux/build-packages.sh).
# Ubuntu 24.04 is the oldest base that works: xcap's PipeWire bindings (libspa 0.10) need
# PipeWire 1.0 headers (Ubuntu 22.04 has 0.3.48, Debian 12 0.3.65). The binaries need
# glibc 2.39 and PipeWire 1.0: Ubuntu 24.04+, Debian 13+, Fedora 40+ and similar.
FROM ubuntu:24.04

ARG DEBIAN_FRONTEND=noninteractive
# Headers: PipeWire (Wayland screen capture through the portal; xcap needs it too), EGL and GBM (xcap)
# ALSA (playback through cpal), XCB (X11 capture), D-Bus. PulseAudio is loaded at run time.
# clang: bindgen for the PipeWire bindings. rpm: cargo-generate-rpm checks.
# Xvfb, xdotool, PulseAudio: tests (deploy/linux/test-x11.sh).
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl git build-essential pkg-config clang libclang-dev \
        libpipewire-0.3-dev libspa-0.2-dev libasound2-dev libdbus-1-dev libegl-dev libgbm-dev \
        libxcb1-dev libxcb-randr0-dev libxcb-shm0-dev libxcb-xfixes0-dev \
        libxkbcommon-dev libwayland-dev libx11-dev libxi-dev libxtst-dev libgl-dev \
        rpm file xz-utils desktop-file-utils \
        xvfb xauth xdotool x11-utils x11-xserver-utils xclip pulseaudio pulseaudio-utils alsa-utils libasound2-plugins dbus-x11 libxkbcommon-x11-0 libgl1-mesa-dri \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable \
    && cargo install --locked cargo-deb cargo-generate-rpm \
    && rm -rf /usr/local/cargo/registry
