#!/usr/bin/env bash
# Batch Mode Interoperability Test Script
#
# Tests batch mode compatibility:
#   1. oc-rsync roundtrip: write-batch then read-batch (required to pass)
#   2. Cross-tool: oc-rsync <-> upstream rsync (known limitations, informational)
#
# Cross-tool batch interop is a known limitation: oc-rsync uses a custom
# batch body format (FileEntry serialization) rather than upstream rsync's
# raw protocol stream tee. This means batch files are not interchangeable
# between the two implementations. Cross-tool tests are run for visibility
# but failures do not block CI.
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

# Counters for required tests (must pass)
REQUIRED_RUN=0
REQUIRED_PASSED=0
REQUIRED_FAILED=0

# Counters for cross-tool tests (informational)
XFAIL_RUN=0
XFAIL_PASSED=0
XFAIL_FAILED=0
XFAIL_SKIPPED=0

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

    # Copy to dest (this will be the basis for delta transfer)
    cp "$src_dir/testfile.bin" "$dest_dir/testfile.bin"

    # Modify source to create delta
    dd if=/dev/urandom of="$src_dir/testfile.bin" bs=1 count=100 seek=50000 conv=notrunc 2>/dev/null
}

# Save the original basis before any transfer modifies it.
# The --write-batch flag both performs the transfer AND records the batch,
# so dest/ ends up with the synced file. We need to save the pre-sync
# basis to properly test --read-batch replay.
setup_test_data_with_basis() {
    local src_dir="$1"
    local dest_dir="$2"
    local basis_dir="$3"

    setup_test_data "$src_dir" "$dest_dir"

    # Save original basis BEFORE any transfer modifies dest/
    mkdir -p "$basis_dir"
    cp "$dest_dir/testfile.bin" "$basis_dir/testfile.bin"
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

# =========================================================================
# Required tests: oc-rsync roundtrip (write-batch then read-batch)
# =========================================================================

test_oc_roundtrip() {
    local test_name="oc-rsync roundtrip (write-batch -> read-batch)"

    log_test "$test_name"
    REQUIRED_RUN=$((REQUIRED_RUN + 1))

    local work_dir="$TEST_DIR/oc_roundtrip"
    mkdir -p "$work_dir"/{src,dest,basis,final}

    setup_test_data_with_basis "$work_dir/src" "$work_dir/dest" "$work_dir/basis"

    # oc-rsync creates batch (also performs the transfer to dest/)
    log_info "Creating batch with oc-rsync..."
    if ! "$OC_RSYNC" -av --no-whole-file --ignore-times \
        --write-batch="$work_dir/mybatch" \
        "$work_dir/src/" "$work_dir/dest/" > "$work_dir/write.log" 2>&1; then
        log_error "$test_name: oc-rsync --write-batch failed"
        cat "$work_dir/write.log" >&2
        REQUIRED_FAILED=$((REQUIRED_FAILED + 1))
        return 0
    fi

    if [ ! -f "$work_dir/mybatch" ]; then
        log_error "$test_name: Batch file not created"
        REQUIRED_FAILED=$((REQUIRED_FAILED + 1))
        return 0
    fi

    # Copy the ORIGINAL basis (pre-sync) to final/ so replay must apply deltas
    cp "$work_dir/basis/testfile.bin" "$work_dir/final/testfile.bin"

    # oc-rsync replays the batch
    log_info "Replaying batch with oc-rsync..."
    if ! "$OC_RSYNC" --read-batch="$work_dir/mybatch" "$work_dir/final/" > "$work_dir/read.log" 2>&1; then
        log_error "$test_name: oc-rsync --read-batch failed"
        cat "$work_dir/read.log" >&2
        REQUIRED_FAILED=$((REQUIRED_FAILED + 1))
        return 0
    fi

    if verify_files_match "$work_dir/src/testfile.bin" "$work_dir/final/testfile.bin" "$test_name"; then
        log_info "$test_name: PASS"
        REQUIRED_PASSED=$((REQUIRED_PASSED + 1))
    else
        log_error "$test_name: FAIL"
        REQUIRED_FAILED=$((REQUIRED_FAILED + 1))
    fi
}

test_oc_roundtrip_whole_file() {
    local test_name="oc-rsync roundtrip whole-file (write-batch -> read-batch)"

    log_test "$test_name"
    REQUIRED_RUN=$((REQUIRED_RUN + 1))

    local work_dir="$TEST_DIR/oc_roundtrip_whole"
    mkdir -p "$work_dir"/{src,dest,final}

    # Create source file (no basis needed for whole-file mode)
    mkdir -p "$work_dir/src"
    dd if=/dev/urandom of="$work_dir/src/newfile.bin" bs=1K count=50 2>/dev/null

    # Empty destination (whole-file transfer)
    mkdir -p "$work_dir/dest"

    # oc-rsync creates batch
    log_info "Creating batch with oc-rsync (whole-file)..."
    if ! "$OC_RSYNC" -av \
        --write-batch="$work_dir/mybatch" \
        "$work_dir/src/" "$work_dir/dest/" > "$work_dir/write.log" 2>&1; then
        log_error "$test_name: oc-rsync --write-batch failed"
        cat "$work_dir/write.log" >&2
        REQUIRED_FAILED=$((REQUIRED_FAILED + 1))
        return 0
    fi

    if [ ! -f "$work_dir/mybatch" ]; then
        log_error "$test_name: Batch file not created"
        REQUIRED_FAILED=$((REQUIRED_FAILED + 1))
        return 0
    fi

    # Empty final directory — replay must create the file from scratch
    mkdir -p "$work_dir/final"

    # oc-rsync replays the batch
    log_info "Replaying batch with oc-rsync..."
    if ! "$OC_RSYNC" --read-batch="$work_dir/mybatch" "$work_dir/final/" > "$work_dir/read.log" 2>&1; then
        log_error "$test_name: oc-rsync --read-batch failed"
        cat "$work_dir/read.log" >&2
        REQUIRED_FAILED=$((REQUIRED_FAILED + 1))
        return 0
    fi

    if verify_files_match "$work_dir/src/newfile.bin" "$work_dir/final/newfile.bin" "$test_name"; then
        log_info "$test_name: PASS"
        REQUIRED_PASSED=$((REQUIRED_PASSED + 1))
    else
        log_error "$test_name: FAIL"
        REQUIRED_FAILED=$((REQUIRED_FAILED + 1))
    fi
}

# =========================================================================
# Cross-tool tests (informational — known limitations)
# =========================================================================

test_oc_to_upstream() {
    local upstream_rsync="$1"
    local version="$2"
    local test_name="[xfail] oc-rsync -> upstream $version"

    log_test "$test_name"
    XFAIL_RUN=$((XFAIL_RUN + 1))

    local work_dir="$TEST_DIR/oc_to_${version//./_}"
    mkdir -p "$work_dir"/{src,dest,basis,final}

    setup_test_data_with_basis "$work_dir/src" "$work_dir/dest" "$work_dir/basis"

    # oc-rsync creates batch
    log_info "Creating batch with oc-rsync..."
    if ! "$OC_RSYNC" -av --no-whole-file --ignore-times \
        --write-batch="$work_dir/mybatch" \
        "$work_dir/src/" "$work_dir/dest/" > "$work_dir/write.log" 2>&1; then
        log_warn "$test_name: oc-rsync --write-batch failed (expected)"
        cat "$work_dir/write.log" >&2
        XFAIL_FAILED=$((XFAIL_FAILED + 1))
        return 0
    fi

    if [ ! -f "$work_dir/mybatch" ]; then
        log_warn "$test_name: Batch file not created (expected)"
        XFAIL_FAILED=$((XFAIL_FAILED + 1))
        return 0
    fi

    # Copy original basis to final directory
    cp "$work_dir/basis/testfile.bin" "$work_dir/final/testfile.bin"

    # upstream rsync reads batch
    log_info "Replaying batch with upstream rsync $version..."
    if ! "$upstream_rsync" --read-batch="$work_dir/mybatch" "$work_dir/final/" > "$work_dir/read.log" 2>&1; then
        log_warn "$test_name: upstream rsync --read-batch failed (expected — custom batch format)"
        cat "$work_dir/read.log" >&2
        XFAIL_FAILED=$((XFAIL_FAILED + 1))
        return 0
    fi

    if verify_files_match "$work_dir/src/testfile.bin" "$work_dir/final/testfile.bin" "$test_name"; then
        log_info "$test_name: PASS (unexpected success)"
        XFAIL_PASSED=$((XFAIL_PASSED + 1))
    else
        log_warn "$test_name: files differ (expected — custom batch format)"
        XFAIL_FAILED=$((XFAIL_FAILED + 1))
    fi
}

test_upstream_to_oc() {
    local upstream_rsync="$1"
    local version="$2"
    local test_name="[xfail] upstream $version -> oc-rsync"

    log_test "$test_name"
    XFAIL_RUN=$((XFAIL_RUN + 1))

    local work_dir="$TEST_DIR/${version//./_}_to_oc"
    mkdir -p "$work_dir"/{src,dest,basis,final}

    setup_test_data_with_basis "$work_dir/src" "$work_dir/dest" "$work_dir/basis"

    # upstream rsync creates batch
    log_info "Creating batch with upstream rsync $version..."
    if ! "$upstream_rsync" -av --no-whole-file --ignore-times \
        --write-batch="$work_dir/mybatch" \
        "$work_dir/src/" "$work_dir/dest/" > "$work_dir/write.log" 2>&1; then
        log_warn "$test_name: upstream rsync --write-batch failed (expected)"
        cat "$work_dir/write.log" >&2
        XFAIL_FAILED=$((XFAIL_FAILED + 1))
        return 0
    fi

    if [ ! -f "$work_dir/mybatch" ]; then
        log_warn "$test_name: Batch file not created (expected)"
        XFAIL_FAILED=$((XFAIL_FAILED + 1))
        return 0
    fi

    # Copy original basis to final directory
    cp "$work_dir/basis/testfile.bin" "$work_dir/final/testfile.bin"

    # oc-rsync reads batch
    log_info "Replaying batch with oc-rsync..."
    if ! "$OC_RSYNC" --read-batch="$work_dir/mybatch" "$work_dir/final/" > "$work_dir/read.log" 2>&1; then
        log_warn "$test_name: oc-rsync --read-batch failed (expected — custom batch format)"
        cat "$work_dir/read.log" >&2
        XFAIL_FAILED=$((XFAIL_FAILED + 1))
        return 0
    fi

    if verify_files_match "$work_dir/src/testfile.bin" "$work_dir/final/testfile.bin" "$test_name"; then
        log_info "$test_name: PASS (unexpected success)"
        XFAIL_PASSED=$((XFAIL_PASSED + 1))
    else
        log_warn "$test_name: files differ (expected — custom batch format)"
        XFAIL_FAILED=$((XFAIL_FAILED + 1))
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

    # =====================================================================
    # Required tests: oc-rsync roundtrip
    # =====================================================================
    log_info "=== Required Tests: oc-rsync Roundtrip ==="
    test_oc_roundtrip
    test_oc_roundtrip_whole_file

    # =====================================================================
    # Cross-tool tests (informational — expected to fail)
    # =====================================================================
    log_info "=== Informational Tests: Cross-tool Compatibility (expected failures) ==="
    log_info "Note: oc-rsync batch files use a custom format, not upstream's raw"
    log_info "protocol stream tee. Cross-tool interop is a known limitation."

    # Build list of available upstream versions
    local available_versions=()
    for version in $UPSTREAM_VERSIONS; do
        local binary="$UPSTREAM_INSTALL_ROOT/$version/bin/rsync"
        if [ -x "$binary" ]; then
            available_versions+=("$version")
        else
            log_warn "Upstream rsync $version not found at $binary, skipping"
            XFAIL_SKIPPED=$((XFAIL_SKIPPED + 2))
        fi
    done

    if [ ${#available_versions[@]} -gt 0 ]; then
        for version in "${available_versions[@]}"; do
            test_oc_to_upstream "$UPSTREAM_INSTALL_ROOT/$version/bin/rsync" "$version"
        done
        for version in "${available_versions[@]}"; do
            test_upstream_to_oc "$UPSTREAM_INSTALL_ROOT/$version/bin/rsync" "$version"
        done
    else
        log_warn "No upstream rsync versions available, skipping cross-tool tests"
    fi

    # Summary
    echo ""
    echo "========================================="
    echo "Batch Mode Interoperability Test Summary"
    echo "========================================="
    echo ""
    echo "Required tests (oc-rsync roundtrip):"
    echo "  Run:    $REQUIRED_RUN"
    echo "  Passed: $REQUIRED_PASSED"
    echo "  Failed: $REQUIRED_FAILED"
    echo ""
    echo "Cross-tool tests (informational, expected failures):"
    echo "  Run:     $XFAIL_RUN"
    echo "  Passed:  $XFAIL_PASSED  (unexpected success)"
    echo "  Failed:  $XFAIL_FAILED  (expected)"
    echo "  Skipped: $XFAIL_SKIPPED"
    echo "========================================="

    if [ $REQUIRED_FAILED -eq 0 ]; then
        log_info "All required batch tests passed!"
        if [ $XFAIL_FAILED -gt 0 ]; then
            log_info "Cross-tool failures are expected (custom batch format)."
        fi
        exit 0
    else
        log_error "$REQUIRED_FAILED required batch test(s) failed"
        exit 1
    fi
}

main "$@"
