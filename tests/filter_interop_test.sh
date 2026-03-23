#!/usr/bin/env bash
# Filter Rules Interoperability Test Script
#
# Tests filter rule compatibility between oc-rsync and upstream rsync.
# Validates that --exclude, --include/--exclude precedence, --filter merge
# files, and --delete with filters produce identical results.
#
# Environment variable overrides:
#   OC_RSYNC              - path to oc-rsync binary
#   UPSTREAM_INSTALL_ROOT - root of upstream installs (expects {version}/bin/rsync)
#   UPSTREAM_VERSION      - upstream version to test against (default: "3.4.1")

set -euo pipefail

# Resolve workspace root from script location
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Paths (overridable via environment)
OC_RSYNC="${OC_RSYNC:-${WORKSPACE_ROOT}/target/release/oc-rsync}"
UPSTREAM_INSTALL_ROOT="${UPSTREAM_INSTALL_ROOT:-${WORKSPACE_ROOT}/target/interop/upstream-install}"
UPSTREAM_VERSION="${UPSTREAM_VERSION:-3.4.1}"
UPSTREAM_RSYNC="${UPSTREAM_INSTALL_ROOT}/${UPSTREAM_VERSION}/bin/rsync"

# Create a temp directory with cleanup trap
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

# Counters
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0
TESTS_SKIPPED=0

log_info() {
    echo "[INFO] $1"
}

log_warn() {
    echo "[WARN] $1"
}

log_error() {
    echo "[ERROR] $1" >&2
}

log_test() {
    echo ""
    echo "=== $1 ==="
}

# Compare two directories and report result
compare_dirs() {
    local dir1="$1"
    local dir2="$2"
    local test_name="$3"

    local diff_output
    if diff_output=$(diff -r "$dir1" "$dir2" 2>&1); then
        log_info "$test_name: directories match"
        return 0
    else
        log_error "$test_name: directories differ"
        echo "$diff_output" >&2
        return 1
    fi
}

# Create a common set of test fixtures for filter tests
setup_filter_fixtures() {
    local src_dir="$1"
    mkdir -p "$src_dir/subdir"

    echo "hello world" > "$src_dir/readme.txt"
    echo "data file" > "$src_dir/data.csv"
    echo "log entry 1" > "$src_dir/app.log"
    echo "log entry 2" > "$src_dir/error.log"
    echo "build artifact" > "$src_dir/output.o"
    echo "sub text" > "$src_dir/subdir/notes.txt"
    echo "sub log" > "$src_dir/subdir/debug.log"
    echo "sub data" > "$src_dir/subdir/report.csv"
}

# =========================================================================
# Test 1: --exclude pattern
# =========================================================================

test_exclude_pattern() {
    local test_name="--exclude '*.log' pattern"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work_dir="$TEST_DIR/exclude_pattern"
    mkdir -p "$work_dir"/{src,dest_upstream,dest_oc}

    setup_filter_fixtures "$work_dir/src"

    log_info "Running upstream rsync with --exclude '*.log'..."
    if ! "$UPSTREAM_RSYNC" -av --exclude '*.log' \
        "$work_dir/src/" "$work_dir/dest_upstream/" > "$work_dir/upstream.log" 2>&1; then
        log_error "$test_name: upstream rsync failed"
        cat "$work_dir/upstream.log" >&2
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    log_info "Running oc-rsync with --exclude '*.log'..."
    if ! "$OC_RSYNC" -av --exclude '*.log' \
        "$work_dir/src/" "$work_dir/dest_oc/" > "$work_dir/oc.log" 2>&1; then
        log_error "$test_name: oc-rsync failed"
        cat "$work_dir/oc.log" >&2
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if compare_dirs "$work_dir/dest_upstream" "$work_dir/dest_oc" "$test_name"; then
        log_info "$test_name: PASS"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        log_error "$test_name: FAIL"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

# =========================================================================
# Test 2: --include/--exclude precedence
# =========================================================================

test_include_exclude_precedence() {
    local test_name="--include '*.txt' --exclude '*' precedence"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work_dir="$TEST_DIR/include_exclude"
    mkdir -p "$work_dir"/{src,dest_upstream,dest_oc}

    setup_filter_fixtures "$work_dir/src"

    log_info "Running upstream rsync with --include '*.txt' --exclude '*'..."
    if ! "$UPSTREAM_RSYNC" -av \
        --include '*.txt' --include '*/' --exclude '*' \
        "$work_dir/src/" "$work_dir/dest_upstream/" > "$work_dir/upstream.log" 2>&1; then
        log_error "$test_name: upstream rsync failed"
        cat "$work_dir/upstream.log" >&2
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    log_info "Running oc-rsync with --include '*.txt' --exclude '*'..."
    if ! "$OC_RSYNC" -av \
        --include '*.txt' --include '*/' --exclude '*' \
        "$work_dir/src/" "$work_dir/dest_oc/" > "$work_dir/oc.log" 2>&1; then
        log_error "$test_name: oc-rsync failed"
        cat "$work_dir/oc.log" >&2
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if compare_dirs "$work_dir/dest_upstream" "$work_dir/dest_oc" "$test_name"; then
        log_info "$test_name: PASS"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        log_error "$test_name: FAIL"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

# =========================================================================
# Test 3: --filter merge file
# =========================================================================

test_filter_merge_file() {
    local test_name="--filter 'merge .rsync-filter' merge file"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work_dir="$TEST_DIR/filter_merge"
    mkdir -p "$work_dir"/{src,dest_upstream,dest_oc}

    setup_filter_fixtures "$work_dir/src"

    # Create a .rsync-filter file in the source directory
    cat > "$work_dir/src/.rsync-filter" <<'FILTER'
- *.log
- *.o
FILTER

    log_info "Running upstream rsync with --filter 'merge .rsync-filter'..."
    if ! "$UPSTREAM_RSYNC" -av --filter 'merge .rsync-filter' \
        "$work_dir/src/" "$work_dir/dest_upstream/" > "$work_dir/upstream.log" 2>&1; then
        log_error "$test_name: upstream rsync failed"
        cat "$work_dir/upstream.log" >&2
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    log_info "Running oc-rsync with --filter 'merge .rsync-filter'..."
    if ! "$OC_RSYNC" -av --filter 'merge .rsync-filter' \
        "$work_dir/src/" "$work_dir/dest_oc/" > "$work_dir/oc.log" 2>&1; then
        log_error "$test_name: oc-rsync failed"
        cat "$work_dir/oc.log" >&2
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if compare_dirs "$work_dir/dest_upstream" "$work_dir/dest_oc" "$test_name"; then
        log_info "$test_name: PASS"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        log_error "$test_name: FAIL"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

# =========================================================================
# Test 4: --delete with filters
# =========================================================================

test_delete_with_filters() {
    local test_name="--delete --exclude '*.keep' with filters"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work_dir="$TEST_DIR/delete_filters"
    mkdir -p "$work_dir"/{src,dest_upstream,dest_oc}

    # Source has a few files
    echo "source file" > "$work_dir/src/wanted.txt"
    echo "also source" > "$work_dir/src/also.dat"

    # Pre-populate both destinations identically with extra files
    # that should be deleted, and .keep files that should be preserved
    for dest in "$work_dir/dest_upstream" "$work_dir/dest_oc"; do
        echo "source file" > "$dest/wanted.txt"
        echo "also source" > "$dest/also.dat"
        echo "stale file" > "$dest/stale.txt"
        echo "old data" > "$dest/old.dat"
        echo "preserve me" > "$dest/important.keep"
        echo "also preserve" > "$dest/backup.keep"
    done

    log_info "Running upstream rsync with --delete --exclude '*.keep'..."
    if ! "$UPSTREAM_RSYNC" -av --delete --exclude '*.keep' \
        "$work_dir/src/" "$work_dir/dest_upstream/" > "$work_dir/upstream.log" 2>&1; then
        log_error "$test_name: upstream rsync failed"
        cat "$work_dir/upstream.log" >&2
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    log_info "Running oc-rsync with --delete --exclude '*.keep'..."
    if ! "$OC_RSYNC" -av --delete --exclude '*.keep' \
        "$work_dir/src/" "$work_dir/dest_oc/" > "$work_dir/oc.log" 2>&1; then
        log_error "$test_name: oc-rsync failed"
        cat "$work_dir/oc.log" >&2
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if compare_dirs "$work_dir/dest_upstream" "$work_dir/dest_oc" "$test_name"; then
        log_info "$test_name: PASS"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        log_error "$test_name: FAIL"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

# =========================================================================
# Main
# =========================================================================

main() {
    log_info "Starting Filter Rules Interoperability Tests"
    log_info "oc-rsync: $OC_RSYNC"
    log_info "Upstream rsync: $UPSTREAM_RSYNC"
    log_info "Test directory: $TEST_DIR"

    # Verify oc-rsync binary exists
    if [ ! -x "$OC_RSYNC" ]; then
        log_error "oc-rsync binary not found or not executable: $OC_RSYNC"
        exit 1
    fi

    # Verify upstream rsync binary exists
    if [ ! -x "$UPSTREAM_RSYNC" ]; then
        log_error "upstream rsync not found or not executable: $UPSTREAM_RSYNC"
        log_warn "Skipping all filter interop tests"
        TESTS_SKIPPED=4
        echo ""
        echo "========================================="
        echo "Filter Rules Interoperability Test Summary"
        echo "========================================="
        echo "Total tests run:    0"
        echo "Tests passed:       0"
        echo "Tests failed:       0"
        echo "Tests skipped:      $TESTS_SKIPPED"
        echo "========================================="
        exit 0
    fi

    test_exclude_pattern
    test_include_exclude_precedence
    test_filter_merge_file
    test_delete_with_filters

    # Summary
    echo ""
    echo "========================================="
    echo "Filter Rules Interoperability Test Summary"
    echo "========================================="
    echo "Total tests run:    $TESTS_RUN"
    echo "Tests passed:       $TESTS_PASSED"
    echo "Tests failed:       $TESTS_FAILED"
    echo "Tests skipped:      $TESTS_SKIPPED"
    echo "========================================="

    if [ $TESTS_FAILED -gt 0 ]; then
        log_error "$TESTS_FAILED test(s) failed"
        exit 1
    fi

    if [ $TESTS_PASSED -gt 0 ]; then
        log_info "All $TESTS_PASSED test(s) passed!"
    fi

    exit 0
}

main "$@"
