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

# --- registry --------------------------------------------------------------
SURFACES=(daemon-directives cli-long-options cli-short-options syslog-facilities)

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

  # Case guard, and the reason --delta exists. The 3.4.4 -> 3.5.0 option delta
  # is +5, but a hand-rolled `[a-z0-9-]+` extraction reported +3 during the
  # migration: `drop-D` and `no-drop-D` carry a capital letter and were dropped
  # silently. Both the miscount and the two names are pinned here, so a future
  # extractor that loses case fails loudly instead of understating a release.
  #
  # Skipped, not failed, when the newer oracle is not fetched: a self-test that
  # cannot run must say so rather than pass vacuously.
  if [ -d "$(upstream_src 3.5.0)" ]; then
    local added
    added=$(comm -13 <(surface_at 3.4.4 cli-long-options) <(surface_at 3.5.0 cli-long-options))
    check "cli-long-options 3.4.4->3.5.0 adds exactly 5" \
      "5" "$(printf '%s\n' "$added" | wc -l | tr -d ' ')"
    check "cli-long-options delta keeps mixed-case 'drop-D'" \
      "drop-D" "$(printf '%s\n' "$added" | grep -x 'drop-D' || true)"
    check "cli-long-options delta keeps mixed-case 'no-drop-D'" \
      "no-drop-D" "$(printf '%s\n' "$added" | grep -x 'no-drop-D' || true)"
  else
    printf '  skip  cli-long-options delta: rsync 3.5.0 source not fetched\n'
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
