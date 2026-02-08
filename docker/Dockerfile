# Multi-stage build for oc-rsync on Alpine Linux (musl)
# Produces a minimal runtime image with a statically-linked binary
#
# Build:  podman build -f docker/Dockerfile -t oc-rsync .
# Run:    podman run --rm oc-rsync --version

# ---- Build stage ----
FROM rust:1-alpine3.21 AS builder

RUN apk add --no-cache \
    musl-dev \
    pkgconf \
    git \
    linux-headers \
    acl-dev \
    acl-static \
    attr-dev \
    attr-static \
    lz4-dev \
    lz4-static \
    zstd-dev \
    zstd-static

WORKDIR /src

# Copy dependency manifests first for layer caching
COPY Cargo.toml Cargo.lock ./
COPY crates crates/
COPY xtask xtask/
COPY src src/

# Copy .git for branding build script (version detection)
COPY .git .git/

RUN cargo build --release && \
    strip target/release/oc-rsync

# ---- Runtime stage ----
FROM alpine:3.21

COPY --from=builder /src/target/release/oc-rsync /usr/local/bin/oc-rsync

ENTRYPOINT ["oc-rsync"]
