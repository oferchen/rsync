#!/usr/bin/env bash
# Batch Mode Interoperability Test Script
#
# Tests batch mode compatibility between oc-rsync and upstream rsync versions.
# Validates both directions:
#   1. oc-rsync creates batch -> upstream rsync reads it
#   2. upstream rsync creates batch -> oc-rsync reads it
#
# Tests across upstream versions: 3.0.9, 3.1.3, 3.4.1
#
# Environment variable overrides:
#   OC_RSYNC              - path to oc-rsync binary
#   UPSTREAM_INSTALL_ROOT - root of upstream installs (expects {version}/bin/rsync)
#   UPSTREAM_VERSIONS     - space-separated list of versions (default: "3.0.9 3.1.3 3.4.1")

set -euo pipefail

# Resolve workspace root from script location
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Paths (overridable via environment)
OC_RSYNC="${OC_RSYNC:-${WORKSPACE_ROOT}/target/release/oc-rsync}"
UPSTREAM_INSTALL_ROOT="${UPSTREAM_INSTALL_ROOT:-${WORKSPACE_ROOT}/target/interop/upstream-install}"
UPSTREAM_VERSIONS="${UPSTREAM_VERSIONS:-3.0.9 3.1.3 3.4.1}"

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

# Cross-platform checksum: prefers md5sum (Linux), falls back to md5 (macOS)
file_checksum() {
    if command -v md5sum >/dev/null 2>&1; then
        md5sum "$1" | awk '{print $1}'
    elif command -v md5 >/dev/null 2>&1; then
        md5 -q "$1"
    else
        # Fallback: compare files byte-for-byte
        echo "NO_MD5"
    fi
}

setup_test_data() {
    local src_dir="$1"
    local dest_dir="$2"

    mkdir -p "$src_dir" "$dest_dir"

    # Create test file with specific pattern for delta mode
    dd if=/dev/zero of="$src_dir/testfile.bin" bs=1K count=100 2>/dev/null

    # Copy to dest (this will be the basis)
    cp "$src_dir/testfile.bin" "$dest_dir/testfile.bin"

    # Modify source to create delta
    dd if=/dev/urandom of="$src_dir/testfile.bin" bs=1 count=100 seek=50000 conv=notrunc 2>/dev/null
}

verify_files_match() {
    local file1="$1"
    local file2="$2"
    local test_name="$3"

    if [ ! -f "$file1" ]; then
        log_error "$test_name: File $file1 does not exist"
        return 1
    fi

    if [ ! -f "$file2" ]; then
        log_error "$test_name: File $file2 does not exist"
        return 1
    fi

    local sum1
    local sum2
    sum1=$(file_checksum "$file1")
    sum2=$(file_checksum "$file2")

    if [ "$sum1" = "NO_MD5" ]; then
        # No checksum tool available; use cmp
        if cmp -s "$file1" "$file2"; then
            log_info "$test_name: Files match (byte comparison)"
            return 0
        else
            log_error "$test_name: Files differ (byte comparison)"
            return 1
        fi
    fi

    if [ "$sum1" = "$sum2" ]; then
        log_info "$test_name: Files match (MD5: $sum1)"
        return 0
    else
        log_error "$test_name: Files differ (MD5: $sum1 vs $sum2)"
        return 1
    fi
}

test_oc_to_upstream() {
    local upstream_rsync="$1"
    local version="$2"
    local test_name="oc-rsync -> upstream $version"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work_dir="$TEST_DIR/oc_to_${version//./_}"
    mkdir -p "$work_dir"/{src,dest,final}

    setup_test_data "$work_dir/src" "$work_dir/dest"

    # oc-rsync creates batch
    log_info "Creating batch with oc-rsync..."
    if ! "$OC_RSYNC" -av --no-whole-file --ignore-times \
        --write-batch="$work_dir/mybatch" \
        "$work_dir/src/" "$work_dir/dest/" > "$work_dir/write.log" 2>&1; then
        log_error "$test_name: oc-rsync --write-batch failed"
        cat "$work_dir/write.log" >&2
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if [ ! -f "$work_dir/mybatch" ]; then
        log_error "$test_name: Batch file not created"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    # Copy basis to final directory
    cp "$work_dir/dest/testfile.bin" "$work_dir/final/testfile.bin"

    # upstream rsync reads batch
    log_info "Replaying batch with upstream rsync $version..."
    if ! "$upstream_rsync" --read-batch="$work_dir/mybatch" "$work_dir/final/" > "$work_dir/read.log" 2>&1; then
        log_error "$test_name: upstream rsync --read-batch failed"
        cat "$work_dir/read.log" >&2
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if verify_files_match "$work_dir/src/testfile.bin" "$work_dir/final/testfile.bin" "$test_name"; then
        log_info "$test_name: PASS"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        log_error "$test_name: FAIL"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

test_upstream_to_oc() {
    local upstream_rsync="$1"
    local version="$2"
    local test_name="upstream $version -> oc-rsync"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work_dir="$TEST_DIR/${version//./_}_to_oc"
    mkdir -p "$work_dir"/{src,dest,final}

    setup_test_data "$work_dir/src" "$work_dir/dest"

    # upstream rsync creates batch
    log_info "Creating batch with upstream rsync $version..."
    if ! "$upstream_rsync" -av --no-whole-file --ignore-times \
        --write-batch="$work_dir/mybatch" \
        "$work_dir/src/" "$work_dir/dest/" > "$work_dir/write.log" 2>&1; then
        log_error "$test_name: upstream rsync --write-batch failed"
        cat "$work_dir/write.log" >&2
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if [ ! -f "$work_dir/mybatch" ]; then
        log_error "$test_name: Batch file not created"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    # Copy basis to final directory
    cp "$work_dir/dest/testfile.bin" "$work_dir/final/testfile.bin"

    # oc-rsync reads batch
    log_info "Replaying batch with oc-rsync..."
    if ! "$OC_RSYNC" --read-batch="$work_dir/mybatch" "$work_dir/final/" > "$work_dir/read.log" 2>&1; then
        log_error "$test_name: oc-rsync --read-batch failed"
        cat "$work_dir/read.log" >&2
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if verify_files_match "$work_dir/src/testfile.bin" "$work_dir/final/testfile.bin" "$test_name"; then
        log_info "$test_name: PASS"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        log_error "$test_name: FAIL"
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

main() {
    log_info "Starting Batch Mode Interoperability Tests"
    log_info "oc-rsync: $OC_RSYNC"
    log_info "Upstream install root: $UPSTREAM_INSTALL_ROOT"
    log_info "Test directory: $TEST_DIR"

    # Verify oc-rsync binary exists
    if [ ! -x "$OC_RSYNC" ]; then
        log_error "oc-rsync binary not found or not executable: $OC_RSYNC"
        exit 1
    fi

    # Build list of available upstream versions
    local available_versions=()
    for version in $UPSTREAM_VERSIONS; do
        local binary="$UPSTREAM_INSTALL_ROOT/$version/bin/rsync"
        if [ -x "$binary" ]; then
            available_versions+=("$version")
        else
            log_warn "Upstream rsync $version not found at $binary, skipping"
            TESTS_SKIPPED=$((TESTS_SKIPPED + 2))
        fi
    done

    if [ ${#available_versions[@]} -eq 0 ]; then
        log_error "No upstream rsync versions available"
        exit 1
    fi

    # Test oc-rsync -> upstream (available versions)
    log_info "Testing oc-rsync creates batch -> upstream reads"
    for version in "${available_versions[@]}"; do
        test_oc_to_upstream "$UPSTREAM_INSTALL_ROOT/$version/bin/rsync" "$version"
    done

    # Test upstream -> oc-rsync (available versions)
    log_info "Testing upstream creates batch -> oc-rsync reads"
    for version in "${available_versions[@]}"; do
        test_upstream_to_oc "$UPSTREAM_INSTALL_ROOT/$version/bin/rsync" "$version"
    done

    # Summary
    echo ""
    echo "========================================="
    echo "Batch Mode Interoperability Test Summary"
    echo "========================================="
    echo "Total tests run:    $TESTS_RUN"
    echo "Tests passed:       $TESTS_PASSED"
    echo "Tests failed:       $TESTS_FAILED"
    echo "Tests skipped:      $TESTS_SKIPPED"
    echo "========================================="

    if [ $TESTS_FAILED -eq 0 ]; then
        log_info "All batch interop tests passed!"
        exit 0
    else
        log_error "Some batch interop tests failed"
        exit 1
    fi
}

main "$@"
