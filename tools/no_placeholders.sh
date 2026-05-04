#!/usr/bin/env bash
set -euo pipefail

fail=0
# Match common placeholder macros and comment tags. The panic! guard needs to
# handle escaped quotes inside the literal, so it uses a negated character
# class that also permits escaped characters via `\.`.
# GNU/BSD `grep -E` does not recognize `\b` as a word boundary, so approximate one
# using start/end checks against non-identifier characters. This keeps `todo`,
# `unimplemented`, `fixme`, and `xxx` detections case-insensitive without
# tripping on identifiers such as `prefixme`. The panic! guard retains its
# escaped-quote handling.
pattern='todo!|unimplemented!|(^|[^[:alnum:]_])(todo|unimplemented|fixme|xxx)([^[:alnum:]_]|$)|panic!\("([^"\\]|\\.)*(todo|fixme|xxx|unimplemented)([^"\\]|\\.)*"\)'

while IFS= read -r -d '' file; do
    offenses=$(grep -nEi "$pattern" "$file" | grep -v '^1:' || true)
    if [[ -n $offenses ]]; then
        while IFS= read -r offense; do
            printf '%s:%s\n' "$file" "$offense" >&2
        done <<<"$offenses"
        fail=1
    fi
# Include tracked and untracked Rust sources so local checks flag issues before staging.
done < <(git ls-files -z --cached --others --exclude-standard -- '*.rs')

exit $fail
