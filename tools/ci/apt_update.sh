#!/usr/bin/env bash
# Refresh the APT indexes without letting an unrelated third-party repository
# fail the job.
#
# WHY THIS EXISTS
#
# The GitHub runner image ships source lists for repositories this project has
# no interest in - Google Chrome, Microsoft, and whatever a future image adds.
# When one of them republishes its Release between the Release fetch and the
# index fetch, `apt-get update` reports:
#
#     Err:24 https://dl.google.com/linux/chrome-stable/deb stable/main amd64 Packages
#       Hash Sum mismatch
#     ...
#     E: Failed to fetch https://dl.google.com/.../Packages.gz  Hash Sum mismatch
#     E: Some index files failed to download. They have been ignored, or old ones used instead.
#
# and exits 100. Measured on run 34376473178 (2026-09-09), where it failed
# `Linux musl (stable)` and `Linux musl (nightly)` at the "Install musl
# toolchain" step - before a single line of this project was compiled - on a
# pull request whose entire diff was doc comments.
#
# THE CONTRACT
#
# `apt-get update` is a best-effort index refresh; `apt-get install` is the
# gate. That split is apt's own: the summary line above says the failed indexes
# were IGNORED and the cached ones used, and the Ubuntu archive indexes this
# project actually needs fetched successfully in the same run. If the package a
# caller wants is genuinely unresolvable, its `apt-get install` says so by name
# and fails there, which is a better diagnostic than exit 100 from a repository
# nobody asked for.
#
# So: exit 0 when every error apt reported is a per-URL index fetch covered by
# its own "they have been ignored" summary, and emit a warning naming the URLs
# so the degradation is visible rather than silent. Exit non-zero - unchanged -
# for anything else, including a lock conflict, a broken sources.list, or an
# archive that is wholly unreachable.
#
# This is not a retry. A retry would paper over a genuine outage and cost a
# minute doing it; this reclassifies one specific, self-declared-recoverable
# failure and leaves every other one fatal.
set -uo pipefail

readonly IGNORED_SUMMARY='Some index files failed to download. They have been ignored, or old ones used instead.'

output=$(sudo apt-get update 2>&1) && status=0 || status=$?
printf '%s\n' "$output"

if [ "$status" -eq 0 ]; then
    exit 0
fi

# apt did not declare the failure recoverable, so it is not.
if ! grep -qF "E: $IGNORED_SUMMARY" <<<"$output"; then
    printf '::error::apt-get update failed (exit %s); apt did not report the failure as ignorable\n' "$status"
    exit "$status"
fi

# The summary line covers index downloads only. Any other error - a lock, a
# malformed source, a missing key - rides in on the same exit code and must
# still be fatal, so it is matched out explicitly rather than assumed absent.
unexplained=$(grep '^E: ' <<<"$output" | grep -vF "E: $IGNORED_SUMMARY" | grep -v '^E: Failed to fetch ')
if [ -n "$unexplained" ]; then
    printf '::error::apt-get update reported errors beyond the ignored index downloads:\n%s\n' "$unexplained"
    exit "$status"
fi

urls=$(grep '^E: Failed to fetch ' <<<"$output" | awk '{ print $4 }' | tr '\n' ' ')
count=$(grep -c '^E: Failed to fetch ' <<<"$output")
printf '::warning::apt-get update ignored %s unreachable index file(s); the install step remains the gate: %s\n' \
    "$count" "$urls"
exit 0
