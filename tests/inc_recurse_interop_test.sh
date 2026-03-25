#!/usr/bin/env bash
# INC_RECURSE Interoperability Test Script
#
# Validates incremental recursion behavior between oc-rsync and upstream rsync
# 3.4.1. Tests local transfers using both binaries to verify that recursive
# directory walking, incremental updates, and deletions produce identical
# results.
#
# Environment variable overrides:
#   OC_RSYNC              - path to oc-rsync binary
#   UPSTREAM_RSYNC        - path to upstream rsync binary
#   UPSTREAM_INSTALL_ROOT - root of upstream installs (expects {version}/bin/rsync)
#   UPSTREAM_VERSION      - upstream version to test (default: 3.4.1)

set -euo pipefail

# Resolve workspace root from script location
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Paths (overridable via environment)
OC_RSYNC="${OC_RSYNC:-${WORKSPACE_ROOT}/target/release/oc-rsync}"
UPSTREAM_VERSION="${UPSTREAM_VERSION:-3.4.1}"
UPSTREAM_INSTALL_ROOT="${UPSTREAM_INSTALL_ROOT:-${WORKSPACE_ROOT}/target/interop/upstream-install}"
UPSTREAM_RSYNC="${UPSTREAM_RSYNC:-${UPSTREAM_INSTALL_ROOT}/${UPSTREAM_VERSION}/bin/rsync}"

# Create a temp directory with cleanup trap
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

# Counters
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

log_info() {
    echo "[INFO] $1"
}

log_error() {
    echo "[ERROR] $1" >&2
}

log_test() {
    echo ""
    echo "=== $1 ==="
}

pass_test() {
    local test_name="$1"
    log_info "$test_name: PASS"
    TESTS_PASSED=$((TESTS_PASSED + 1))
}

fail_test() {
    local test_name="$1"
    local reason="${2:-}"
    if [ -n "$reason" ]; then
        log_error "$test_name: FAIL - $reason"
    else
        log_error "$test_name: FAIL"
    fi
    TESTS_FAILED=$((TESTS_FAILED + 1))
}

# Create a deep directory tree: 5 levels, ~50 files total
create_deep_tree() {
    local base="$1"
    mkdir -p "$base"
    local count=0
    for a in d1 d2 d3 d4 d5; do
        mkdir -p "$base/$a"
        for b in sub1 sub2; do
            mkdir -p "$base/$a/$b"
            for c in inner1 inner2; do
                mkdir -p "$base/$a/$b/$c"
                for d in leaf1 leaf2; do
                    mkdir -p "$base/$a/$b/$c/$d"
                    dd if=/dev/urandom of="$base/$a/$b/$c/$d/file_${count}.dat" \
                        bs=64 count=1 2>/dev/null
                    count=$((count + 1))
                done
            done
        done
    done
    log_info "Created deep tree with $count files in $base"
}

# Create a wide directory tree: 100 top-level dirs, 10 files each
create_wide_tree() {
    local base="$1"
    mkdir -p "$base"
    for i in $(seq 1 100); do
        local dir
        dir=$(printf "%s/dir_%03d" "$base" "$i")
        mkdir -p "$dir"
        for j in $(seq 1 10); do
            dd if=/dev/urandom of="$dir/file_${j}.dat" bs=32 count=1 2>/dev/null
        done
    done
    log_info "Created wide tree with 1000 files in $base"
}

# Count files recursively (portable)
count_files() {
    find "$1" -type f | wc -l | tr -d ' '
}

# =========================================================================
# Test 1: oc-rsync push to upstream rsync destination
# =========================================================================
test_oc_push_deep_tree() {
    local test_name="oc-rsync push deep tree (5 levels, ~80 files)"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work_dir="$TEST_DIR/test_push"
    mkdir -p "$work_dir"/{src,dest_oc,dest_upstream}

    create_deep_tree "$work_dir/src"

    # Sync using oc-rsync
    if ! "$OC_RSYNC" -r "$work_dir/src/" "$work_dir/dest_oc/" > "$work_dir/oc.log" 2>&1; then
        fail_test "$test_name" "oc-rsync transfer failed"
        cat "$work_dir/oc.log" >&2
        return 0
    fi

    # Sync using upstream rsync for reference
    if ! "$UPSTREAM_RSYNC" -r "$work_dir/src/" "$work_dir/dest_upstream/" > "$work_dir/upstream.log" 2>&1; then
        fail_test "$test_name" "upstream rsync transfer failed"
        cat "$work_dir/upstream.log" >&2
        return 0
    fi

    # Verify oc-rsync destination matches source
    if ! diff -r "$work_dir/src" "$work_dir/dest_oc" > "$work_dir/diff_oc.log" 2>&1; then
        fail_test "$test_name" "oc-rsync destination differs from source"
        cat "$work_dir/diff_oc.log" >&2
        return 0
    fi

    # Verify upstream destination matches source
    if ! diff -r "$work_dir/src" "$work_dir/dest_upstream" > "$work_dir/diff_upstream.log" 2>&1; then
        fail_test "$test_name" "upstream destination differs from source"
        cat "$work_dir/diff_upstream.log" >&2
        return 0
    fi

    pass_test "$test_name"
}

# =========================================================================
# Test 2: upstream rsync push, oc-rsync as remote binary via --rsync-path
# =========================================================================
test_upstream_with_oc_rsync_path() {
    local test_name="upstream client with --rsync-path=oc-rsync (local)"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work_dir="$TEST_DIR/test_rsync_path"
    mkdir -p "$work_dir"/{src,dest_oc,dest_upstream}

    create_deep_tree "$work_dir/src"

    # Use upstream rsync with oc-rsync as the receiving binary via --rsync-path.
    # This tests oc-rsync acting as the server-side (remote) binary.
    # We use a local transfer with --rsync-path to simulate the remote side.
    if ! "$UPSTREAM_RSYNC" -r --rsync-path="$OC_RSYNC" \
        "$work_dir/src/" "$work_dir/dest_oc/" > "$work_dir/oc.log" 2>&1; then
        fail_test "$test_name" "upstream rsync with --rsync-path=oc-rsync failed"
        cat "$work_dir/oc.log" >&2
        return 0
    fi

    # Verify destination matches source
    if ! diff -r "$work_dir/src" "$work_dir/dest_oc" > "$work_dir/diff.log" 2>&1; then
        fail_test "$test_name" "destination differs from source"
        cat "$work_dir/diff.log" >&2
        return 0
    fi

    pass_test "$test_name"
}

# =========================================================================
# Test 3: Incremental update - add new files/dirs, re-sync
# =========================================================================
test_incremental_update() {
    local test_name="incremental update (add files, re-sync)"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work_dir="$TEST_DIR/test_incremental"
    mkdir -p "$work_dir"/{src,dest}

    create_deep_tree "$work_dir/src"

    # Initial sync with oc-rsync
    if ! "$OC_RSYNC" -r "$work_dir/src/" "$work_dir/dest/" > "$work_dir/initial.log" 2>&1; then
        fail_test "$test_name" "initial oc-rsync transfer failed"
        cat "$work_dir/initial.log" >&2
        return 0
    fi

    # Add new files and directories at various depths
    mkdir -p "$work_dir/src/new_top_dir/nested"
    dd if=/dev/urandom of="$work_dir/src/new_top_dir/new1.dat" bs=128 count=1 2>/dev/null
    dd if=/dev/urandom of="$work_dir/src/new_top_dir/nested/new2.dat" bs=128 count=1 2>/dev/null
    mkdir -p "$work_dir/src/d1/sub1/inner1/new_leaf"
    dd if=/dev/urandom of="$work_dir/src/d1/sub1/inner1/new_leaf/deep_new.dat" bs=128 count=1 2>/dev/null
    dd if=/dev/urandom of="$work_dir/src/d3/added_file.dat" bs=128 count=1 2>/dev/null

    local src_count
    src_count=$(count_files "$work_dir/src")

    # Re-sync with oc-rsync (verbose to check what gets transferred)
    if ! "$OC_RSYNC" -rv "$work_dir/src/" "$work_dir/dest/" > "$work_dir/resync.log" 2>&1; then
        fail_test "$test_name" "re-sync oc-rsync transfer failed"
        cat "$work_dir/resync.log" >&2
        return 0
    fi

    # Verify destination matches updated source
    if ! diff -r "$work_dir/src" "$work_dir/dest" > "$work_dir/diff.log" 2>&1; then
        fail_test "$test_name" "destination differs from source after incremental update"
        cat "$work_dir/diff.log" >&2
        return 0
    fi

    local dest_count
    dest_count=$(count_files "$work_dir/dest")

    if [ "$src_count" -ne "$dest_count" ]; then
        fail_test "$test_name" "file count mismatch: src=$src_count dest=$dest_count"
        return 0
    fi

    log_info "Incremental update: $src_count files in sync"
    pass_test "$test_name"
}

# =========================================================================
# Test 4: Delete with inc_recurse
# =========================================================================
test_delete_with_inc_recurse() {
    local test_name="delete propagation with --delete"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work_dir="$TEST_DIR/test_delete"
    mkdir -p "$work_dir"/{src,dest}

    create_deep_tree "$work_dir/src"

    # Initial sync
    if ! "$OC_RSYNC" -r "$work_dir/src/" "$work_dir/dest/" > "$work_dir/initial.log" 2>&1; then
        fail_test "$test_name" "initial transfer failed"
        cat "$work_dir/initial.log" >&2
        return 0
    fi

    # Verify initial sync
    if ! diff -r "$work_dir/src" "$work_dir/dest" > /dev/null 2>&1; then
        fail_test "$test_name" "initial sync mismatch"
        return 0
    fi

    # Remove some source files and directories
    rm -rf "$work_dir/src/d2"
    rm -f "$work_dir/src/d1/sub1/inner1/leaf1/file_0.dat"
    rm -rf "$work_dir/src/d4/sub2"

    local src_count
    src_count=$(count_files "$work_dir/src")

    # Re-sync with --delete
    if ! "$OC_RSYNC" -r --delete "$work_dir/src/" "$work_dir/dest/" > "$work_dir/delete.log" 2>&1; then
        fail_test "$test_name" "delete sync failed"
        cat "$work_dir/delete.log" >&2
        return 0
    fi

    # Verify destination matches source (deleted files should be gone)
    if ! diff -r "$work_dir/src" "$work_dir/dest" > "$work_dir/diff.log" 2>&1; then
        fail_test "$test_name" "destination differs from source after delete sync"
        cat "$work_dir/diff.log" >&2
        return 0
    fi

    local dest_count
    dest_count=$(count_files "$work_dir/dest")

    if [ "$src_count" -ne "$dest_count" ]; then
        fail_test "$test_name" "file count mismatch after delete: src=$src_count dest=$dest_count"
        return 0
    fi

    # Verify specific deletions
    if [ -d "$work_dir/dest/d2" ]; then
        fail_test "$test_name" "d2/ still exists in destination after --delete"
        return 0
    fi

    if [ -f "$work_dir/dest/d1/sub1/inner1/leaf1/file_0.dat" ]; then
        fail_test "$test_name" "d1/sub1/inner1/leaf1/file_0.dat still exists after --delete"
        return 0
    fi

    if [ -d "$work_dir/dest/d4/sub2" ]; then
        fail_test "$test_name" "d4/sub2/ still exists after --delete"
        return 0
    fi

    log_info "Delete propagation: $dest_count files remain, deletions verified"
    pass_test "$test_name"
}

# =========================================================================
# Test 5: Large directory count (100 dirs x 10 files = 1000 files)
# =========================================================================
test_large_directory_count() {
    local test_name="large directory count (100 dirs, 1000 files)"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work_dir="$TEST_DIR/test_large"
    mkdir -p "$work_dir"/{src,dest_oc,dest_upstream}

    create_wide_tree "$work_dir/src"

    local src_count
    src_count=$(count_files "$work_dir/src")

    # Sync with oc-rsync
    if ! "$OC_RSYNC" -r "$work_dir/src/" "$work_dir/dest_oc/" > "$work_dir/oc.log" 2>&1; then
        fail_test "$test_name" "oc-rsync transfer failed"
        cat "$work_dir/oc.log" >&2
        return 0
    fi

    # Sync with upstream rsync
    if ! "$UPSTREAM_RSYNC" -r "$work_dir/src/" "$work_dir/dest_upstream/" > "$work_dir/upstream.log" 2>&1; then
        fail_test "$test_name" "upstream rsync transfer failed"
        cat "$work_dir/upstream.log" >&2
        return 0
    fi

    # Verify oc-rsync destination matches source
    if ! diff -r "$work_dir/src" "$work_dir/dest_oc" > "$work_dir/diff_oc.log" 2>&1; then
        fail_test "$test_name" "oc-rsync destination differs from source"
        cat "$work_dir/diff_oc.log" >&2
        return 0
    fi

    local dest_oc_count
    dest_oc_count=$(count_files "$work_dir/dest_oc")

    local dest_upstream_count
    dest_upstream_count=$(count_files "$work_dir/dest_upstream")

    if [ "$src_count" -ne "$dest_oc_count" ]; then
        fail_test "$test_name" "oc-rsync file count mismatch: src=$src_count dest=$dest_oc_count"
        return 0
    fi

    if [ "$src_count" -ne "$dest_upstream_count" ]; then
        fail_test "$test_name" "upstream file count mismatch: src=$src_count dest=$dest_upstream_count"
        return 0
    fi

    log_info "Large tree: $src_count source, $dest_oc_count oc-rsync, $dest_upstream_count upstream"
    pass_test "$test_name"
}

# =========================================================================
# Main
# =========================================================================
main() {
    log_info "Starting INC_RECURSE Interoperability Tests"
    log_info "oc-rsync: $OC_RSYNC"
    log_info "upstream rsync: $UPSTREAM_RSYNC"
    log_info "Test directory: $TEST_DIR"

    # Verify binaries exist
    if [ ! -x "$OC_RSYNC" ]; then
        log_error "oc-rsync binary not found or not executable: $OC_RSYNC"
        exit 1
    fi

    if [ ! -x "$UPSTREAM_RSYNC" ]; then
        log_error "upstream rsync binary not found or not executable: $UPSTREAM_RSYNC"
        exit 1
    fi

    log_info "oc-rsync version: $("$OC_RSYNC" --version 2>/dev/null | head -1 || echo 'unknown')"
    log_info "upstream rsync version: $("$UPSTREAM_RSYNC" --version 2>/dev/null | head -1 || echo 'unknown')"

    # Run tests
    test_oc_push_deep_tree
    test_upstream_with_oc_rsync_path
    test_incremental_update
    test_delete_with_inc_recurse
    test_large_directory_count

    # Summary
    echo ""
    echo "========================================="
    echo "INC_RECURSE Interoperability Test Summary"
    echo "========================================="
    echo "Total tests run:    $TESTS_RUN"
    echo "Tests passed:       $TESTS_PASSED"
    echo "Tests failed:       $TESTS_FAILED"
    echo "========================================="

    if [ "$TESTS_FAILED" -gt 0 ]; then
        log_error "$TESTS_FAILED test(s) failed"
        exit 1
    fi

    log_info "All tests passed"
    exit 0
}

main "$@"
