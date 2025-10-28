#!/usr/bin/env bash
set -euo pipefail

fail=0

while IFS= read -r -d '' file; do
    case "$file" in
        src/* | crates/* | xtask/src/*)
            ;;
        *)
            continue
            ;;
    esac
    header="$(head -n 1 "$file")"
    # Normalize Windows-style headers so CRLF checkouts do not fail the lint.
    header="${header%$'\r'}"
    expected_header="// $file"
    if [[ $header != "$expected_header" ]]; then
        echo "$file: incorrect header (expected '$expected_header')" >&2
        fail=1
        continue
    fi
    safety_block=0
    while IFS= read -r line; do
        if [[ $line =~ ^[[:space:]]*// ]]; then
            if [[ $line =~ ^[[:space:]]*/// ]] || [[ $line =~ ^[[:space:]]*//! ]]; then
                safety_block=0
                continue
            fi
            if [[ $line =~ ^[[:space:]]*//[[:space:]]*SAFETY: ]]; then
                safety_block=1
                continue
            fi
            if (( safety_block )); then
                continue
            fi
            echo "$file: contains // comments" >&2
            fail=1
        else
            safety_block=0
        fi
    done < <(tail -n +2 "$file")
# Include tracked and untracked Rust sources so local checks flag issues before staging.
done < <(git ls-files -z --cached --others --exclude-standard -- '*.rs')

exit $fail
