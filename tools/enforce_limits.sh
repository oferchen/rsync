#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/.." && pwd)"

max_lines="${MAX_RUST_LINES:-600}"
warn_lines="${WARN_RUST_LINES:-400}"

if ! [[ "$max_lines" =~ ^[0-9]+$ && "$warn_lines" =~ ^[0-9]+$ ]]; then
    echo "MAX_RUST_LINES and WARN_RUST_LINES must be positive integers" >&2
    exit 2
fi

if (( warn_lines > max_lines )); then
    echo "WARN_RUST_LINES (${warn_lines}) cannot exceed MAX_RUST_LINES (${max_lines})." >&2
    exit 2
fi

mapfile -d '' rust_files < <(find "${repo_root}" -type f -name '*.rs' -not -path '*/target/*' -not -path '*/.git/*' -print0)

if (( ${#rust_files[@]} == 0 )); then
    echo "No Rust sources found." >&2
    exit 0
fi

failure=0
warned=0

for file in "${rust_files[@]}"; do
    # wc prepends spaces; use read to trim.
    read -r line_count _ < <(wc -l -- "${file}")

    if (( line_count > max_lines )); then
        printf '::error file=%s::Rust source has %d lines (limit %d)\n' "${file}" "${line_count}" "${max_lines}" >&2
        failure=1
        continue
    fi

    if (( line_count > warn_lines )); then
        printf '::warning file=%s::Rust source has %d lines (target %d)\n' "${file}" "${line_count}" "${warn_lines}" >&2
        warned=1
    fi
done

if (( failure )); then
    exit 1
fi

if (( warned )); then
    echo "Rust source files exceed target length but remain under the enforced limit." >&2
fi

exit 0
