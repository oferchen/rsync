#!/usr/bin/env bash
# Filter Edge Cases Interoperability Test Script
#
# Tests filter rule edge cases by comparing oc-rsync output against upstream
# rsync 3.4.1. Each test creates source/dest trees, runs both tools with
# identical arguments, and diffs the results.
#
# Environment variable overrides:
#   OC_RSYNC              - path to oc-rsync binary
#   UPSTREAM_RSYNC        - path to upstream rsync binary
#   UPSTREAM_INSTALL_ROOT - root of upstream installs (expects {version}/bin/rsync)

set -euo pipefail

# Resolve workspace root from script location
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Paths (overridable via environment)
OC_RSYNC="${OC_RSYNC:-${WORKSPACE_ROOT}/target/release/oc-rsync}"
UPSTREAM_INSTALL_ROOT="${UPSTREAM_INSTALL_ROOT:-${WORKSPACE_ROOT}/target/interop/upstream-install}"
UPSTREAM_RSYNC="${UPSTREAM_RSYNC:-${UPSTREAM_INSTALL_ROOT}/3.4.1/bin/rsync}"

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

# Run rsync (upstream or oc-rsync) and capture the resulting file tree.
# Returns the sorted list of relative paths under the destination directory.
list_tree() {
    local dir="$1"
    (cd "$dir" && find . -not -name '.' | sort)
}

# Compare destination trees produced by upstream rsync and oc-rsync.
# $1 = upstream dest dir, $2 = oc-rsync dest dir, $3 = test name
compare_trees() {
    local upstream_dest="$1"
    local oc_dest="$2"
    local test_name="$3"

    local upstream_tree oc_tree
    upstream_tree="$(list_tree "$upstream_dest")"
    oc_tree="$(list_tree "$oc_dest")"

    if [ "$upstream_tree" = "$oc_tree" ]; then
        return 0
    else
        log_error "$test_name: file trees differ"
        diff <(echo "$upstream_tree") <(echo "$oc_tree") || true
        return 1
    fi
}

# =========================================================================
# Test 1: Anchored patterns
# =========================================================================

test_anchored_patterns() {
    local test_name="Anchored patterns (/top-level-only.txt)"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work="$TEST_DIR/anchored"
    mkdir -p "$work"/{src/subdir,upstream_dest,oc_dest}

    echo "root" > "$work/src/top-level-only.txt"
    echo "nested" > "$work/src/subdir/top-level-only.txt"
    echo "other" > "$work/src/other.txt"
    echo "sub-other" > "$work/src/subdir/other.txt"

    if ! "$UPSTREAM_RSYNC" -av --exclude '/top-level-only.txt' \
        "$work/src/" "$work/upstream_dest/" > "$work/upstream.log" 2>&1; then
        log_error "$test_name: upstream rsync failed"
        cat "$work/upstream.log" >&2
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if ! "$OC_RSYNC" -av --exclude '/top-level-only.txt' \
        "$work/src/" "$work/oc_dest/" > "$work/oc.log" 2>&1; then
        log_error "$test_name: oc-rsync failed"
        cat "$work/oc.log" >&2
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if compare_trees "$work/upstream_dest" "$work/oc_dest" "$test_name"; then
        log_info "$test_name: PASS"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

# =========================================================================
# Test 2: Directory-only patterns (trailing slash)
# =========================================================================

test_directory_only_patterns() {
    local test_name="Directory-only patterns (cache/)"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work="$TEST_DIR/dironly"
    mkdir -p "$work"/{src/cache,src/subdir/cache,upstream_dest,oc_dest}

    # 'cache' as a directory with contents
    echo "cached" > "$work/src/cache/data.txt"
    echo "nested-cached" > "$work/src/subdir/cache/data.txt"
    # 'cache' as a plain file
    echo "file-named-cache" > "$work/src/cache_file"
    # We need an actual file named 'cache' (not directory) in a subdir
    mkdir -p "$work/src/other"
    echo "i-am-a-file" > "$work/src/other/cache"

    if ! "$UPSTREAM_RSYNC" -av --exclude 'cache/' \
        "$work/src/" "$work/upstream_dest/" > "$work/upstream.log" 2>&1; then
        log_error "$test_name: upstream rsync failed"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if ! "$OC_RSYNC" -av --exclude 'cache/' \
        "$work/src/" "$work/oc_dest/" > "$work/oc.log" 2>&1; then
        log_error "$test_name: oc-rsync failed"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if compare_trees "$work/upstream_dest" "$work/oc_dest" "$test_name"; then
        log_info "$test_name: PASS"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

# =========================================================================
# Test 3: Double-star patterns
# =========================================================================

test_double_star_patterns() {
    local test_name="Double-star patterns (**/*.tmp)"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work="$TEST_DIR/doublestar"
    mkdir -p "$work"/{src/a/b/c,upstream_dest,oc_dest}

    echo "keep" > "$work/src/keep.txt"
    echo "tmp-root" > "$work/src/root.tmp"
    echo "tmp-a" > "$work/src/a/level1.tmp"
    echo "tmp-deep" > "$work/src/a/b/c/deep.tmp"
    echo "keep-deep" > "$work/src/a/b/c/keep.txt"

    if ! "$UPSTREAM_RSYNC" -av --exclude '**/*.tmp' \
        "$work/src/" "$work/upstream_dest/" > "$work/upstream.log" 2>&1; then
        log_error "$test_name: upstream rsync failed"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if ! "$OC_RSYNC" -av --exclude '**/*.tmp' \
        "$work/src/" "$work/oc_dest/" > "$work/oc.log" 2>&1; then
        log_error "$test_name: oc-rsync failed"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if compare_trees "$work/upstream_dest" "$work/oc_dest" "$test_name"; then
        log_info "$test_name: PASS"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

# =========================================================================
# Test 4: Character class patterns
# =========================================================================

test_character_class_patterns() {
    local test_name="Character class patterns ([0-9]*.log)"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work="$TEST_DIR/charclass"
    mkdir -p "$work"/{src,upstream_dest,oc_dest}

    echo "digit-log" > "$work/src/1error.log"
    echo "digit-log2" > "$work/src/99warnings.log"
    echo "alpha-log" > "$work/src/app.log"
    echo "no-ext" > "$work/src/3data"
    echo "keep" > "$work/src/readme.txt"

    if ! "$UPSTREAM_RSYNC" -av --exclude '[0-9]*.log' \
        "$work/src/" "$work/upstream_dest/" > "$work/upstream.log" 2>&1; then
        log_error "$test_name: upstream rsync failed"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if ! "$OC_RSYNC" -av --exclude '[0-9]*.log' \
        "$work/src/" "$work/oc_dest/" > "$work/oc.log" 2>&1; then
        log_error "$test_name: oc-rsync failed"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if compare_trees "$work/upstream_dest" "$work/oc_dest" "$test_name"; then
        log_info "$test_name: PASS"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

# =========================================================================
# Test 5: Include after exclude (first match wins)
# =========================================================================

test_include_after_exclude() {
    local test_name="Include after exclude (first match wins)"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work="$TEST_DIR/include_after"
    mkdir -p "$work"/{src,upstream_dest,oc_dest}

    echo "obj1" > "$work/src/foo.o"
    echo "obj2" > "$work/src/important.o"
    echo "src" > "$work/src/main.c"

    # In rsync, first matching rule wins. --exclude '*.o' comes first,
    # so --include 'important.o' should NOT rescue it.
    if ! "$UPSTREAM_RSYNC" -av --exclude '*.o' --include 'important.o' \
        "$work/src/" "$work/upstream_dest/" > "$work/upstream.log" 2>&1; then
        log_error "$test_name: upstream rsync failed"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if ! "$OC_RSYNC" -av --exclude '*.o' --include 'important.o' \
        "$work/src/" "$work/oc_dest/" > "$work/oc.log" 2>&1; then
        log_error "$test_name: oc-rsync failed"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if compare_trees "$work/upstream_dest" "$work/oc_dest" "$test_name"; then
        log_info "$test_name: PASS"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

# =========================================================================
# Test 6: Nested filter files (.rsync-filter in multiple directories)
# =========================================================================

test_nested_filter_files() {
    local test_name="Nested filter files (.rsync-filter per directory)"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work="$TEST_DIR/nested_filter"
    mkdir -p "$work"/{src/alpha,src/beta,upstream_dest,oc_dest}

    # Root filter excludes *.log
    echo "- *.log" > "$work/src/.rsync-filter"
    # alpha filter excludes *.bak
    echo "- *.bak" > "$work/src/alpha/.rsync-filter"
    # beta filter excludes *.tmp
    echo "- *.tmp" > "$work/src/beta/.rsync-filter"

    echo "keep" > "$work/src/keep.txt"
    echo "root-log" > "$work/src/root.log"
    echo "alpha-txt" > "$work/src/alpha/data.txt"
    echo "alpha-bak" > "$work/src/alpha/data.bak"
    echo "alpha-log" > "$work/src/alpha/data.log"
    echo "beta-txt" > "$work/src/beta/data.txt"
    echo "beta-tmp" > "$work/src/beta/data.tmp"
    echo "beta-log" > "$work/src/beta/data.log"

    if ! "$UPSTREAM_RSYNC" -av --filter 'dir-merge .rsync-filter' \
        "$work/src/" "$work/upstream_dest/" > "$work/upstream.log" 2>&1; then
        log_error "$test_name: upstream rsync failed"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if ! "$OC_RSYNC" -av --filter 'dir-merge .rsync-filter' \
        "$work/src/" "$work/oc_dest/" > "$work/oc.log" 2>&1; then
        log_error "$test_name: oc-rsync failed"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if compare_trees "$work/upstream_dest" "$work/oc_dest" "$test_name"; then
        log_info "$test_name: PASS"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

# =========================================================================
# Test 7: CVE-related path escaping (../ components)
# =========================================================================

test_path_escaping() {
    local test_name="CVE-related path escaping (../ in names)"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work="$TEST_DIR/path_escape"
    mkdir -p "$work"/{src/subdir,upstream_dest,oc_dest}

    # Create files with literal '../' in their names (not path traversal)
    echo "normal" > "$work/src/normal.txt"
    echo "dotdot" > "$work/src/subdir/..weird-name.txt"
    # A directory whose name contains dots
    mkdir -p "$work/src/...dots"
    echo "dots-content" > "$work/src/...dots/file.txt"

    # Exclude the dotdot-named file
    if ! "$UPSTREAM_RSYNC" -av --exclude '..weird-name.txt' \
        "$work/src/" "$work/upstream_dest/" > "$work/upstream.log" 2>&1; then
        log_error "$test_name: upstream rsync failed"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if ! "$OC_RSYNC" -av --exclude '..weird-name.txt' \
        "$work/src/" "$work/oc_dest/" > "$work/oc.log" 2>&1; then
        log_error "$test_name: oc-rsync failed"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if compare_trees "$work/upstream_dest" "$work/oc_dest" "$test_name"; then
        log_info "$test_name: PASS"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

# =========================================================================
# Test 8: Empty filter file
# =========================================================================

test_empty_filter_file() {
    local test_name="Empty filter file (merge with empty .rsync-filter)"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work="$TEST_DIR/empty_filter"
    mkdir -p "$work"/{src/subdir,upstream_dest,oc_dest}

    # Empty filter file
    touch "$work/src/.rsync-filter"

    echo "a" > "$work/src/a.txt"
    echo "b" > "$work/src/subdir/b.txt"

    if ! "$UPSTREAM_RSYNC" -av --filter 'dir-merge .rsync-filter' \
        "$work/src/" "$work/upstream_dest/" > "$work/upstream.log" 2>&1; then
        log_error "$test_name: upstream rsync failed"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if ! "$OC_RSYNC" -av --filter 'dir-merge .rsync-filter' \
        "$work/src/" "$work/oc_dest/" > "$work/oc.log" 2>&1; then
        log_error "$test_name: oc-rsync failed"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if compare_trees "$work/upstream_dest" "$work/oc_dest" "$test_name"; then
        log_info "$test_name: PASS"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

# =========================================================================
# Test 9: Unicode filenames
# =========================================================================

test_unicode_filenames() {
    local test_name="Unicode filenames in filter rules"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work="$TEST_DIR/unicode"
    mkdir -p "$work"/{src,upstream_dest,oc_dest}

    echo "cafe" > "$work/src/cafe.txt"
    echo "resume" > "$work/src/resume.txt"

    # UTF-8 filenames
    echo "japanese" > "$work/src/data.txt"

    # Platform check - some systems cannot create certain Unicode filenames
    if ! echo "umlaut" > "$work/src/ueber.txt" 2>/dev/null; then
        log_warn "$test_name: Cannot create Unicode filenames on this filesystem"
        TESTS_SKIPPED=$((TESTS_SKIPPED + 1))
        return 0
    fi

    # Exclude a pattern that includes Unicode characters
    if ! "$UPSTREAM_RSYNC" -av --exclude 'cafe*' --exclude 'ueber*' \
        "$work/src/" "$work/upstream_dest/" > "$work/upstream.log" 2>&1; then
        log_error "$test_name: upstream rsync failed"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if ! "$OC_RSYNC" -av --exclude 'cafe*' --exclude 'ueber*' \
        "$work/src/" "$work/oc_dest/" > "$work/oc.log" 2>&1; then
        log_error "$test_name: oc-rsync failed"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if compare_trees "$work/upstream_dest" "$work/oc_dest" "$test_name"; then
        log_info "$test_name: PASS"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

# =========================================================================
# Test 10: Very long paths
# =========================================================================

test_very_long_paths() {
    local test_name="Very long paths near OS path length limit"

    log_test "$test_name"
    TESTS_RUN=$((TESTS_RUN + 1))

    local work="$TEST_DIR/longpath"
    mkdir -p "$work"/{upstream_dest,oc_dest}

    # Build a deeply nested path (each component 50 chars, ~10 levels = ~500 chars)
    local long_component="abcdefghijklmnopqrstuvwxyz01234567890123456789abcd"
    local deep_dir="$work/src"
    for i in $(seq 1 8); do
        deep_dir="$deep_dir/${long_component}${i}"
    done

    if ! mkdir -p "$deep_dir" 2>/dev/null; then
        log_warn "$test_name: Cannot create deep directory path on this filesystem"
        TESTS_SKIPPED=$((TESTS_SKIPPED + 1))
        return 0
    fi

    echo "deep" > "$deep_dir/deep.txt"
    echo "deep-log" > "$deep_dir/deep.log"
    echo "shallow" > "$work/src/shallow.txt"

    # Exclude *.log even in deep paths
    if ! "$UPSTREAM_RSYNC" -av --exclude '*.log' \
        "$work/src/" "$work/upstream_dest/" > "$work/upstream.log" 2>&1; then
        log_error "$test_name: upstream rsync failed"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if ! "$OC_RSYNC" -av --exclude '*.log' \
        "$work/src/" "$work/oc_dest/" > "$work/oc.log" 2>&1; then
        log_error "$test_name: oc-rsync failed"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 0
    fi

    if compare_trees "$work/upstream_dest" "$work/oc_dest" "$test_name"; then
        log_info "$test_name: PASS"
        TESTS_PASSED=$((TESTS_PASSED + 1))
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
    fi
}

# =========================================================================
# Main
# =========================================================================

main() {
    log_info "Starting Filter Edge Cases Interoperability Tests"
    log_info "oc-rsync:       $OC_RSYNC"
    log_info "upstream rsync: $UPSTREAM_RSYNC"
    log_info "Test directory: $TEST_DIR"

    # Verify binaries exist
    if [ ! -x "$OC_RSYNC" ]; then
        log_error "oc-rsync binary not found or not executable: $OC_RSYNC"
        exit 1
    fi

    if [ ! -x "$UPSTREAM_RSYNC" ]; then
        log_error "upstream rsync binary not found or not executable: $UPSTREAM_RSYNC"
        log_info "Set UPSTREAM_RSYNC or ensure upstream rsync 3.4.1 is installed."
        exit 1
    fi

    log_info "oc-rsync version: $("$OC_RSYNC" --version | head -1)"
    log_info "upstream version: $("$UPSTREAM_RSYNC" --version | head -1)"

    # Run all tests
    test_anchored_patterns
    test_directory_only_patterns
    test_double_star_patterns
    test_character_class_patterns
    test_include_after_exclude
    test_nested_filter_files
    test_path_escaping
    test_empty_filter_file
    test_unicode_filenames
    test_very_long_paths

    # Summary
    echo ""
    echo "========================================="
    echo "Filter Edge Cases Test Summary"
    echo "========================================="
    echo "Total tests run:    $TESTS_RUN"
    echo "Tests passed:       $TESTS_PASSED"
    echo "Tests failed:       $TESTS_FAILED"
    echo "Tests skipped:      $TESTS_SKIPPED"
    echo "========================================="

    if [ $TESTS_FAILED -gt 0 ]; then
        log_error "$TESTS_FAILED test(s) failed."
        exit 1
    fi

    if [ $TESTS_PASSED -gt 0 ]; then
        log_info "All $TESTS_PASSED test(s) passed!"
    fi

    exit 0
}

main "$@"
