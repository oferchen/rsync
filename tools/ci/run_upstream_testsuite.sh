#!/usr/bin/env bash
# run_upstream_testsuite.sh - run upstream rsync's testsuite/ against oc-rsync.
#
# Mirrors the contract of upstream's runtests.sh:
#   - exports $RSYNC, $TOOLDIR, $srcdir, $suitedir, $scratchdir per test
#   - sources rsync.fns indirectly (each *.test sources it itself)
#   - exit codes from a test: 0=pass, 77=skip, 78=xfail, anything else=fail
#
# Differences vs upstream runtests.sh:
#   - $RSYNC is oc-rsync, not the upstream rsync binary
#   - we still need upstream's helper tools (tls, getgroups, lsh.sh) and
#     config.h/shconfig artifacts; those come from a one-time `./configure`
#     and partial `make` against the upstream source tree
#   - known failures are tracked in tools/ci/upstream_testsuite_known_failures.conf
#
# Usage:
#   tools/ci/run_upstream_testsuite.sh                # run all *.test
#   WHICHTESTS=00-hello.test tools/ci/...sh           # run a single test
#   UPSTREAM_VERSION=3.4.1 tools/ci/...sh             # pin upstream version
#   PRESERVE_SCRATCH=yes tools/ci/...sh               # keep per-test scratch dirs

set -euo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
upstream_version="${UPSTREAM_VERSION:-3.4.1}"
upstream_src_root="${workspace_root}/target/interop/upstream-src"
upstream_src_dir="${upstream_src_root}/rsync-${upstream_version}"
oc_rsync_bin="${OC_RSYNC_BIN:-${workspace_root}/target/release/oc-rsync}"
known_failures_conf="${workspace_root}/tools/ci/upstream_testsuite_known_failures.conf"
log_root="${workspace_root}/target/interop/upstream-testsuite"
testrun_timeout="${TESTRUN_TIMEOUT:-300}"

KNOWN_FAILURES=()
if [[ -f "$known_failures_conf" ]]; then
    # shellcheck source=/dev/null
    source "$known_failures_conf"
fi

is_known_failure() {
    local name=$1
    local kf
    for kf in "${KNOWN_FAILURES[@]}"; do
        [[ "$kf" == "$name" ]] && return 0
    done
    return 1
}

ensure_oc_rsync() {
    if [[ -x "$oc_rsync_bin" ]]; then
        return
    fi
    echo "==> Building oc-rsync (release)..." >&2
    (cd "$workspace_root" && cargo build --release --bin oc-rsync)
}

ensure_upstream_src() {
    if [[ -d "$upstream_src_dir" && -f "${upstream_src_dir}/configure" ]]; then
        return
    fi
    echo "==> Fetching upstream rsync ${upstream_version} source..." >&2
    mkdir -p "$upstream_src_root"
    local tarball="${upstream_src_root}/rsync-${upstream_version}.tar.gz"
    if [[ ! -f "$tarball" ]]; then
        curl -fsSL --connect-timeout 30 --max-time 300 \
            "https://download.samba.org/pub/rsync/src/rsync-${upstream_version}.tar.gz" \
            -o "$tarball"
    fi
    (cd "$upstream_src_root" && tar xzf "$tarball")
}

build_upstream_helpers() {
    if [[ -f "${upstream_src_dir}/shconfig" && \
          -x "${upstream_src_dir}/tls" && \
          -x "${upstream_src_dir}/getgroups" && \
          -x "${upstream_src_dir}/support/lsh.sh" ]]; then
        return
    fi
    echo "==> Configuring and building upstream helper tools..." >&2
    (
        cd "$upstream_src_dir"
        if [[ ! -f shconfig ]]; then
            ./configure --disable-debug --disable-md2man --disable-iconv \
                --disable-zstd --disable-lz4 >configure.log 2>&1 \
                || { tail -50 configure.log; exit 1; }
        fi
        # `make all` is the simplest way to get tls, getgroups, and the
        # upstream rsync binary built (the binary is not used as $RSYNC
        # but lsh.sh and a few tests reference $TOOLDIR/rsync).
        make all >make.log 2>&1 || { tail -100 make.log; exit 1; }
    )
}

setup_test_env() {
    cd "$upstream_src_dir"
    TOOLDIR="$upstream_src_dir"
    srcdir="$upstream_src_dir"
    suitedir="$upstream_src_dir/testsuite"
    RSYNC="$oc_rsync_bin"
    TLS_ARGS=''
    if grep -E '^#define HAVE_LUTIMES 1' "${upstream_src_dir}/config.h" >/dev/null 2>&1; then
        TLS_ARGS="$TLS_ARGS -l"
    fi
    if grep -E '#undef CHOWN_MODIFIES_SYMLINK' "${upstream_src_dir}/config.h" >/dev/null 2>&1; then
        TLS_ARGS="$TLS_ARGS -L"
    fi
    POSIXLY_CORRECT=1
    # Sourced from shconfig in upstream; for portability set defaults.
    : "${ECHO_N:=}"
    : "${ECHO_C:=\\c}"
    : "${ECHO_T:=}"
    if [[ -f "${upstream_src_dir}/shconfig" ]]; then
        # shellcheck source=/dev/null
        . "${upstream_src_dir}/shconfig"
    fi
    export TOOLDIR srcdir suitedir RSYNC TLS_ARGS POSIXLY_CORRECT \
        ECHO_N ECHO_C ECHO_T
}

prep_scratch() {
    local sd=$1
    [[ -d "$sd" ]] && chmod -R u+rwX "$sd" 2>/dev/null && rm -rf "$sd"
    mkdir -p "$sd"
    ln -sfn "$srcdir" "$sd/src"
}

run_one_test() {
    local testscript=$1
    local testbase log scratchdir result
    testbase=$(basename "$testscript" .test)
    scratchdir="${scratchbase}/${testbase}"
    log="${log_root}/${testbase}.log"
    export scratchdir

    prep_scratch "$scratchdir"

    set +e
    timeout "$testrun_timeout" bash -e "$testscript" >"$log" 2>&1
    result=$?
    set -e

    if [[ "${PRESERVE_SCRATCH:-no}" != "yes" && $result -eq 0 ]]; then
        rm -rf "$scratchdir"
    fi

    if is_known_failure "$testbase"; then
        if [[ $result -eq 0 ]]; then
            echo "UPASS   $testbase  (was expected to fail; remove from known_failures.conf)"
            unexpected_passes+=("$testbase")
            return 4
        fi
        echo "XFAIL   $testbase"
        return 3
    fi

    case $result in
        0)   echo "PASS    $testbase";                            return 0 ;;
        77)  echo "SKIP    $testbase";                            return 1 ;;
        78)  echo "XFAIL   $testbase  (test_xfail self-marked)";  return 3 ;;
        124) echo "FAIL    $testbase  (timed out after ${testrun_timeout}s)" ;;
        *)   echo "FAIL    $testbase  (exit $result)" ;;
    esac
    failed_tests+=("$testbase")
    return 2
}

summarize() {
    echo "------------------------------------------------------------"
    echo "  passed:   $passed"
    echo "  failed:   $failed"
    echo "  xfail:    $xfail"
    echo "  upass:    ${#unexpected_passes[@]}"
    echo "  skipped:  $skipped"
    if (( ${#failed_tests[@]} )); then
        echo "  failures:"
        local t
        for t in "${failed_tests[@]}"; do
            echo "    - $t (log: ${log_root}/${t}.log)"
        done
    fi
    if (( ${#unexpected_passes[@]} )); then
        echo "  unexpected passes (remove from known_failures.conf):"
        local t
        for t in "${unexpected_passes[@]}"; do
            echo "    - $t"
        done
    fi
}

main() {
    ensure_oc_rsync
    ensure_upstream_src
    build_upstream_helpers
    setup_test_env

    rm -rf "$log_root"
    mkdir -p "$log_root"
    scratchbase="${log_root}/scratch"
    mkdir -p "$scratchbase"

    passed=0
    failed=0
    xfail=0
    skipped=0
    failed_tests=()
    unexpected_passes=()

    local pattern="${WHICHTESTS:-*.test}"
    local testscript
    for testscript in "$suitedir"/$pattern; do
        [[ -e "$testscript" ]] || continue
        local rc=0
        run_one_test "$testscript" || rc=$?
        case $rc in
            0) passed=$((passed+1)) ;;
            1) skipped=$((skipped+1)) ;;
            2) failed=$((failed+1)) ;;
            3) xfail=$((xfail+1)) ;;
            4) ;;  # unexpected pass; counted via array length
        esac
    done

    summarize

    if (( failed > 0 || ${#unexpected_passes[@]} > 0 )); then
        exit 1
    fi
}

main "$@"
