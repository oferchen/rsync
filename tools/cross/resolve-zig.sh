#!/bin/sh
set -eu

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
CACHE_DIR="${SCRIPT_DIR}/dist"
ZIG_VERSION="0.12.1"

resolve_archive_name() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Linux)
            case "$arch" in
                x86_64|amd64)
                    printf 'zig-linux-x86_64-%s.tar.xz\n' "$ZIG_VERSION"
                    ;;
                aarch64|arm64)
                    printf 'zig-linux-aarch64-%s.tar.xz\n' "$ZIG_VERSION"
                    ;;
                *)
                    echo "error: unsupported architecture '$arch' for automatic zig download" >&2
                    exit 1
                    ;;
            esac
            ;;
        Darwin)
            case "$arch" in
                x86_64)
                    printf 'zig-macos-x86_64-%s.tar.xz\n' "$ZIG_VERSION"
                    ;;
                arm64|aarch64)
                    printf 'zig-macos-aarch64-%s.tar.xz\n' "$ZIG_VERSION"
                    ;;
                *)
                    echo "error: unsupported architecture '$arch' for automatic zig download" >&2
                    exit 1
                    ;;
            esac
            ;;
        *)
            echo "error: unsupported operating system '$os' for automatic zig download" >&2
            exit 1
            ;;
    esac
}

ensure_archive() {
    archive=$(resolve_archive_name)
    mkdir -p "$CACHE_DIR"
    archive_path="${CACHE_DIR}/${archive}"
    if [ ! -f "$archive_path" ]; then
        tmp_path="${archive_path}.tmp"
        rm -f "$tmp_path"
        urls="https://ziglang.org/download/${ZIG_VERSION}/${archive}"
        urls="$urls https://ziglang.org/builds/${archive}"
        urls="$urls https://ziglang.org/download/${archive}"
        for url in $urls; do
            if [ -z "$url" ]; then
                continue
            fi
            if download_archive "$url" "$tmp_path"; then
                mv "$tmp_path" "$archive_path"
                break
            fi
        done
        if [ ! -f "$archive_path" ]; then
            echo "error: unable to download zig archive for ${archive}" >&2
            echo "hint: install a native cross compiler (for example, aarch64-linux-gnu-gcc) or set the ZIG environment variable" >&2
            exit 1
        fi
    fi
    echo "$archive_path"
}

download_archive() {
    url=$1
    destination=$2
    if command -v curl >/dev/null 2>&1; then
        if curl -fsSL "$url" -o "$destination"; then
            return 0
        fi
    fi
    if command -v wget >/dev/null 2>&1; then
        if wget -q "$url" -O "$destination"; then
            return 0
        fi
    fi
    rm -f "$destination"
    return 1
}

extract_archive() {
    archive_path=$1
    dir_name=$(basename "$archive_path" .tar.xz)
    target_dir="${CACHE_DIR}/${dir_name}"
    if [ ! -x "${target_dir}/zig" ]; then
        rm -rf "$target_dir.tmp"
        mkdir -p "$target_dir.tmp"
        tar -xJf "$archive_path" -C "$target_dir.tmp"
        inner_dir=$(find "$target_dir.tmp" -mindepth 1 -maxdepth 1 -type d | head -n 1)
        if [ -z "$inner_dir" ]; then
            echo "error: unexpected layout in zig archive" >&2
            exit 1
        fi
        rm -rf "$target_dir"
        mv "$inner_dir" "$target_dir"
        rm -rf "$target_dir.tmp"
    fi
    printf '%s\n' "${target_dir}/zig"
}

archive=$(ensure_archive)
extract_archive "$archive"
