#!/usr/bin/env bash
set -euo pipefail

TARGET="$1"
OUTNAME="$2"
BINARIES="${3:-oc-rsync}"

OUTDIR="dist/${TARGET}"
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

tar -czf "${OUTNAME}" -C "${OUTDIR}" .

