#!/usr/bin/env bash
# BPF-5: EnvGuard CI lint. Detects cap-touching tests without EnvGuard.
# DELETE ME WHEN BPF-9 LANDS (per BPF-4 spec section 9 EOL plan).
#
# This lint exists because BufferPool capacity tests share a global
# OnceLock singleton and must serialise env mutations via EnvGuard
# (from platform::env::EnvGuard). BPF-9 replaces the singleton with a
# per-test factory; once it merges, this script, its CI step, the
# ignore file, the CONTRIBUTING entry, and the BPF-4 design document
# are all deleted.
#
# Tracking: https://github.com/oferchen/oc-rsync/issues/2828
#
# Mode of operation:
#   - Default: warn-only. Logs violations to stdout, exits 0.
#   - Strict:  set OC_RSYNC_ENVGUARD_LINT_STRICT=1 to exit 1 on any
#              violation. Flip the workflow step to strict mode once
#              BPF-3 has closed the existing gap.
#
# Usage:
#   tools/ci/check_envguard.sh [--list-tracked] [--ignore-file <path>]

set -euo pipefail

# ---------------------------------------------------------------------------
# Canonical cap-token list (BPF-4 spec section 2.1).
# Order matters: longer/more-specific tokens first so the first-match
# reported in a violation line is the most informative one.
# ---------------------------------------------------------------------------
CAP_TOKENS=(
    "OC_RSYNC_BUFFER_POOL_SIZE"
    "OC_RSYNC_BUFFER_POOL_STATS"
    "OC_RSYNC_BUFFER_POOL_"
)

# Guard token (BPF-4 spec section 2.2). The bare type name matches the
# canonical guard at platform::env::EnvGuard as well as the inline
# duplicates that BPF-8 will collapse into a single re-export.
GUARD_TOKEN="EnvGuard"

# ---------------------------------------------------------------------------
# Repo root resolution. Script lives at tools/ci/<this>.
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

# ---------------------------------------------------------------------------
# CLI parsing.
# ---------------------------------------------------------------------------
IGNORE_FILE="${REPO_ROOT}/tools/ci/envguard_lint.ignore"
LIST_TRACKED=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --list-tracked)
            LIST_TRACKED=1
            shift
            ;;
        --ignore-file)
            if [[ $# -lt 2 ]]; then
                echo "error: --ignore-file requires a path argument" >&2
                exit 2
            fi
            IGNORE_FILE="$2"
            shift 2
            ;;
        -h|--help)
            sed -n '1,30p' "$0"
            exit 0
            ;;
        *)
            echo "error: unknown flag: $1" >&2
            exit 2
            ;;
    esac
done

if [[ "${LIST_TRACKED}" -eq 1 ]]; then
    for token in "${CAP_TOKENS[@]}"; do
        printf '%s\n' "${token}"
    done
    exit 0
fi

# ---------------------------------------------------------------------------
# Tool availability.
# ---------------------------------------------------------------------------
if ! command -v rg >/dev/null 2>&1; then
    echo "error: ripgrep (rg) is required but not installed" >&2
    exit 2
fi
if ! command -v awk >/dev/null 2>&1; then
    echo "error: awk is required but not installed" >&2
    exit 2
fi

# ---------------------------------------------------------------------------
# Load ignore file. Format: one "<repo-rel-path>::<test_fn_name>" per
# line; blank lines and `#` comments allowed. A bare path with no
# `::<fn_name>` ignores the entire file.
# ---------------------------------------------------------------------------
IGNORE_ENTRIES=()
if [[ -f "${IGNORE_FILE}" ]]; then
    while IFS= read -r line || [[ -n "${line}" ]]; do
        # Strip trailing CR (Windows line endings) and leading/trailing whitespace.
        line="${line%$'\r'}"
        line="${line#"${line%%[![:space:]]*}"}"
        line="${line%"${line##*[![:space:]]}"}"
        if [[ -z "${line}" ]] || [[ "${line}" == \#* ]]; then
            continue
        fi
        IGNORE_ENTRIES+=("${line}")
    done < "${IGNORE_FILE}"
elif [[ "${IGNORE_FILE}" != "/dev/null" ]]; then
    echo "error: ignore file not found: ${IGNORE_FILE}" >&2
    exit 2
fi

is_ignored() {
    local rel_path="$1"
    local fn_name="$2"
    local entry
    for entry in "${IGNORE_ENTRIES[@]+"${IGNORE_ENTRIES[@]}"}"; do
        if [[ "${entry}" == "${rel_path}::${fn_name}" ]]; then
            return 0
        fi
        if [[ "${entry}" == "${rel_path}" ]]; then
            return 0
        fi
    done
    return 1
}

# ---------------------------------------------------------------------------
# Candidate file enumeration (BPF-4 spec section 5).
#   - crates/*/tests/**/*.rs
#   - crates/*/src/**/*.rs containing `#[cfg(test)]` or `#[test]`
# Exclusions: target/, .claude/worktrees/, tools/, xtask/, fuzz/.
# ---------------------------------------------------------------------------
cd "${REPO_ROOT}"

CANDIDATES=()
while IFS= read -r f; do
    [[ -z "${f}" ]] && continue
    CANDIDATES+=("${f}")
done < <(
    rg --files \
        --glob 'crates/*/tests/**/*.rs' \
        --glob 'crates/*/src/**/*.rs' \
        --glob '!target/**' \
        --glob '!.claude/worktrees/**' \
        --glob '!tools/**' \
        --glob '!xtask/**' \
        --glob '!fuzz/**' \
        2>/dev/null | sort -u
)

# For src/ files, restrict to those containing #[cfg(test)] or #[test]
# so production-only modules are skipped.
FILTERED_CANDIDATES=()
for f in "${CANDIDATES[@]+"${CANDIDATES[@]}"}"; do
    if [[ "${f}" == crates/*/tests/* ]]; then
        FILTERED_CANDIDATES+=("${f}")
        continue
    fi
    if rg --quiet -e '#\[cfg\(test\)\]' -e '#\[test\]' -e '#\[tokio::test\]' "${f}" 2>/dev/null; then
        FILTERED_CANDIDATES+=("${f}")
    fi
done

# ---------------------------------------------------------------------------
# Detection algorithm (BPF-4 spec section 6).
# A small awk program does the brace-balanced extraction: starting from
# any `#[test]` or `#[tokio::test]` attribute, find the following `fn
# <name>(...) {` and scan forward counting `{`/`}` until the depth is
# back to zero. Emit "<start_line>\t<fn_name>\t<body>" for each test.
# ---------------------------------------------------------------------------
extract_tests_awk='
BEGIN {
    in_test = 0
    pending_attr = 0
    depth = 0
    body = ""
    fn_name = ""
    start_line = 0
}
function flush() {
    if (fn_name != "") {
        # Emit start_line, fn_name, body (newlines in body replaced with \x01).
        gsub(/\x01/, " ", body)
        # Replace literal newlines with \x01 so the consumer can split records.
        printf "%d\t%s\t", start_line, fn_name
        n = split(body, lines, "\n")
        for (i = 1; i <= n; i++) {
            if (i > 1) printf "\x01"
            printf "%s", lines[i]
        }
        printf "\n"
    }
    in_test = 0
    pending_attr = 0
    depth = 0
    body = ""
    fn_name = ""
    start_line = 0
}
{
    line = $0
    if (!in_test) {
        # Look for a #[test] or #[tokio::test] attribute line.
        if (line ~ /#\[test\]/ || line ~ /#\[tokio::test\]/) {
            pending_attr = 1
            start_line = NR
            body = line
            next
        }
        if (pending_attr) {
            body = body "\n" line
            # Tolerate additional attributes (e.g. #[ignore], #[serial]).
            if (line ~ /^[[:space:]]*#\[/) {
                next
            }
            # Look for fn <name>. Use portable match() + substr() because
            # the 3-argument match() with a capture array is gawk-only.
            if (match(line, /fn[[:space:]]+[A-Za-z_][A-Za-z0-9_]*/)) {
                token = substr(line, RSTART, RLENGTH)
                sub(/^fn[[:space:]]+/, "", token)
                fn_name = token
                # Count braces on this line.
                opens = gsub(/\{/, "{", line)
                closes = gsub(/\}/, "}", line)
                depth += opens - closes
                if (depth > 0) {
                    in_test = 1
                } else if (opens > 0 && depth == 0) {
                    # Single-line body: balanced on the same line.
                    flush()
                }
                next
            }
            # Attribute did not immediately precede an fn; reset.
            pending_attr = 0
            body = ""
            start_line = 0
            next
        }
        next
    }
    # Inside a test body: accumulate and track braces.
    body = body "\n" line
    opens = gsub(/\{/, "{", line)
    closes = gsub(/\}/, "}", line)
    depth += opens - closes
    if (depth <= 0) {
        flush()
    }
}
END {
    if (in_test) {
        flush()
    }
}
'

# Build extended-regex for cap-tokens (OR of literals).
# `awk -v` cannot pass an array, so join into a single regex.
CAP_REGEX=""
for token in "${CAP_TOKENS[@]}"; do
    if [[ -z "${CAP_REGEX}" ]]; then
        CAP_REGEX="${token}"
    else
        CAP_REGEX="${CAP_REGEX}|${token}"
    fi
done

violations=0
violation_lines=()

for f in "${FILTERED_CANDIDATES[@]+"${FILTERED_CANDIDATES[@]}"}"; do
    # Quick reject: skip files that mention no cap-tokens at all.
    if ! rg --quiet --no-messages -e "${CAP_REGEX}" "${f}"; then
        continue
    fi
    # Extract each (start_line, fn_name, body) record from the file.
    while IFS=$'\t' read -r start_line fn_name body; do
        [[ -z "${fn_name}" ]] && continue
        if is_ignored "${f}" "${fn_name}"; then
            continue
        fi
        # Restore newlines.
        body_decoded="${body//$'\x01'/$'\n'}"
        # Find the first cap-token present in this body.
        first_match=""
        for token in "${CAP_TOKENS[@]}"; do
            if [[ "${body_decoded}" == *"${token}"* ]]; then
                first_match="${token}"
                break
            fi
        done
        if [[ -z "${first_match}" ]]; then
            continue
        fi
        # Skip if the guard token is present.
        if [[ "${body_decoded}" == *"${GUARD_TOKEN}"* ]]; then
            continue
        fi
        violations=$((violations + 1))
        violation_lines+=("VIOLATION: ${f}:${start_line}: function ${fn_name} touches ${first_match} without EnvGuard")
    done < <(awk "${extract_tests_awk}" "${f}")
done

# ---------------------------------------------------------------------------
# Reporting & exit.
# ---------------------------------------------------------------------------
strict="${OC_RSYNC_ENVGUARD_LINT_STRICT:-0}"

if [[ "${violations}" -eq 0 ]]; then
    echo "EnvGuard lint: 0 violations across ${#FILTERED_CANDIDATES[@]} candidate file(s)."
    exit 0
fi

# Emit violation lines. In strict mode they go to stderr; in warn-only
# mode to stdout so the workflow log shows them but the step still
# succeeds.
if [[ "${strict}" == "1" ]]; then
    for line in "${violation_lines[@]}"; do
        echo "${line}" >&2
    done
    echo "EnvGuard lint: ${violations} violation(s) - see https://github.com/oferchen/oc-rsync/issues/2822" >&2
    exit 1
else
    for line in "${violation_lines[@]}"; do
        echo "${line}"
    done
    echo "EnvGuard lint: ${violations} violation(s) reported in warn-only mode."
    echo "EnvGuard lint: set OC_RSYNC_ENVGUARD_LINT_STRICT=1 to fail the build on violations."
    echo "EnvGuard lint: tracking issue https://github.com/oferchen/oc-rsync/issues/2822"
    exit 0
fi
