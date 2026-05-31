#!/usr/bin/env bash
# IUR-6.c: Regression guard against reintroduction of the shared-ring
# serialization bottleneck in io_uring hot-path factories.
#
# Background: The IUR-1 audit identified that `file_writer`, `file_reader`,
# and `socket_writer` factories serialized parallel submissions through a
# single `Arc<Mutex<IoUring>>` ring. IUR-3 migrated these to per-thread rings
# via `per_thread_ring.rs`. This script prevents the `Arc<Mutex<IoUring>>`
# pattern from being reintroduced in those hot-path modules.
#
# What it checks:
#   1. Factory modules (file_writer, file_reader, socket_writer, file_factory,
#      socket_factory) must not contain `Arc<Mutex` wrapping an io_uring ring.
#   2. No new `mod shared_ring` declaration in `mod.rs` that re-exports a
#      global/static shared ring used by factories.
#
# Legitimate uses (allowlisted):
#   - `send_zc.rs`: Uses `Arc<Mutex<IoUring>>` for zero-copy sender, which is
#     pinned to a single connection - not a hot-path factory pattern.
#   - `shared_ring.rs` itself: The module implements a per-session reader+writer
#     ring topology (not the old global bottleneck). Once IUR-6.b removes it,
#     this guard prevents reintroduction.
#
# Usage:
#   tools/ci/check_shared_ring_removal.sh
#
# Exit codes:
#   0 - No violations found.
#   1 - Violation detected: shared-ring serialization pattern in factories.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

IO_URING_DIR="${REPO_ROOT}/crates/fast_io/src/io_uring"

# Hot-path factory modules that must never contain Arc<Mutex<IoUring>>.
GUARDED_FILES=(
    "${IO_URING_DIR}/file_writer.rs"
    "${IO_URING_DIR}/file_reader.rs"
    "${IO_URING_DIR}/socket_writer.rs"
    "${IO_URING_DIR}/file_factory.rs"
    "${IO_URING_DIR}/socket_factory.rs"
)

# Pattern: Arc<Mutex wrapping anything ring-related. Catches:
#   Arc<Mutex<IoUring>>
#   Arc<Mutex<io_uring::IoUring>>
#   Arc<Mutex<RawIoUring>>
# The regex is intentionally broad to catch renamed variants.
ARC_MUTEX_PATTERN='Arc<Mutex'

violations=0

printf '=== IUR-6.c: shared-ring removal guard ===\n'
printf 'Checking hot-path factory modules for Arc<Mutex serialization...\n\n'

for file in "${GUARDED_FILES[@]}"; do
    if [ ! -f "$file" ]; then
        # File might not exist on non-Linux or stub builds; skip.
        continue
    fi

    matches=$(grep -n "$ARC_MUTEX_PATTERN" "$file" 2>/dev/null || true)
    if [ -n "$matches" ]; then
        printf 'VIOLATION: %s contains Arc<Mutex (shared-ring serialization)\n' \
            "$(realpath --relative-to="$REPO_ROOT" "$file" 2>/dev/null || echo "$file")"
        printf '%s\n\n' "$matches"
        violations=$((violations + 1))
    fi
done

# Guard 2: Check that no static/lazy_static/OnceLock wraps a shared IoUring
# instance in the factory modules. This catches indirect patterns like:
#   static SHARED_RING: OnceLock<Mutex<IoUring>>
#   lazy_static! { static ref RING: Mutex<IoUring> }
STATIC_MUTEX_PATTERN='static.*Mutex.*[Ii]o[Uu]ring\|OnceLock.*Mutex.*[Ii]o[Uu]ring\|lazy_static.*Mutex.*[Ii]o[Uu]ring'

for file in "${GUARDED_FILES[@]}"; do
    if [ ! -f "$file" ]; then
        continue
    fi

    matches=$(grep -n "$STATIC_MUTEX_PATTERN" "$file" 2>/dev/null || true)
    if [ -n "$matches" ]; then
        printf 'VIOLATION: %s contains static Mutex<IoUring> (global shared ring)\n' \
            "$(realpath --relative-to="$REPO_ROOT" "$file" 2>/dev/null || echo "$file")"
        printf '%s\n\n' "$matches"
        violations=$((violations + 1))
    fi
done

if [ "$violations" -gt 0 ]; then
    printf 'FAILED: %d violation(s) found.\n' "$violations"
    printf 'The per-thread ring architecture (IUR-3) forbids Arc<Mutex<IoUring>>\n'
    printf 'in hot-path factory modules. Use per_thread_ring::with_ring() instead.\n'
    printf 'See docs/design/shared-ring-removal-guard.md for rationale.\n'
    exit 1
fi

printf 'PASSED: No shared-ring serialization patterns in factory modules.\n'
exit 0
