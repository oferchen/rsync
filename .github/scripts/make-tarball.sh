#!/usr/bin/env bash
set -euo pipefail

TARGET="$1"
OUTNAME="$2"
BINARIES="${3:-oc-rsync}"

OUTDIR="dist/${TARGET}"
rm -rf "${OUTDIR}"
mkdir -p "${OUTDIR}"

for bin in $BINARIES; do
  case "$TARGET" in
    *windows*)
      SRC="target/${TARGET}/release/${bin}.exe"
      ;;
    *)
      SRC="target/${TARGET}/release/${bin}"
      ;;
  esac
  cp "${SRC}" "${OUTDIR}/"
done

ARCHIVE_PATH="dist/${OUTNAME}"
mkdir -p "$(dirname "${ARCHIVE_PATH}")"
tar -czf "${ARCHIVE_PATH}" -C "${OUTDIR}" .

