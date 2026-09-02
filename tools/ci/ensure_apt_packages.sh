#!/usr/bin/env bash
# Guarantee the named APT packages are actually installed.
#
# WHY THIS EXISTS
#
# `awalsh128/cache-apt-pkgs-action` restores a cache and then reports which
# packages it believes are present by reading `manifest_main.log` out of that
# cache. The manifest is data the cache carries, not an observation of the
# machine, so a cache entry saved without its package payload restores as:
#
#     Cache hit for: cache-apt-pkgs_fe10c55f565538c3338c7fd7f037618c
#     Cache Size: ~0 MB (1229 B)
#     Found 3 files in the cache.
#     - cache_key.md5
#     - install.log
#     - manifest_main.log
#     Reading from main requested packages manifest...
#     - libacl1-dev=2.3.2-1build1.1
#
# Every requested package is announced as installed, nothing is installed, and
# the step exits 0. The failure surfaces much later and somewhere else - the
# upstream rsync oracle stops linking:
#
#     /usr/bin/ld: cannot find -lacl: No such file or directory
#     /usr/bin/ld: cannot find -lxxhash: No such file or directory
#
# with no oracle binary the testsuite cells that need one report `got skip`
# rather than their expected outcome, and a required context goes red for a
# reason that has nothing to do with the commit under test.
#
# Deleting a poisoned entry does not fix it: the entry is re-saved by the next
# run that takes the same path. Measured 2026-09-02, key fe10c55f... was
# deleted and re-created ~10 minutes later on two refs at 1214 B and 1220 B.
#
# THE CONTRACT
#
# The cache is an OPTIMISATION. It is never the thing that makes the packages
# present. This script asks dpkg what is actually installed - the machine, not
# the manifest - and installs whatever is missing. A cache hit that delivered
# its payload makes this a no-op; a poisoned one costs an install and emits a
# warning so the poisoning is visible instead of silent.
set -euo pipefail

if [ "$#" -eq 0 ]; then
    echo "usage: $0 <package>..." >&2
    exit 2
fi

# dpkg-query exits non-zero for an unknown package, which is one of the two
# ways a package can be missing; `-e` must not abort on it.
is_installed() {
    local status
    status=$(dpkg-query -W -f='${Status}' "$1" 2>/dev/null) || return 1
    [ "$status" = "install ok installed" ]
}

# Accept both `$APT_PACKAGES` and `"$APT_PACKAGES"`. Callers pass a single
# space-separated variable, and whether it word-splits depends on a pair of
# quotes someone will eventually "correct" in either direction. Splitting here
# makes both spellings mean the same thing instead of making one of them fail
# with dpkg complaining about a package whose name is the whole list.
requested=()
for arg in "$@"; do
    for pkg in $arg; do
        requested+=("$pkg")
    done
done

missing=()
for pkg in "${requested[@]}"; do
    if ! is_installed "$pkg"; then
        missing+=("$pkg")
    fi
done

if [ "${#missing[@]}" -eq 0 ]; then
    echo "All ${#requested[@]} requested APT packages are installed."
    exit 0
fi

# A GitHub warning annotation, not a silent repair: a cache that reported a hit
# and delivered nothing is a defect worth seeing in the run summary.
printf '::warning::APT cache restored without payload; installing %d missing package(s): %s\n' \
    "${#missing[@]}" "${missing[*]}"

sudo apt-get update
sudo apt-get install -y --no-install-recommends "${missing[@]}"

# Re-ask dpkg. `apt-get install` can exit 0 having skipped a package it could
# not resolve, so trusting its status here would rebuild the same hole this
# script exists to close.
still_missing=()
for pkg in "${missing[@]}"; do
    if ! is_installed "$pkg"; then
        still_missing+=("$pkg")
    fi
done

if [ "${#still_missing[@]}" -ne 0 ]; then
    printf '::error::APT packages still missing after install: %s\n' "${still_missing[*]}"
    exit 1
fi

echo "Installed ${#missing[@]} package(s) the cache did not provide."
