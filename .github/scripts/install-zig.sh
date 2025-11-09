#!/usr/bin/env bash
# .github/scripts/install-zig.sh
# Installs a real, existing Zig toolchain on GitHub Actions runners.
# - Auto-detects OS/arch
# - Fetches https://ziglang.org/download/index.json and selects a valid build
# - Allows pinning via $ZIG_VERSION (e.g. ZIG_VERSION=0.13.0)
# - Exports the installed Zig to $GITHUB_PATH so later steps can use `zig`
# - Fails hard on any mismatch
set -euo pipefail

# You can pin in your workflow like:
#   env:
#     ZIG_VERSION: "0.13.0"
ZIG_VERSION="${ZIG_VERSION:-}"
ZIG_CHANNEL="${ZIG_CHANNEL:-stable}"
ZIG_BASE_URL="https://ziglang.org/download"
TMPDIR="${RUNNER_TEMP:-/tmp}"

detect_platform() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "${os}" in
    Linux)   os="linux" ;;
    Darwin)  os="macos" ;;
    MINGW*|MSYS*|CYGWIN*) os="windows" ;;
    *) echo "Unsupported OS: ${os}" >&2; exit 1 ;;
  esac

  case "${arch}" in
    x86_64|amd64) arch="x86_64" ;;
    aarch64|arm64) arch="aarch64" ;;
    *) echo "Unsupported arch: ${arch}" >&2; exit 1 ;;
  esac

  printf "%s %s\n" "${os}" "${arch}"
}

fetch_index_json() {
  # Single source of truth for what's actually available
  curl -fsSL "${ZIG_BASE_URL}/index.json"
}

select_version() {
  # args: index_json
  # env: ZIG_VERSION, ZIG_CHANNEL
  # output: version string
  python3 - <<'PY'
import os, sys, json
data = json.load(sys.stdin)
want_version = os.environ.get("ZIG_VERSION", "").strip()
channel = os.environ.get("ZIG_CHANNEL", "stable").strip()

if want_version:
    # specific version requested
    if want_version in data:
        print(want_version)
        sys.exit(0)
    else:
        sys.stderr.write(f"Requested ZIG_VERSION={want_version} not found in index.json\n")
        sys.exit(1)

# no explicit version -> use channel (stable, master, etc.)
if channel in data:
    # channel entries usually hold "version" + per-target URLs
    entry = data[channel]
    # for stable, data["stable"]["version"] is the actual version
    if "version" in entry:
        print(entry["version"])
        sys.exit(0)
    # fallback: the key itself
    print(channel)
    sys.exit(0)

sys.stderr.write(f"Channel {channel} not found in index.json\n")
sys.exit(1)
PY
}

download_and_install() {
  local version="$1"
  local os="$2"
  local arch="$3"

  # index.json layout: data[version][os-"arch"].tar.xz / .zip
  # We'll re-fetch index.json here to get the exact URL for this (version, os, arch)
  local index_json url filename dest_dir

  index_json="$(fetch_index_json)"

  # extract URL for this target with python to avoid fragile jq
  url="$(python3 - <<'PY'
import os, sys, json
data = json.loads("""'"${index_json}"'""")
version = os.environ["ZIG_VER"]
os_ = os.environ["ZIG_OS"]
arch = os.environ["ZIG_ARCH"]

entry = data.get(version)
if not entry:
    sys.stderr.write(f"Version {version} missing from index.json\n")
    sys.exit(1)

# keys look like: linux-x86_64, macos-aarch64, windows-x86_64
key = f"{os_}-{arch}"
target = entry.get(key)
if not target:
    sys.stderr.write(f"No build for {version} on {os_}-{arch}\n")
    sys.exit(1)

# tarball is usually under "tarball" or "xz" depending on platform
url = target.get("tarball") or target.get("xz") or target.get("zip")
if not url:
    sys.stderr.write(f"No downloadable artifact for {version} on {os_}-{arch}\n")
    sys.exit(1)

print(url)
PY
)" || exit 1

  filename="${url##*/}"
  dest_dir="${HOME}/.local/zig/${version}"

  mkdir -p "${dest_dir}"
  echo "Downloading Zig ${version} for ${os}/${arch}..."
  curl -fsSL "${url}" -o "${TMPDIR}/${filename}"

  case "${filename}" in
    *.tar.xz|*.tar.gz)
      tar -C "${dest_dir}" --strip-components=1 -xf "${TMPDIR}/${filename}"
      ;;
    *.zip)
      unzip -q "${TMPDIR}/${filename}" -d "${dest_dir}"
      # some windows zips unpack to zig-windows-<ver>/...
      if [ -d "${dest_dir}/zig-windows-${version}" ]; then
        mv "${dest_dir}/zig-windows-${version}/"* "${dest_dir}/"
        rmdir "${dest_dir}/zig-windows-${version}"
      fi
      ;;
    *)
      echo "Unknown Zig archive format: ${filename}" >&2
      exit 1
      ;;
  esac

  # expose Zig to later steps
  if [ -n "${GITHUB_PATH:-}" ]; then
    printf '%s\n' "${dest_dir}" >> "${GITHUB_PATH}"
  else
    echo "Add ${dest_dir} to your PATH"
  fi

  # quick sanity
  "${dest_dir}/zig" version
}

main() {
  read os arch < <(detect_platform)
  index_json="$(fetch_index_json)"
  # select version using index.json
  ZIG_VER="$(printf '%s' "${index_json}" | select_version)"
  export ZIG_VER
  export ZIG_OS="${os}"
  export ZIG_ARCH="${arch}"

  download_and_install "${ZIG_VER}" "${os}" "${arch}"
}

main "$@"

