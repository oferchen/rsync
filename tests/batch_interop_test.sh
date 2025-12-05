#!/bin/bash
# Batch Mode Interoperability Test Script
#
# Tests batch mode compatibility between oc-rsync and upstream rsync versions.
# Validates both directions:
#   1. oc-rsync creates batch → upstream rsync reads it
#   2. upstream rsync creates batch → oc-rsync reads it
#
# Tests across upstream versions: 3.0.9, 3.1.3, 3.4.1

set -e

# Color output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Paths
OC_RSYNC="/home/ofer/rsync/target/debug/oc-rsync"
UPSTREAM_309="/home/ofer/rsync/target/interop/upstream-install/3.0.9/bin/rsync"
UPSTREAM_313="/home/ofer/rsync/target/interop/upstream-install/3.1.3/bin/rsync"
UPSTREAM_341="/home/ofer/rsync/target/interop/upstream-install/3.4.1/bin/rsync"
TEST_DIR="/tmp/batch_interop_test"

# Counters
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

log_test() {
    echo -e "\n${YELLOW}=== $1 ===${NC}"
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

    local sum1=$(md5sum "$file1" | awk '{print $1}')
    local sum2=$(md5sum "$file2" | awk '{print $1}')

    if [ "$sum1" = "$sum2" ]; then
        log_info "$test_name: ✓ Files match (MD5: $sum1)"
        return 0
    else
        log_error "$test_name: ✗ Files differ (MD5: $sum1 vs $sum2)"
        return 1
    fi
}

test_oc_to_upstream() {
    local upstream_rsync="$1"
    local version="$2"
    local test_name="oc-rsync → upstream $version"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work_dir="$TEST_DIR/oc_to_${version//./_}"
    rm -rf "$work_dir"
    mkdir -p "$work_dir"/{src,dest,final}

    # Setup test data
    setup_test_data "$work_dir/src" "$work_dir/dest"

    # oc-rsync creates batch
    log_info "Creating batch with oc-rsync..."
    "$OC_RSYNC" -av --no-whole-file --ignore-times \
        --write-batch="$work_dir/mybatch" \
        "$work_dir/src/" "$work_dir/dest/" > /dev/null 2>&1

    if [ ! -f "$work_dir/mybatch" ]; then
        log_error "$test_name: Batch file not created"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 1
    fi

    # Copy basis to final directory
    cp "$work_dir/dest/testfile.bin" "$work_dir/final/testfile.bin"

    # upstream rsync reads batch
    log_info "Replaying batch with upstream rsync $version..."
    "$upstream_rsync" --read-batch="$work_dir/mybatch" "$work_dir/final/" > /dev/null 2>&1

    # Verify result
    if verify_files_match "$work_dir/src/testfile.bin" "$work_dir/final/testfile.bin" "$test_name"; then
        log_info "$test_name: ✓ PASS"
        TESTS_PASSED=$((TESTS_PASSED + 1))
        return 0
    else
        log_error "$test_name: ✗ FAIL"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 1
    fi
}

test_upstream_to_oc() {
    local upstream_rsync="$1"
    local version="$2"
    local test_name="upstream $version → oc-rsync"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work_dir="$TEST_DIR/${version//./_}_to_oc"
    rm -rf "$work_dir"
    mkdir -p "$work_dir"/{src,dest,final}

    # Setup test data
    setup_test_data "$work_dir/src" "$work_dir/dest"

    # upstream rsync creates batch
    log_info "Creating batch with upstream rsync $version..."
    "$upstream_rsync" -av --no-whole-file --ignore-times \
        --write-batch="$work_dir/mybatch" \
        "$work_dir/src/" "$work_dir/dest/" > /dev/null 2>&1

    if [ ! -f "$work_dir/mybatch" ]; then
        log_error "$test_name: Batch file not created"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 1
    fi

    # Copy basis to final directory
    cp "$work_dir/dest/testfile.bin" "$work_dir/final/testfile.bin"

    # oc-rsync reads batch
    log_info "Replaying batch with oc-rsync..."
    "$OC_RSYNC" --read-batch="$work_dir/mybatch" "$work_dir/final/" > /dev/null 2>&1

    # Verify result
    if verify_files_match "$work_dir/src/testfile.bin" "$work_dir/final/testfile.bin" "$test_name"; then
        log_info "$test_name: ✓ PASS"
        TESTS_PASSED=$((TESTS_PASSED + 1))
        return 0
    else
        log_error "$test_name: ✗ FAIL"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 1
    fi
}

main() {
    log_info "Starting Batch Mode Interoperability Tests"
    log_info "oc-rsync: $OC_RSYNC"

    # Verify binaries exist
    for binary in "$OC_RSYNC" "$UPSTREAM_309" "$UPSTREAM_313" "$UPSTREAM_341"; do
        if [ ! -x "$binary" ]; then
            log_error "Binary not found or not executable: $binary"
            exit 1
        fi
    done

    # Clean test directory
    rm -rf "$TEST_DIR"
    mkdir -p "$TEST_DIR"

    # Test oc-rsync → upstream (all versions)
    log_info "\n📦 Testing oc-rsync creates batch → upstream reads"
    test_oc_to_upstream "$UPSTREAM_309" "3.0.9"
    test_oc_to_upstream "$UPSTREAM_313" "3.1.3"
    test_oc_to_upstream "$UPSTREAM_341" "3.4.1"

    # Test upstream → oc-rsync (all versions)
    log_info "\n📦 Testing upstream creates batch → oc-rsync reads"
    test_upstream_to_oc "$UPSTREAM_309" "3.0.9"
    test_upstream_to_oc "$UPSTREAM_313" "3.1.3"
    test_upstream_to_oc "$UPSTREAM_341" "3.4.1"

    # Summary
    echo ""
    echo "========================================="
    echo "Batch Mode Interoperability Test Summary"
    echo "========================================="
    echo "Total tests run:    $TESTS_RUN"
    echo -e "Tests passed:       ${GREEN}$TESTS_PASSED${NC}"
    echo -e "Tests failed:       ${RED}$TESTS_FAILED${NC}"
    echo "========================================="

    if [ $TESTS_FAILED -eq 0 ]; then
        log_info "✅ All batch interop tests passed!"
        exit 0
    else
        log_error "❌ Some batch interop tests failed"
        exit 1
    fi
}

main "$@"
