#!/usr/bin/env bash
# CIM-LOCKFILE-5: CI gate against `--locked` removal regressions.
#
# Scans every workflow under `.github/workflows/` and every shell helper
# under `tools/ci/` for cargo invocations of the gated subcommands listed
# below, and asserts that each such invocation also passes `--locked`.
# Fails the PR with a clear error message if any unexpected call slips
# through without `--locked`.
#
# Background
# ----------
# `Cargo.lock` is committed and every CI build is meant to honour it via
# `--locked` so that:
#   - builds are byte-for-byte reproducible at a given commit,
#   - transitive-dep drift surfaces in its own PR rather than silently
#     riding along on an unrelated change,
#   - the weekly cron PR (`cargo-lockfile-weekly.yml`) is the single
#     intentional path for dep bumps.
#
# CIM-LOCKFILE-1..4 audited and fixed every cargo invocation that should
# carry `--locked`. CIM-LOCKFILE-5 (this script) prevents regressions:
# a future PR that adds `cargo build` or `cargo nextest run` without
# `--locked` to a workflow or helper script gets caught here.
#
# What it gates
# -------------
# Gated subcommands (must carry `--locked` on the same logical command):
#   - cargo build
#   - cargo check
#   - cargo clippy
#   - cargo nextest run
#   - cargo run
#   - cargo test
#
# All other cargo subcommands are allowed without `--locked` because
# they either do not consume the workspace lockfile (`fmt`, `bench`),
# are the lockfile-mutation entry point itself (`update`), wrap an
# already-built artifact (`xtask`, `deb`, `generate-rpm`), or are a
# third-party plugin with its own semantics (`llvm-cov`, `fuzz`, `cov`,
# `install`, `deny`, `doc`, `tree`).
#
# Per-line allowlist
# ------------------
# A small allowlist of `file:line` entries covers genuinely intentional
# sites that invoke a gated subcommand without `--locked`. New entries
# require explicit reviewer signoff - see CONTRIBUTING.md, section
# "Cargo.lock maintenance".
#
# Usage
# -----
#   tools/ci/check_locked_flags.sh
#
# Exit codes
#   0 - all gated cargo invocations carry `--locked` (or are allowlisted).
#   1 - one or more violations found.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

# Subcommands that must carry --locked on the same logical command.
# Order matters only for diagnostic output; the matcher checks all of
# them per line.
GATED_SUBCOMMANDS=(
    "build"
    "check"
    "clippy"
    "nextest"
    "run"
    "test"
)

# Per-line allowlist of intentional sites that legitimately invoke a
# gated subcommand without --locked. Entries are `path:line` keyed off
# the repo-relative path. Add new entries only with reviewer signoff
# and document the rationale here.
#
# Currently empty: the lockfile-sync workflows
# (`cargo-lockfile-weekly.yml`, `cargo-lockfile-sync.yml`) only invoke
# `cargo update`, which is not a gated subcommand, so they need no
# explicit allowlist entry.
ALLOWLIST=(
)

# Scan roots.
SCAN_ROOTS=(
    "${REPO_ROOT}/.github/workflows"
    "${REPO_ROOT}/tools/ci"
)

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# Return 0 if the given `path:line` entry is in ALLOWLIST.
is_allowlisted() {
    local entry="$1"
    local item
    for item in "${ALLOWLIST[@]+"${ALLOWLIST[@]}"}"; do
        if [[ "$entry" == "$item" ]]; then
            return 0
        fi
    done
    return 1
}

# Return 0 if the file should be scanned (.yml, .yaml, .sh).
is_scannable() {
    local f="$1"
    case "$f" in
        *.yml|*.yaml|*.sh) return 0 ;;
        *) return 1 ;;
    esac
}

# Read a file and emit "<line-number>\t<joined-logical-line>" for each
# logical command, joining backslash-continued lines into one. Strips
# trailing `# ...` comments (YAML and shell share the same syntax) and
# masks out simple single-line `"..."` and `'...'` quoted spans so that
# `cargo build` appearing inside an error-message string does not trip
# the gate. Lines that do not contain `cargo` after these passes are
# skipped.
emit_joined_lines() {
    local file="$1"
    awk '
        function strip_comment(s,    i, c, in_sq, in_dq, prev) {
            in_sq = 0; in_dq = 0; prev = " "
            for (i = 1; i <= length(s); i++) {
                c = substr(s, i, 1)
                if (in_sq) {
                    if (c == "\x27") in_sq = 0
                } else if (in_dq) {
                    if (c == "\"" && prev != "\\") in_dq = 0
                } else {
                    if (c == "\x27") in_sq = 1
                    else if (c == "\"") in_dq = 1
                    else if (c == "#" && (prev == " " || prev == "\t" || i == 1)) {
                        return substr(s, 1, i - 1)
                    }
                }
                prev = c
            }
            return s
        }
        function mask_quoted(s,    i, c, in_sq, in_dq, out, prev) {
            in_sq = 0; in_dq = 0; out = ""; prev = " "
            for (i = 1; i <= length(s); i++) {
                c = substr(s, i, 1)
                if (in_sq) {
                    if (c == "\x27") { in_sq = 0; out = out c }
                    else out = out " "
                } else if (in_dq) {
                    if (c == "\"" && prev != "\\") { in_dq = 0; out = out c }
                    else out = out " "
                } else {
                    if (c == "\x27") { in_sq = 1; out = out c }
                    else if (c == "\"") { in_dq = 1; out = out c }
                    else out = out c
                }
                prev = c
            }
            return out
        }
        {
            if (buf == "") start_lineno = NR
            line = strip_comment($0)
            line = mask_quoted(line)
            sub(/[[:space:]]+$/, "", line)
            if (line ~ /\\$/) {
                sub(/\\$/, "", line)
                buf = buf line " "
                next
            }
            buf = buf line
            if (buf ~ /cargo[[:space:]]/) {
                print start_lineno "\t" buf
            }
            buf = ""
        }
        END {
            if (buf != "" && buf ~ /cargo[[:space:]]/) {
                print start_lineno "\t" buf
            }
        }
    ' "$file"
}

# Given a joined logical line, return 0 if it invokes one of the gated
# subcommands as a cargo subcommand (e.g., `cargo build`, `cargo +nightly
# nextest run`). Echoes the matched subcommand on stdout on success.
matches_gated_subcommand() {
    local line="$1"
    local sub
    # The regex below matches `cargo` optionally followed by a `+toolchain`
    # token, then one of the gated subcommands as a whole word. Anchored
    # by word boundary on both sides so `cargo build-deps` (hypothetical
    # plugin) does not trigger.
    for sub in "${GATED_SUBCOMMANDS[@]}"; do
        if [[ "$line" =~ (^|[^[:alnum:]_])cargo([[:space:]]+\+[A-Za-z0-9._-]+)?[[:space:]]+${sub}([[:space:]]|$) ]]; then
            printf '%s' "$sub"
            return 0
        fi
    done
    return 1
}

# Return 0 if the joined line carries `--locked` as a whole token.
has_locked_flag() {
    local line="$1"
    [[ "$line" =~ (^|[[:space:]])--locked([[:space:]]|$) ]]
}

# ---------------------------------------------------------------------------
# Main scan
# ---------------------------------------------------------------------------

violations=0
scanned_files=0
gated_invocations=0

printf '=== CIM-LOCKFILE-5: --locked flag regression guard ===\n'
printf 'Scanning workflows and tools/ci helpers for cargo invocations\n'
printf 'of: %s\n\n' "${GATED_SUBCOMMANDS[*]}"

# Enumerate candidate files deterministically. `find -print0` plus a
# null-delimited read avoids whitespace-in-path surprises.
files=()
while IFS= read -r -d '' f; do
    if is_scannable "$f"; then
        files+=("$f")
    fi
done < <(find "${SCAN_ROOTS[@]}" -type f \( -name '*.yml' -o -name '*.yaml' -o -name '*.sh' \) -print0 | sort -z)

for file in "${files[@]}"; do
    scanned_files=$((scanned_files + 1))
    rel="${file#${REPO_ROOT}/}"

    # emit_joined_lines yields "<lineno>\t<joined>" rows; iterate them.
    while IFS=$'\t' read -r lineno joined; do
        [[ -z "$joined" ]] && continue
        sub="$(matches_gated_subcommand "$joined" || true)"
        if [[ -z "$sub" ]]; then
            continue
        fi
        # `cargo nextest` requires the literal subcommand `run` to follow.
        # `cargo nextest list` or future subcommands are allowed without
        # --locked because they do not link tests. Enforce `run` here.
        if [[ "$sub" == "nextest" ]] && ! [[ "$joined" =~ (^|[^[:alnum:]_])nextest[[:space:]]+run([[:space:]]|$) ]]; then
            continue
        fi
        gated_invocations=$((gated_invocations + 1))
        entry="${rel}:${lineno}"
        if has_locked_flag "$joined"; then
            continue
        fi
        if is_allowlisted "$entry"; then
            continue
        fi
        violations=$((violations + 1))
        printf 'VIOLATION: %s\n' "$entry"
        printf '  subcommand: cargo %s\n' "$sub"
        printf '  command:    %s\n\n' "$joined"
    done < <(emit_joined_lines "$file")
done

printf 'Scanned %d file(s); inspected %d gated cargo invocation(s).\n' \
    "$scanned_files" "$gated_invocations"

if [[ "$violations" -gt 0 ]]; then
    printf '\nFAILED: %d gated cargo invocation(s) missing --locked.\n' "$violations"
    printf '\n'
    printf 'Every `cargo build|check|clippy|nextest run|run|test` in CI\n'
    printf 'must pass `--locked` so Cargo.lock is honoured byte-for-byte.\n'
    printf 'If a new site is intentionally lock-free, add `path:line` to\n'
    printf 'ALLOWLIST in tools/ci/check_locked_flags.sh and call it out in\n'
    printf 'the PR description. See CONTRIBUTING.md > Cargo.lock maintenance.\n'
    exit 1
fi

printf 'PASSED: all gated cargo invocations carry --locked.\n'
exit 0
