FROM ubuntu:22.04

ARG DEBIAN_FRONTEND=noninteractive
ARG NODE_VERSION=22.18.0
ARG PNPM_VERSION=10.15.0
ARG RUST_VERSION=1.98.0

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        adb \
        build-essential \
        ca-certificates \
        clang \
        cmake \
        curl \
        file \
        git \
        imagemagick \
        libasound2 \
        libayatana-appindicator3-dev \
        libavcodec-dev \
        libavdevice-dev \
        libavfilter-dev \
        libavformat-dev \
        libavutil-dev \
        libblas3 \
        libclang-dev \
        libdbus-1-dev \
        libdrm-dev \
        libfuse2 \
        libgbm-dev \
        libgtk-3-dev \
        libjack-jackd2-0 \
        liblapack3 \
        libssl-dev \
        libswresample-dev \
        libswscale-dev \
        libva-dev \
        libwayland-dev \
        libwebkit2gtk-4.1-dev \
        libx264-dev \
        libxdo-dev \
        libxkbcommon-dev \
        llvm-dev \
        nasm \
        perl \
        pkg-config \
        xz-utils \
        yasm \
    && rm -rf /var/lib/apt/lists/*

RUN set -eux; \
    case "$(dpkg --print-architecture)" in \
        amd64) node_arch=x64 ;; \
        arm64) node_arch=arm64 ;; \
        *) echo "Unsupported Node.js architecture" >&2; exit 1 ;; \
    esac; \
    node_archive="node-v${NODE_VERSION}-linux-${node_arch}.tar.xz"; \
    curl -fsSLO "https://nodejs.org/dist/v${NODE_VERSION}/${node_archive}"; \
    curl -fsSLO "https://nodejs.org/dist/v${NODE_VERSION}/SHASUMS256.txt"; \
    grep "  ${node_archive}$" SHASUMS256.txt | sha256sum --check --strict; \
    tar -xJf "${node_archive}" -C /usr/local --strip-components=1; \
    rm "${node_archive}" SHASUMS256.txt; \
    npm install --global "pnpm@${PNPM_VERSION}"; \
    node --version; \
    pnpm --version

ENV RUSTUP_HOME=/opt/rustup
ENV CARGO_HOME=/opt/cargo
ENV PATH=/opt/cargo/bin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
ENV RUSTUP_TOOLCHAIN=${RUST_VERSION}
ENV RUSTUP_NO_UPDATE_CHECK=1

RUN curl --proto '=https' --tlsv1.2 -fsS https://sh.rustup.rs -o /tmp/rustup-init.sh \
    && sh /tmp/rustup-init.sh -y --profile minimal --default-toolchain "${RUST_VERSION}" \
    && rm /tmp/rustup-init.sh \
    && chmod -R a+rX /opt/cargo /opt/rustup \
    && rustc --version \
    && cargo --version

WORKDIR /workspace
