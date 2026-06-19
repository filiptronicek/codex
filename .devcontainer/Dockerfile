FROM mcr.microsoft.com/devcontainers/base:ubuntu-24.04

ARG DEBIAN_FRONTEND=noninteractive
ARG NODE_MAJOR=22
ARG RUST_TOOLCHAIN=1.95.0

ENV COREPACK_ENABLE_DOWNLOAD_PROMPT=0

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

# enable 'universe' because musl-tools & clang live there
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        gnupg \
        software-properties-common && \
    add-apt-repository --yes universe && \
    curl -fsSL "https://deb.nodesource.com/setup_${NODE_MAJOR}.x" | bash -

# now install build deps
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        build-essential \
        curl \
        git \
        ca-certificates \
        pkg-config \
        libcap-dev \
        clang \
        musl-tools \
        libssl-dev \
        libsqlite3-dev \
        just \
        nodejs \
        python3 \
        python3-pip \
        ripgrep && \
    npm install -g corepack@latest && \
    corepack enable && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/*

USER vscode

# install Rust + musl target as dev user
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain "${RUST_TOOLCHAIN}" && \
    ~/.cargo/bin/rustup target add aarch64-unknown-linux-musl x86_64-unknown-linux-musl && \
    ~/.cargo/bin/rustup component add clippy rustfmt rust-src

ENV PATH="/home/vscode/.cargo/bin:${PATH}"

WORKDIR /workspace
