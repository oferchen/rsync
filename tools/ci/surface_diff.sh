#!/usr/bin/env bash
# Mechanically diff enumerable "surfaces" between upstream rsync 3.4.4 and
# oc-rsync, so an audit can show it covered every member of a table instead of
# asserting that it did.
#
# A surface is any list upstream defines exhaustively in one place: daemon
# config directives, command-line options, syslog facilities, exit codes. For
# each, we extract upstream's members and oc's, then diff. Members upstream has
# and oc lacks are candidate gaps. Members oc has and upstream lacks are
# candidate extensions - deliberate ones are declared in the registry (see
# ACCEPTED below), undeclared ones are undocumented divergence.
#
# The point is completeness evidence. A hand audit that reads "most" of a table
# and finds nothing is indistinguishable from one that read all of it; this is
# not. Every extraction here is untruncated by construction - no head, no tail.
#
# Usage:
#   surface_diff.sh                 report every surface
#   surface_diff.sh <name> ...      report only the named surfaces
#   surface_diff.sh --list          list surface names
#   surface_diff.sh --check         exit 1 on any unaccepted divergence
#   surface_diff.sh --self-test     prove the extractors detect known answers
#   surface_diff.sh --delta A B     what upstream release A -> B added/removed
#
# The oracle release defaults to UPSTREAM_VERSION and can be overridden per
# invocation. A version migration therefore re-points these extractors instead
# of growing a second, ad-hoc copy of them somewhere else - see --delta below
# for why that distinction is the whole point.
set -euo pipefail

# Every surface funnels through sort/comm. comm silently reports every line
# as unique when the two inputs were sorted under different collations, so the
# whole script pins the C locale once instead of trusting each call site.
export LC_ALL=C

REPO_ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
UPSTREAM_VERSION="${UPSTREAM_VERSION:-3.4.4}"

# The single place that knows where a release's source lives. Every question -
# the oc-vs-upstream diff, the release-to-release delta, the self-test - resolves
# its oracle through here, so moving the pin is one assignment.
upstream_src() { printf '%s/target/interop/upstream-src/rsync-%s' "$REPO_ROOT" "$1"; }

UPSTREAM="$(upstream_src "$UPSTREAM_VERSION")"

# Divergences that are known and deliberate. Each entry is
# "<surface>:<side>:<member>" where side is `upstream_only` (a real gap we have
# chosen not to close yet) or `oc_only` (a declared oc extension). Anything not
# listed here fails --check, so new drift is loud and known state is quiet.
ACCEPTED_FILE="${REPO_ROOT}/tools/ci/surface_diff.accepted"

die() { printf 'surface_diff: %s\n' "$*" >&2; exit 2; }

# Named so both the default oracle and any --delta operand report the same way.
require_upstream() { # version
  [ -d "$(upstream_src "$1")" ] || die "upstream source missing at $(upstream_src "$1")
Fetch it with:
  mkdir -p target/interop/upstream-src && cd target/interop/upstream-src
  curl -L https://download.samba.org/pub/rsync/src/rsync-$1.tar.gz | tar xz"
}

require_upstream "$UPSTREAM_VERSION"

# --- surface: daemon-directives -------------------------------------------
# Upstream generates its daemon parameter table from daemon-parm.txt. The TYPE
# name field is `varname|pubname`; daemon-parm.awk takes the part after `|` as
# the directive an operator writes, with `_` rendered as a space. Entries with
# no `|` use the variable name directly.
#
# oc stores directive keys lowercased and whitespace-folded (`refuse options`
# becomes `refuseoptions`), so both sides are folded the same way before
# comparison. Directive names contain hyphens (`pre-xfer exec`) - a [a-z0-9]+
# pattern silently drops those, which is why the class below includes `-`.
up_daemon_directives() {
  awk '$1 ~ /^(STRING|CHAR|PATH|INTEGER|ENUM|OCTAL|BOOL|BOOLREV|BOOL3)$/ {
         n = $2; sub(/.*\|/, "", n); gsub(/_/, "", n); print tolower(n)
       }' "${UPSTREAM}/daemon-parm.txt" | sort -u
}

# oc's real dispatch lives in config_parsing/. module_parsing/ holds only a
# fraction of the names and reads as a huge false gap if used here.
oc_daemon_directives() {
  local dir="${REPO_ROOT}/crates/daemon/src/daemon/sections/config_parsing"
  grep -ohE '^[[:space:]]+"[a-z0-9-]+"' \
    "${dir}/module_directives.rs" \
    "${dir}/global_directives/dispatch.rs" \
    | sed -E 's/.*"([a-z0-9-]+)".*/\1/' | sort -u
}

# --- surface: cli-long-options --------------------------------------------
# Upstream's popt table. Rows whose longName is NULL carry only a short letter
# and are handled by cli-short-options instead.
up_cli_long_options() {
  awk '/^static struct poptOption long_options\[\]/,/^\};/' "${UPSTREAM}/options.c" \
    | grep -oE '^ *\{ *"[^"]+"' | sed -E 's/^ *\{ *"//; s/"$//' | sort -u
}

# oc builds its table with clap. `.long()` alone misses ~42 aliases (cc, del,
# i-r, zl, no-8 ...) and reports them as missing upstream options, so the
# alias forms are unioned in.
oc_cli_long_options() {
  grep -rhoE '\.(long|alias|visible_alias)\("[^"]+"\)' "${REPO_ROOT}/crates/cli/src" \
    | sed -E 's/.*\("//; s/"\)//' | sort -u
}

# --- surface: cli-short-options -------------------------------------------
# The shortName column. Rows with a NULL longName (D, F, P) matter here, so the
# pattern accepts either a quoted longName or a bare 0.
up_cli_short_options() {
  awk '/^static struct poptOption long_options\[\]/,/^\};/' "${UPSTREAM}/options.c" \
    | grep -oE "^ *\{ *(\"[^\"]*\"|0) *, *'.'" \
    | sed -E "s/.*'(.)'/\1/" | sort -u
}

oc_cli_short_options() {
  grep -rhoE "\.short\('.'\)" "${REPO_ROOT}/crates/cli/src" \
    | sed -E "s/.*'(.)'.*/\1/" | sort -u
}

# --- surface: syslog-facilities -------------------------------------------
# Upstream's table is #ifdef-guarded per platform (LOG_AUTHPRIV, LOG_FTP), so a
# name missing from oc is a portability question, not automatically a defect.
up_syslog_facilities() {
  awk '/enum_syslog_facility\[\] = \{/,/^\};/' "${UPSTREAM}/loadparm.c" \
    | grep -oE '"[a-z0-9]+"' | tr -d '"' | sort -u
}

oc_syslog_facilities() {
  grep -rhoE '"[a-z0-9]+" *=>' "${REPO_ROOT}/crates/logging-sink/src" \
    | sed -E 's/"([a-z0-9]+)".*/\1/' | sort -u
}

# --- surface: debug-words ---------------------------------------------------
# Upstream's --debug word table (options.c debug_words[]). The awk range keeps
# the extraction inside the table so the DEBUG_WORD macro definition above it
# cannot contaminate the list with its formal parameter.
#
# oc's table is a SUBSET of the accepted wire surface by construction: the
# server accepts unknown words unconditionally (flags/debug.rs apply_with_mode,
# upstream options.c:484), so a green diff here proves the FORWARDING table
# matches upstream - not that every word changes behaviour. The four oc
# extension words (iouring/clone/sockopt/iocp) are declared in
# surface_diff.accepted; they ride the same unconditional-forward path, so
# they are wire-safe against upstream peers.
up_debug_words() {
  awk '/^static struct output_struct debug_words\[/,/^\};/' "${UPSTREAM}/options.c" \
    | sed -n -E 's/.*DEBUG_WORD\(([A-Za-z0-9_]+),.*/\1/p' | tr '[:upper:]' '[:lower:]' | sort -u
}

oc_debug_words() {
  awk '/fn entries\(&self\)/,/^    \}/' \
    "${REPO_ROOT}/crates/cli/src/frontend/execution/flags/debug.rs" \
    | sed -n -E 's/^ *\("([a-z0-9]+)", self\..*/\1/p' | sort -u
}

# --- surface: info-words ----------------------------------------------------
# Upstream's --info word table (options.c info_words[]). The oc side reads
# INFO_FLAG_SPECS - the accepted-words table - rather than the forwarding
# list, so an accepted-but-unforwardable word would still be caught.
up_info_words() {
  awk '/^static struct output_struct info_words\[/,/^\};/' "${UPSTREAM}/options.c" \
    | sed -n -E 's/.*INFO_WORD\(([A-Za-z0-9_]+),.*/\1/p' | tr '[:upper:]' '[:lower:]' | sort -u
}

oc_info_words() {
  awk '/const INFO_FLAG_SPECS/,/^\];/' \
    "${REPO_ROOT}/crates/cli/src/frontend/execution/flags/info.rs" \
    | sed -n -E 's/.*name: "([a-z0-9]+)".*/\1/p' | sort -u
}

# --- surface: exit-codes ----------------------------------------------------
# Members are "code:description" pairs so a renumbered code and a reworded
# string both surface. Upstream splits the surface across two files - the
# numbers live in errcode.h, the strings in log.c rerr_names[] - so the awk
# joins them by RERR_* name. oc keeps both halves in ExitCode (codes.rs);
# joining as_i32() to description() through the variant name mirrors the same
# two-table join. ExitCode::Other is a passthrough for unrecognised raw
# statuses, not a table row, and matches neither pattern.
#
# The two oc-only rows are declared in surface_diff.accepted: upstream's table
# has no row for 0 (log_exit() special-cases success before consulting it) and
# none for 6 (errcode.h defines no RERR_* with value 6 - the "daemon unable to
# append to log-file" description exists only in rsync.1.md:4671).
up_exit_codes() {
  awk '
    FNR == NR { if ($1 == "#define" && $2 ~ /^RERR_/) code[$2] = $3; next }
    /rerr_names\[\] = \{/ { t = 1; next }
    t && /\{ 0, NULL \}/ { t = 0 }
    t && match($0, /RERR_[A-Z0-9_]+/) {
      n = substr($0, RSTART, RLENGTH)
      s = $0; sub(/^[^"]*"/, "", s); sub(/".*/, "", s)
      print code[n] ":" s
    }
  ' "${UPSTREAM}/errcode.h" "${UPSTREAM}/log.c" | sort -u
}

oc_exit_codes() {
  awk '
    /pub const fn as_i32/ { sect = 1 }
    /pub const fn description/ { sect = 2 }
    sect == 1 && match($0, /Self::[A-Za-z0-9]+ => [0-9]+,/) {
      pair = substr($0, RSTART, RLENGTH)
      name = pair; sub(/^Self::/, "", name); sub(/ =>.*/, "", name)
      val = pair; sub(/.*=> /, "", val); sub(/,$/, "", val)
      code[name] = val
    }
    sect == 2 && match($0, /Self::[A-Za-z0-9]+ => "/) {
      name = $0; sub(/.*Self::/, "", name); sub(/ =>.*/, "", name)
      s = $0; sub(/^[^"]*"/, "", s); sub(/".*/, "", s)
      if (name in code) print code[name] ":" s
    }
  ' "${REPO_ROOT}/crates/core/src/exit_code/codes.rs" | sort -u
}

# --- surface: msg-codes -----------------------------------------------------
# Multiplexed message codes (rsync.h enum msgcode vs envelope/message_code.rs).
# Compared by VALUE only: the names do not map mechanically (upstream MSG_NOOP
# is oc NoOp, not No_Op). Half of upstream's entries are spelled as aliases of
# enum logcode members (MSG_INFO=FINFO), so the logcode enum is parsed first
# and every alias resolved through it - a naive numeric grep finds only the
# literal rows. An alias that fails to resolve prints UNRESOLVED and aborts
# rather than shrinking the list silently.
up_msg_codes() {
  awk '
    /^enum logcode \{/ { lc = 1 }
    lc {
      line = $0
      while (match(line, /[A-Z][A-Z0-9_]*=[0-9]+/)) {
        pair = substr(line, RSTART, RLENGTH); eq = index(pair, "=")
        logv[substr(pair, 1, eq - 1)] = substr(pair, eq + 1)
        line = substr(line, RSTART + RLENGTH)
      }
    }
    lc && /\};/ { lc = 0 }
    /^enum msgcode \{/ { mc = 1; next }
    mc && /\};/ { mc = 0 }
    mc {
      line = $0
      while (match(line, /MSG_[A-Z0-9_]+=[A-Za-z0-9_]+/)) {
        pair = substr(line, RSTART, RLENGTH); eq = index(pair, "=")
        v = substr(pair, eq + 1)
        if (v ~ /^[0-9]+$/) print v
        else if (v in logv) print logv[v]
        else { print "UNRESOLVED:" v; exit 1 }
        line = substr(line, RSTART + RLENGTH)
      }
    }
  ' "${UPSTREAM}/rsync.h" | sort -u
}

oc_msg_codes() {
  awk '/^pub enum MessageCode \{/,/^\}/' \
    "${REPO_ROOT}/crates/protocol/src/envelope/message_code.rs" \
    | sed -n -E 's/^ *[A-Za-z0-9]+ = ([0-9]+),$/\1/p' | sort -u
}

# --- surface: compat-flag-bits ----------------------------------------------
# Compatibility-flag bit positions. The source is compat.c - the CF_* macros
# live there, NOT in rsync.h. Compared by BIT POSITION only: three names
# differ in spelling (SAFE_FLIST/SAFE_FILE_LIST, AVOID_XATTR_OPTIM/
# AVOID_XATTR_OPTIMIZATION, CHKSUM_SEED_FIX/CHECKSUM_SEED_FIX). On the oc
# side only the `1 << n` upstream-mirroring constants are members: EMPTY is
# Self::new(0), and the private oc extension CONSECUTIVE_MATCH is
# Self::new(0x0200_0000) - deliberately outside KNOWN_MASK and this surface;
# the self-test pins its exclusion so a spelling change cannot smuggle it in.
up_compat_flag_bits() {
  sed -n -E 's/^#define CF_[A-Z0-9_]+[[:space:]]+\(1<<([0-9]+)\)/\1/p' \
    "${UPSTREAM}/compat.c" | sort -u
}

oc_compat_flag_bits() {
  sed -n -E 's/^ *pub const [A-Z0-9_]+: Self = Self::new\(1 << ([0-9]+)\);$/\1/p' \
    "${REPO_ROOT}/crates/protocol/src/compatibility/flags.rs" | sort -u
}

# --- surface: socket-options ------------------------------------------------
# The `socket options` name table (socket.c socket_options[] vs the
# lookup_socket_option() match). Two independent guards: the awk range pins
# the enclosing structure, and the `=> Some(`/`{"..."` arm pattern pins the
# row shape - intern_name() in the same oc file repeats all twelve names with
# a different arm shape and must not double-count them.
up_socket_options() {
  awk '/^\} socket_options\[\] = \{/,/\{NULL,0,0,0,0\}/' "${UPSTREAM}/socket.c" \
    | sed -n -E 's/^ *\{"([A-Z0-9_]+)",.*/\1/p' | sort -u
}

oc_socket_options() {
  awk '/fn lookup_socket_option/,/^\}/' \
    "${REPO_ROOT}/crates/core/src/client/module_list/socket_options/lookup.rs" \
    | sed -n -E 's/^ *"([A-Z0-9_]+)" => Some\(.*/\1/p' | sort -u
}

# --- surface: checksum-names ------------------------------------------------
# Negotiable checksum names (checksum.c valid_checksums_items[] vs
# SUPPORTED_CHECKSUMS). The upstream table needs an alias collapse: "xxhash"
# shares CSUM_XXH64 with "xxh64", and get_default_nno_list() (compat.c:480-483)
# `continue`s past duplicate nums, so upstream never advertises it. Keeping
# the first name per CSUM_* constant reproduces that; without the collapse the
# raw table reports a phantom "xxhash" gap. oc accepts the alias on parse
# (ChecksumAlgorithm::parse) but never advertises it, same as upstream.
up_checksum_names() {
  awk '/^struct name_num_item valid_checksums_items\[\] = \{/,/^\};/' "${UPSTREAM}/checksum.c" \
    | awk 'match($0, /CSUM_[A-Z0-9_]+/) {
        c = substr($0, RSTART, RLENGTH)
        s = $0; sub(/^[^"]*"/, "", s); sub(/".*/, "", s)
        if (!(c in seen)) { seen[c] = 1; print s }
      }' | sort -u
}

oc_checksum_names() {
  awk '/const SUPPORTED_CHECKSUMS/,/\];/' \
    "${REPO_ROOT}/crates/protocol/src/negotiation/capabilities/algorithms.rs" \
    | grep -oE '"[a-z0-9]+"' | tr -d '"' | sort -u
}

# --- surface: compression-names ---------------------------------------------
# Negotiable compression names (compat.c valid_compressions_items[] vs
# supported_compressions()). The `grep -v '#\[cfg'` is MANDATORY: the oc
# function's feature gates spell the codec names inside attribute text
# (`#[cfg(feature = "zstd")]`), so without it attribute strings contribute
# phantom members - today's names are shadowed by the real rows, but a gated
# name with no matching push would appear as a member that does not exist.
up_compression_names() {
  awk '/^struct name_num_item valid_compressions_items\[\] = \{/,/^\};/' "${UPSTREAM}/compat.c" \
    | grep -oE '"[a-z0-9]+"' | tr -d '"' | sort -u
}

oc_compression_names() {
  awk '/^pub\(super\) fn supported_compressions/,/^\}/' \
    "${REPO_ROOT}/crates/protocol/src/negotiation/capabilities/algorithms.rs" \
    | grep -v '#\[cfg' | grep -oE '"[a-z0-9]+"' | tr -d '"' | sort -u
}

# --- registry --------------------------------------------------------------
SURFACES=(daemon-directives cli-long-options cli-short-options syslog-facilities
  debug-words info-words exit-codes msg-codes compat-flag-bits socket-options
  checksum-names compression-names)

fn_name() { printf '%s_%s' "$1" "$(printf '%s' "$2" | tr '-' '_')"; }

accepted() { # surface side member
  [ -f "$ACCEPTED_FILE" ] || return 1
  grep -qxF "$1:$2:$3" <(grep -v '^[[:space:]]*#' "$ACCEPTED_FILE" | sed '/^[[:space:]]*$/d')
}

# Lane B. oc's deliberate CLI extensions are already declared in a test-enforced
# registry, so an oc-only option that appears there is declared, not divergence.
# This is the whole point of consulting the registry rather than inventing a
# second list: the two cannot drift apart, because the help test pins one of
# them and this pins the other against it.
#
# upstream: none - this is an oc-only concept.
oc_extension_registry() {
  awk '/const OC_EXTENSION_FLAGS/,/^\];/' \
    "${REPO_ROOT}/crates/cli/src/frontend/tests/help.rs" \
    | grep -oE '"--[a-z0-9-]+"' | sed -E 's/"--([a-z0-9-]+)"/\1/' | sort -u
}

registry_allows() { # surface member
  [ "$1" = "cli-long-options" ] || return 1
  oc_extension_registry | grep -qxF "$2"
}

report_surface() { # name -> prints report, sets DIVERGED
  local name="$1" up oc line n_up n_oc
  up="$(mktemp)"; oc="$(mktemp)"
  trap 'rm -f "$up" "$oc"' RETURN

  "$(fn_name up "$name")" > "$up"
  "$(fn_name oc "$name")" > "$oc"
  n_up=$(wc -l < "$up" | tr -d ' '); n_oc=$(wc -l < "$oc" | tr -d ' ')

  printf '\n== %s ==\n' "$name"
  printf 'upstream: %s members    oc: %s members\n' "$n_up" "$n_oc"

  [ "$n_up" -gt 0 ] || { printf 'ERROR: upstream extraction returned nothing - the extractor is broken, not the code\n'; DIVERGED=1; return; }
  [ "$n_oc" -gt 0 ] || { printf 'ERROR: oc extraction returned nothing - the extractor is broken, not the code\n'; DIVERGED=1; return; }

  while IFS= read -r line; do
    [ -n "$line" ] || continue
    if accepted "$name" upstream_only "$line"; then
      printf '  (accepted) upstream-only: %s\n' "$line"
    else
      printf '  GAP  upstream has, oc lacks: %s\n' "$line"; DIVERGED=1
    fi
  done < <(comm -23 "$up" "$oc")

  while IFS= read -r line; do
    [ -n "$line" ] || continue
    if registry_allows "$name" "$line"; then
      printf '  (registered) oc extension: %s\n' "$line"
    elif accepted "$name" oc_only "$line"; then
      printf '  (accepted) oc extension: %s\n' "$line"
    else
      printf '  EXT  oc-only and UNREGISTERED: %s\n' "$line"; DIVERGED=1
    fi
  done < <(comm -13 "$up" "$oc")
}

# --- self-test -------------------------------------------------------------
# An extractor that returns an empty list "finds no gaps" and looks like a pass.
# These cases are chosen because each one has already been wrong in this repo:
# a truncated search reported an implemented directive as missing, and a
# too-narrow character class dropped every hyphenated directive.
self_test() {
  local fails=0
  check() { # description expected actual
    if [ "$2" = "$3" ]; then printf '  ok    %s\n' "$1"
    else printf '  FAIL  %s (expected %s, got %s)\n' "$1" "$2" "$3"; fails=$((fails + 1)); fi
  }

  printf 'self-test: extractors must produce known-correct answers\n'

  # No extractor may return an empty list - that is the failure mode that reads
  # as a clean result.
  local s n
  for s in "${SURFACES[@]}"; do
    n=$("$(fn_name up "$s")" | wc -l | tr -d ' ')
    [ "$n" -gt 0 ] && printf '  ok    %s: upstream extraction non-empty (%s)\n' "$s" "$n" \
      || { printf '  FAIL  %s: upstream extraction empty\n' "$s"; fails=$((fails + 1)); }
    n=$("$(fn_name oc "$s")" | wc -l | tr -d ' ')
    [ "$n" -gt 0 ] && printf '  ok    %s: oc extraction non-empty (%s)\n' "$s" "$n" \
      || { printf '  FAIL  %s: oc extraction empty\n' "$s"; fails=$((fails + 1)); }
  done

  # Positive control: `refuse options` IS implemented. An audit once reported it
  # missing because a truncated grep hid the implementation. If this fails, the
  # extractor produces false gaps.
  check "daemon-directives finds implemented 'refuse options'" \
    "refuseoptions" "$(oc_daemon_directives | grep -x 'refuseoptions' || true)"

  # Hyphen guard: upstream spells it `pre-xfer_exec` - literal hyphen, `_` for
  # the space. oc folds whitespace only and keeps the hyphen, so the folded form
  # is `pre-xferexec` on both sides. A [a-z0-9]+ class on either side drops all
  # three hyphenated directives silently.
  check "daemon-directives keeps hyphenated 'pre-xfer exec' (upstream)" \
    "pre-xferexec" "$(up_daemon_directives | grep -x 'pre-xferexec' || true)"
  check "daemon-directives keeps hyphenated 'pre-xfer exec' (oc)" \
    "pre-xferexec" "$(oc_daemon_directives | grep -x 'pre-xferexec' || true)"

  # Alias guard: `--cc` is a visible alias for --checksum-choice. A .long()-only
  # extraction reports it as an upstream option oc lacks.
  check "cli-long-options keeps clap aliases (cc)" \
    "cc" "$(oc_cli_long_options | grep -x 'cc' || true)"

  # NULL-longName guard: -P has no long name in the popt table.
  check "cli-short-options keeps NULL-longName rows (P)" \
    "P" "$(up_cli_short_options | grep -x 'P' || true)"

  # Macro-parameter guard: if the debug_words[] awk range broke, the sed would
  # capture the DEBUG_WORD macro definition's formal parameter as a member.
  check "debug-words excludes the DEBUG_WORD macro definition" \
    "" "$(up_debug_words | grep -x 'name' || true)"
  check "debug-words finds 'deltasum' (upstream)" \
    "deltasum" "$(up_debug_words | grep -x 'deltasum' || true)"
  check "debug-words keeps declared oc extension 'iouring'" \
    "iouring" "$(oc_debug_words | grep -x 'iouring' || true)"

  # nonreg is the one always-on info word (priority group 0); losing it means
  # the INFO_FLAG_SPECS range or the name capture broke.
  check "info-words finds 'nonreg' (upstream)" \
    "nonreg" "$(up_info_words | grep -x 'nonreg' || true)"
  check "info-words finds 'nonreg' (oc)" \
    "nonreg" "$(oc_info_words | grep -x 'nonreg' || true)"

  # The two-file join must carry both the number and the exact string; row 23
  # has the longest string and parentheses, so it breaks first.
  check "exit-codes joins errcode.h numbers to log.c strings (23)" \
    "23:some files/attrs were not transferred (see previous errors)" \
    "$(up_exit_codes | grep -x '23:.*' || true)"
  # Upstream has NO row for code 6 - errcode.h defines no RERR_* with that
  # value. This pins the reason the oc-only row is allowlisted; if an upstream
  # release ever adds one, the allowlist entry must be re-examined.
  check "exit-codes: upstream defines no code-6 row" \
    "" "$(up_exit_codes | grep '^6:' || true)"
  check "exit-codes: oc declares the man-page-only code-6 row" \
    "6:daemon unable to append to log-file" "$(oc_exit_codes | grep '^6:' || true)"

  # Alias guard: MSG_CLIENT is spelled as an alias of FCLIENT (=7). A naive
  # numeric grep misses every logcode-aliased row and finds 8 of 18 values.
  check "msg-codes resolves logcode aliases (FCLIENT=7)" \
    "7" "$(up_msg_codes | grep -x '7' || true)"
  check "msg-codes upstream extraction has all 18 values" \
    "18" "$(up_msg_codes | wc -l | tr -d ' ')"

  # Bit-position guard: ID0_NAMES is the highest upstream bit; the private oc
  # extension CONSECUTIVE_MATCH (0x0200_0000, bit 25) is written in hex
  # precisely so it stays outside the 1<<n upstream-mirroring surface.
  check "compat-flag-bits finds bit 8 (upstream)" \
    "8" "$(up_compat_flag_bits | grep -x '8' || true)"
  check "compat-flag-bits excludes the private CONSECUTIVE_MATCH bit" \
    "" "$(oc_compat_flag_bits | grep -x '25' || true)"

  # IPTOS_THROUGHPUT is the last, #ifdef-guarded upstream row and the last oc
  # match arm - both extractions must reach the end of their ranges.
  check "socket-options reaches the final row (upstream)" \
    "IPTOS_THROUGHPUT" "$(up_socket_options | grep -x 'IPTOS_THROUGHPUT' || true)"
  check "socket-options reaches the final row (oc)" \
    "IPTOS_THROUGHPUT" "$(oc_socket_options | grep -x 'IPTOS_THROUGHPUT' || true)"

  # Alias-collapse guard: without the CSUM_* dedupe the upstream table yields
  # a phantom "xxhash" member that upstream never advertises.
  check "checksum-names collapses the xxhash alias (upstream)" \
    "" "$(up_checksum_names | grep -x 'xxhash' || true)"
  check "checksum-names keeps xxh64 after the collapse (upstream)" \
    "xxh64" "$(up_checksum_names | grep -x 'xxh64' || true)"

  # Pin the full 5-codec list: a cfg-attribute leak or a lost push would both
  # change this exact answer.
  check "compression-names oc extraction is exactly the 5 codecs" \
    "lz4 none zlib zlibx zstd" "$(oc_compression_names | tr '\n' ' ' | sed 's/ $//')"

  # Case guard, and the reason --delta exists. The 3.4.4 -> 3.5.0 option delta
  # is +5, but a hand-rolled `[a-z0-9-]+` extraction reported +3 during the
  # migration: `drop-D` and `no-drop-D` carry a capital letter and were dropped
  # silently. Both the miscount and the two names are pinned here, so a future
  # extractor that loses case fails loudly instead of understating a release.
  #
  # Skipped, not failed, when either release the delta names is not fetched: a
  # self-test that cannot run must say so rather than pass vacuously. The guard
  # covers BOTH hardcoded versions - the CI job pins UPSTREAM_VERSION=3.5.0 and
  # fetches only that tree, so 3.4.4 (the delta baseline) is absent there; a
  # guard on 3.5.0 alone passed and then read a missing 3.4.4/options.c.
  if [ -d "$(upstream_src 3.4.4)" ] && [ -d "$(upstream_src 3.5.0)" ]; then
    local added
    added=$(comm -13 <(surface_at 3.4.4 cli-long-options) <(surface_at 3.5.0 cli-long-options))
    check "cli-long-options 3.4.4->3.5.0 adds exactly 5" \
      "5" "$(printf '%s\n' "$added" | wc -l | tr -d ' ')"
    check "cli-long-options delta keeps mixed-case 'drop-D'" \
      "drop-D" "$(printf '%s\n' "$added" | grep -x 'drop-D' || true)"
    check "cli-long-options delta keeps mixed-case 'no-drop-D'" \
      "no-drop-D" "$(printf '%s\n' "$added" | grep -x 'no-drop-D' || true)"
  else
    printf '  skip  cli-long-options delta: rsync 3.4.4 and 3.5.0 source not both fetched\n'
  fi

  printf 'self-test: %s failure(s)\n' "$fails"
  return $((fails > 0))
}

# --- release-to-release delta ----------------------------------------------
# "What did this release add or remove?" is the SAME extraction, run twice
# against different sources. Routing it through the existing up_* functions is
# the entire design: answering it with a fresh one-off regex is what produced a
# wrong 3.5.0 new-option count during the migration - an ad-hoc `[a-z0-9-]+`
# class silently dropped `drop-D` and `no-drop-D`, understating +5 as +3. The
# extractors here are case-agnostic by construction, so the same question asked
# through them cannot repeat that. One extractor, many questions.
surface_at() { # version surface -> members on stdout
  UPSTREAM="$(upstream_src "$1")" "$(fn_name up "$2")"
}

delta_side() { # label members
  local n=0
  [ -n "$2" ] && n=$(printf '%s\n' "$2" | wc -l | tr -d ' ')
  printf '  %-12s %s\n' "$1 ($n):" "$(printf '%s' "$2" | tr '\n' ' ')"
}

report_delta() { # old new surface
  local old=$1 new=$2 s=$3 o n
  o=$(surface_at "$old" "$s"); n=$(surface_at "$new" "$s")
  printf '\n=== %s: %s -> %s ===\n' "$s" "$old" "$new"
  delta_side added   "$(comm -13 <(printf '%s\n' "$o") <(printf '%s\n' "$n"))"
  delta_side removed "$(comm -23 <(printf '%s\n' "$o") <(printf '%s\n' "$n"))"
}

# --- main ------------------------------------------------------------------
DIVERGED=0
case "${1-}" in
  --list) printf '%s\n' "${SURFACES[@]}"; exit 0 ;;
  --self-test) self_test; exit $? ;;
  --delta)
    shift
    [ $# -ge 2 ] || die "--delta needs two versions, e.g. --delta 3.4.4 3.5.0"
    delta_old=$1 delta_new=$2; shift 2
    require_upstream "$delta_old"; require_upstream "$delta_new"
    delta_targets=("$@")
    [ ${#delta_targets[@]} -gt 0 ] || delta_targets=("${SURFACES[@]}")
    for s in "${delta_targets[@]}"; do
      printf '%s\n' "${SURFACES[@]}" | grep -qxF "$s" || die "unknown surface: $s (try --list)"
      report_delta "$delta_old" "$delta_new" "$s"
    done
    exit 0 ;;
  --check) CHECK=1; shift ;;
  *) CHECK=0 ;;
esac

targets=("$@")
[ ${#targets[@]} -gt 0 ] || targets=("${SURFACES[@]}")

for s in "${targets[@]}"; do
  printf '%s\n' "${SURFACES[@]}" | grep -qxF "$s" || die "unknown surface: $s (try --list)"
  report_surface "$s"
done

if [ "$DIVERGED" -eq 1 ]; then
  printf '\nUnaccepted divergences found. Close them, or record each in %s with a reason.\n' \
    "${ACCEPTED_FILE#"${REPO_ROOT}/"}"
  [ "$CHECK" -eq 1 ] && exit 1
fi
exit 0
