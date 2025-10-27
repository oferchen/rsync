#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/.." && pwd)"

cd "${repo_root}"

# Use ripgrep to scan Rust sources while ignoring generated artifacts.
if ! command -v rg >/dev/null 2>&1; then
    echo "ripgrep (rg) is required but was not found in PATH." >&2
    exit 2
fi

patterns=(
    'todo!\s*\('
    'unimplemented!\s*\('
    '(?i)\bFIXME\b'
    '(?i)\bXXX\b'
    'panic!\s*\(\s*"(?i:(todo|fixme|placeholder|unimplemented|not implemented))'
)

rg_args=(
    --with-filename
    --line-number
    --color=never
    --no-heading
    --hidden
    -g '*.rs'
    --glob '!target/**'
    --glob '!.git/**'
)

tmp_matches_file="$(mktemp -t rsync_no_placeholders_matches.XXXXXX)"
trap 'rm -f "${tmp_matches_file}"' EXIT

violations=0

for pattern in "${patterns[@]}"; do
    if rg "${rg_args[@]}" --pcre2 "${pattern}" >"${tmp_matches_file}" 2>/dev/null; then
        if (( violations == 0 )); then
            echo "Prohibited placeholders found:" >&2
        fi
        cat "${tmp_matches_file}" >&2
        : > "${tmp_matches_file}"
        violations=1
    fi
done

if (( violations )); then
    exit 1
fi

exit 0
