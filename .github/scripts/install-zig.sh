#!/usr/bin/env bash
# .github/scripts/install-zig.sh
# Pure-shell Zig installer for CI (no inline Python)
# - avoids parsing index.json
# - constructs URL directly
# - allows pin via ZIG_VERSION env
# - tested for Linux/macOS on GitHub runners
set -euo pipefail

ZIG_BASE_URL="https://ziglang.org/download"
# pick a version that actually exists on ziglang.org
ZIG_VERSION="${ZIG_VERSION:-0.13.0}"
TMPDIR="${RUNNER_TEMP:-/tmp}"

detect_platform() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"

    case "${os}" in
        Linux)  os="linux" ;;
        Darwin) os="macos" ;;
        MINGW*|MSYS*|CYGWIN*) os="windows" ;;
        *) echo "Unsupported OS: ${os}" >&2; exit 1 ;;
    esac

    case "${arch}" in
        x86_64|amd64) arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *) echo "Unsupported arch: ${arch}" >&2; exit 1 ;;
    esac

    printf '%s %s\n' "${os}" "${arch}"
}

build_url() {
    # args: os arch version
    # linux/macos -> .tar.xz ; windows -> .zip
    local os="$1" arch="$2" version="$3" ext="tar.xz"
    if [ "${os}" = "windows" ]; then
        ext="zip"
    fi
    printf '%s/%s/zig-%s-%s-%s.%s' \
        "${ZIG_BASE_URL}" \
        "${version}" \
        "${os}" \
        "${arch}" \
        "${version}" \
        "${ext}"
}

download_and_install() {
    local url="$1" version="$2"
    local dest_dir="${HOME}/.local/zig/${version}"
    local filename="${url##*/}"

    mkdir -p "${dest_dir}"
    echo "Downloading Zig ${version} from ${url} ..."
    curl -fsSL --retry 3 --retry-delay 2 "${url}" -o "${TMPDIR}/${filename}"

    case "${filename}" in
        *.tar.xz|*.tar.gz)
            tar -C "${dest_dir}" --strip-components=1 -xf "${TMPDIR}/${filename}"
            ;;
        *.zip)
            # windows / zip path
            unzip -q "${TMPDIR}/${filename}" -d "${dest_dir}"
            # normalize if zip created a subdir
            if [ -d "${dest_dir}/zig-windows-${version}" ]; then
                mv "${dest_dir}/zig-windows-${version}/"* "${dest_dir}/"
                rmdir "${dest_dir}/zig-windows-${version}" || true
            fi
            ;;
        *)
            echo "Unknown archive format: ${filename}" >&2
            exit 1
            ;;
    esac

    # expose to later steps
    if [ -n "${GITHUB_PATH:-}" ]; then
        printf '%s\n' "${dest_dir}" >> "${GITHUB_PATH}"
    fi

    # quick sanity
    "${dest_dir}/zig" version
}

main() {
    read os arch < <(detect_platform)
    url="$(build_url "${os}" "${arch}" "${ZIG_VERSION}")"
    download_and_install "${url}" "${ZIG_VERSION}"
}

main "$@"

