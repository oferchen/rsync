#!/usr/bin/env bash
# argwire-sweep.sh - per-argument client->server wire-string differential.
#
# For each CLI option supported by upstream rsync, run BOTH upstream rsync and
# oc-rsync as a local SENDER pushing to a remote RECEIVER over a recording
# remote shell. The remote shell records the exact server argv the client
# transmits (the `--server ...` token list = wire protocol), then we diff
# oc-rsync vs upstream per option.
#
# This isolates the client-side wire-string surface (server flag letters,
# long options, filter/order) deterministically across every argument, with
# no tcpdump dependency. Wire divergences here are real protocol divergences.
#
# Env:
#   UPSTREAM_RSYNC_BIN  path to upstream rsync   (default: rsync on PATH)
#   OC_RSYNC_BIN        path to oc-rsync         (default: oc-rsync on PATH)
#   OUT                 output dir               (default: /tmp/argwire.$$)
set -u

UP="${UPSTREAM_RSYNC_BIN:-$(command -v rsync)}"
OC="${OC_RSYNC_BIN:-$(command -v oc-rsync)}"
OUT="${OUT:-/tmp/argwire.$$}"
mkdir -p "$OUT"

# Recording remote shell: rsync execs `sh lsh <host> <server cmd...>`.
# Log every arg from `--server` onward (the wire-relevant portion), one token
# per line, then exit 0 so the client tears down quickly.
LSH="$OUT/lsh-record.sh"
cat > "$LSH" <<'EOF'
#!/bin/sh
seen=0
for a in "$@"; do
  if [ "$a" = "--server" ]; then seen=1; fi
  if [ "$seen" = "1" ]; then printf '%s\n' "$a" >> "$RECORD_LOG"; fi
done
exit 0
EOF
chmod +x "$LSH"

# Deterministic source tree.
SRC="$OUT/src"
rm -rf "$SRC"; mkdir -p "$SRC/sub"
i=0; while [ $i -lt 5 ]; do printf 'payload-%d\n' "$i" > "$SRC/f$i.txt"; i=$((i+1)); done
printf 'deep\n' > "$SRC/sub/deep.txt"
ln -sf f0.txt "$SRC/link" 2>/dev/null || true
# A merge filter file + files-from list for options that consume them.
printf -- '- *.tmp\n' > "$OUT/filt"
printf 'f0.txt\nsub/deep.txt\n' > "$OUT/fromlist"

run_one() {  # $1=bin  $2=tag  $3..=extra args
  local bin="$1" tag="$2"; shift 2
  export RECORD_LOG="$OUT/$tag.argv"
  : > "$RECORD_LOG"
  # --checksum-seed fixes any seed-derived bytes; -e uses recording shell.
  timeout 8 "$bin" -e "sh $LSH" --checksum-seed=1 "$@" \
    "$SRC/" "DUMMY:/dst/" >/dev/null 2>&1
}

# Value map: options that require an argument get a benign deterministic value.
val_for() {
  case "$1" in
    --block-size) echo "=2048" ;;
    --compress-level) echo "=3" ;;
    --compress-choice|--zc) echo "=zlib" ;;
    --checksum-choice|--cc) echo "=md5" ;;
    --skip-compress) echo "=gz" ;;
    --bwlimit) echo "=100" ;;
    --timeout) echo "=30" ;;
    --contimeout) echo "=30" ;;
    --modify-window) echo "=2" ;;
    --max-size) echo "=1m" ;;
    --min-size) echo "=1" ;;
    --max-delete) echo "=5" ;;
    --max-alloc) echo "=1m" ;;
    --partial-dir) echo "=.partial" ;;
    --temp-dir|-T) echo "=/tmp" ;;
    --backup-dir) echo "=/tmp/bak" ;;
    --suffix) echo "=.bak" ;;
    --chmod) echo "=F644" ;;
    --chown) echo "=0:0" ;;
    --usermap) echo "=0:0" ;;
    --groupmap) echo "=0:0" ;;
    --rsync-path) echo "=rsync" ;;
    --out-format|--log-format) echo "=%n" ;;
    --info) echo "=stats" ;;
    --debug) echo "=none" ;;
    --log-file) echo "=/tmp/cl.log" ;;
    --log-file-format) echo "=%n" ;;
    --exclude) echo "=*.tmp" ;;
    --include) echo "=*.txt" ;;
    --filter|-f) echo "=- *.tmp" ;;
    --exclude-from) echo "=$OUT/filt" ;;
    --include-from) echo "=$OUT/filt" ;;
    --files-from) echo "=$OUT/fromlist" ;;
    --compare-dest) echo "=/tmp/cd" ;;
    --copy-dest) echo "=/tmp/cpd" ;;
    --link-dest) echo "=/tmp/ld" ;;
    --port) echo "=873" ;;
    --sockopts) echo "=" ;;
    --address) echo "=0.0.0.0" ;;
    --protocol) echo "=32" ;;
    --iconv) echo "=utf8,latin1" ;;
    --outbuf) echo "=N" ;;
    --read-batch|--write-batch|--only-write-batch) echo "=/tmp/batch" ;;
    --stop-after) echo "=10" ;;
    --stop-at) echo "" ;;
    --copy-as) echo "" ;;
    *) echo "" ;;
  esac
}

# Options to skip: meta/daemon/local-only/exit-immediately/interactive.
skip_opt() {
  case "$1" in
    --help|--version|-h|-V|--daemon|--config|--server|--sender|--no-detail \
    |--dparam|--detach|--no-detach|--password-file|--early-input \
    |--stop-at|--copy-as|--sockopts|--address|--port|--rsh|-e \
    |--files-from|--from0|--list-only|--only-write-batch|--write-batch|--read-batch) return 0 ;;
  esac
  return 1
}

# Enumerate every long option upstream advertises.
opts=$("$UP" --help 2>&1 | grep -oE -- '--[a-zA-Z0-9][a-zA-Z0-9-]+' | sort -u)

echo "# argwire sweep" > "$OUT/report.txt"
echo "# upstream: $($UP --version 2>&1 | head -1)" >> "$OUT/report.txt"
echo "# oc-rsync: $($OC --version 2>&1 | head -1)" >> "$OUT/report.txt"
echo "" >> "$OUT/report.txt"

# Baseline -a first.
run_one "$UP" up_base -a
run_one "$OC" oc_base -a
if ! diff -q "$OUT/up_base.argv" "$OUT/oc_base.argv" >/dev/null; then
  echo "DIVERGE  (baseline -a)" >> "$OUT/report.txt"
  diff "$OUT/up_base.argv" "$OUT/oc_base.argv" | sed 's/^/    /' >> "$OUT/report.txt"
else
  echo "MATCH    (baseline -a)" >> "$OUT/report.txt"
fi

ndiv=0; nmatch=0; nskip=0
for o in $opts; do
  if skip_opt "$o"; then nskip=$((nskip+1)); continue; fi
  v=$(val_for "$o")
  arg="${o}${v}"
  run_one "$UP" "up_cur" -a "$arg"
  run_one "$OC" "oc_cur" -a "$arg"
  if [ ! -s "$OUT/up_cur.argv" ]; then
    # upstream itself produced no server string (option errored/exited); skip.
    echo "SKIP-noupstream  $arg" >> "$OUT/report.txt"; nskip=$((nskip+1)); continue
  fi
  if diff -q "$OUT/up_cur.argv" "$OUT/oc_cur.argv" >/dev/null; then
    nmatch=$((nmatch+1))
  else
    ndiv=$((ndiv+1))
    {
      echo "DIVERGE  $arg"
      diff "$OUT/up_cur.argv" "$OUT/oc_cur.argv" | sed 's/^/    /'
      echo ""
    } >> "$OUT/report.txt"
  fi
done

echo "" >> "$OUT/report.txt"
echo "SUMMARY: diverge=$ndiv match=$nmatch skip=$nskip" >> "$OUT/report.txt"
echo "OUTDIR=$OUT"
cat "$OUT/report.txt"
