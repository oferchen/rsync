#!/usr/bin/env bash
set -euo pipefail

# pick a known working zig version for Linux cross
ZIG_URL="https://ziglang.org/builds/zig-linux-x86_64-0.13.0-dev.2552+hash.tar.xz"

mkdir -p "$HOME/zig"
curl -sSL "$ZIG_URL" | tar -xJ --strip-components=1 -C "$HOME/zig"
echo "$HOME/zig" >> "$GITHUB_PATH"

