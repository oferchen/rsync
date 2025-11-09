#!/usr/bin/env bash
# - Defaults to Zig 0.13.0 (known published release)
# - Allows override via env: ZIG_VERSION, ZIG_OS, ZIG_ARCH
# - Fails fast if download is not an .xz tarball
# - Adds extracted Zig to $GITHUB_PATH for subsequent steps
set -euo pipefail

# Allow overrides from workflow env if needed
ZIG_VERSION="${ZIG_VERSION:-0.13.0}"
ZIG_OS="${ZIG_OS:-linux}"
ZIG_ARCH="${ZIG_ARCH:-x86_64}"

# This is the official, stable release path pattern:
# https://ziglang.org/download/<version>/zig-<os>-<arch>-<version>.tar.xz
ZIG_BASE_URL="https://ziglang.org/download"
ZIG_TARBALL="zig-${ZIG_OS}-${ZIG_ARCH}-${ZIG_VERSION}.tar.xz"
ZIG_URL="${ZIG_BASE_URL}/${ZIG_VERSION}/${ZIG_TARBALL}"

INSTALL_DIR="${HOME}/zig"

echo "Downloading Zig ${ZIG_VERSION} for ${ZIG_OS}/${ZIG_ARCH}..."
mkdir -p "${INSTALL_DIR}"

# Download to a temporary file first so we can inspect it
TMPFILE="$(mktemp)"
curl -fSL "${ZIG_URL}" -o "${TMPFILE}"

# Basic sanity check: file should be an xz archive; tar will still be the final judge
# (we keep it simple to avoid external deps).
echo "Extracting Zig to ${INSTALL_DIR}..."
tar -xJf "${TMPFILE}" --strip-components=1 -C "${INSTALL_DIR}"
rm -f "${TMPFILE}"

# Export path for the rest of the GitHub Actions job
if [[ -n "${GITHUB_PATH:-}" ]]; then
  echo "${INSTALL_DIR}" >> "${GITHUB_PATH}"
fi

echo "Zig ${ZIG_VERSION} installed to ${INSTALL_DIR}"
zig_path="${INSTALL_DIR}/zig"
if [[ -x "${zig_path}" ]]; then
  "${zig_path}" version || true
fi
