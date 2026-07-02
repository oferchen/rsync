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
#   UPSTREAM_VERSION=3.4.4 tools/ci/...sh             # pin upstream version
#   PRESERVE_SCRATCH=yes tools/ci/...sh               # keep per-test scratch dirs
#
# Git-ref mode (tracks the moving 3.5.0dev target - RsyncProject master):
#   UPSTREAM_VERSION=master tools/ci/...sh            # git-clone + build master
#   UPSTREAM_REF=<sha-or-tag> tools/ci/...sh          # any RsyncProject ref
#   UPSTREAM_GIT_URL=<url> UPSTREAM_REF=... tools/ci/...sh
#
# Git-ref mode is additive and OFF by default: the release-tarball path above
# is 100% unchanged unless UPSTREAM_REF is set or UPSTREAM_VERSION=master. In
# git-ref mode the upstream source is a git checkout, not a release tarball,
# and (since the 3.4.x -> 3.5.0dev migration) upstream's testsuite is Python
# (runtests.py + testsuite/*_test.py), not the shell *.test scripts of 3.4.x.
# We therefore delegate to upstream's own runtests.py with --rsync-bin set to
# oc-rsync - the master analog of pointing $RSYNC at oc-rsync in the tarball
# path - rather than driving *.test scripts ourselves. See run_git_ref_mode().

set -euo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
upstream_version="${UPSTREAM_VERSION:-3.4.4}"
# Git-ref mode is selected by an explicit UPSTREAM_REF, or by the sentinel
# UPSTREAM_VERSION=master. Default (empty UPSTREAM_REF, numeric version) keeps
# the release-tarball path untouched.
upstream_git_url="${UPSTREAM_GIT_URL:-https://github.com/RsyncProject/rsync}"
upstream_ref="${UPSTREAM_REF:-}"
if [[ -z "$upstream_ref" && "$upstream_version" == "master" ]]; then
    upstream_ref="master"
fi
git_ref_mode="no"
[[ -n "$upstream_ref" ]] && git_ref_mode="yes"
upstream_src_root="${workspace_root}/target/interop/upstream-src"
if [[ "$git_ref_mode" == "yes" ]]; then
    # A ref may be a sha/tag/branch; sanitize it into a safe directory name.
    upstream_src_dir="${upstream_src_root}/rsync-git-${upstream_ref//[^A-Za-z0-9._-]/_}"
else
    upstream_src_dir="${upstream_src_root}/rsync-${upstream_version}"
fi
oc_rsync_bin="${OC_RSYNC_BIN:-${workspace_root}/target/release/oc-rsync}"
# Resolve to absolute path - test scripts cd into the upstream source tree,
# so a relative OC_RSYNC_BIN would break.
if [[ "$oc_rsync_bin" != /* ]]; then
    oc_rsync_bin="${workspace_root}/${oc_rsync_bin}"
fi
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

# Emit a GitHub Actions error annotation. No-op outside GHA so local runs
# stay quiet. The `::error` workflow command surfaces as a red marker on the
# PR/check page, giving a per-test failure indicator without forcing the
# reader to open the job log. See:
# https://docs.github.com/actions/using-workflows/workflow-commands-for-github-actions#setting-an-error-message
gha_annotate_fail() {
    [[ -z "${GITHUB_ACTIONS:-}" ]] && return 0
    local title=$1 message=$2
    # Annotations don't support multiline; collapse newlines to spaces and
    # strip the GHA control characters %, \r, \n that would otherwise break
    # the workflow command parser.
    local sanitized=${message//$'\n'/ }
    sanitized=${sanitized//$'\r'/ }
    sanitized=${sanitized//%/%25}
    printf '::error file=tools/ci/run_upstream_testsuite.sh,title=%s::%s\n' \
        "$title" "$sanitized"
}

ensure_oc_rsync() {
    if [[ -x "$oc_rsync_bin" ]]; then
        return
    fi
    echo "==> Building oc-rsync (release)..." >&2
    (cd "$workspace_root" && cargo build --locked --release --bin oc-rsync)
}

ensure_upstream_src() {
    if [[ -d "$upstream_src_dir" && -f "${upstream_src_dir}/configure" ]]; then
        return
    fi
    if [[ "$git_ref_mode" == "yes" ]]; then
        # Git-ref mode: clone a RsyncProject ref instead of a release tarball.
        # A fresh checkout ships a stub ./configure that bootstraps
        # configure.sh via prepare-source (needs autoconf/autoheader), so the
        # downstream build_upstream_helpers() path is unchanged.
        echo "==> Cloning ${upstream_git_url} @ ${upstream_ref} ..." >&2
        mkdir -p "$upstream_src_root"
        rm -rf "$upstream_src_dir"
        if git ls-remote --exit-code "$upstream_git_url" "$upstream_ref" >/dev/null 2>&1; then
            git clone --depth 1 --branch "$upstream_ref" \
                "$upstream_git_url" "$upstream_src_dir"
        else
            # Ref is a commit sha (not a branch/tag): clone then fetch it.
            git clone "$upstream_git_url" "$upstream_src_dir"
            (cd "$upstream_src_dir" && git checkout --detach "$upstream_ref")
        fi
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
          -x "${upstream_src_dir}/getfsdev" && \
          -x "${upstream_src_dir}/trimslash" && \
          -x "${upstream_src_dir}/t_unsafe" && \
          -x "${upstream_src_dir}/wildtest" && \
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
        # Build the upstream rsync binary (some tests reference
        # $TOOLDIR/rsync) plus all CHECK_PROGS helper programs that the
        # testsuite scripts require. These are not part of the `all`
        # target so they must be named explicitly:
        #   tls, getgroups    - used by rsync.fns (check_perms, rsync_getgroups)
        #   getfsdev          - used by chmod-temp-dir.test (cross-filesystem detection)
        #   trimslash         - used by trimslash.test
        #   t_unsafe          - used by unsafe-byname.test
        #   t_chmod_secure    - used by chmod-symlink-race.test
        #   t_secure_relpath  - used by secure-relpath-validation.test
        #   wildtest          - used by wildmatch.test
        make all tls getgroups getfsdev trimslash t_unsafe t_chmod_secure \
            t_secure_relpath wildtest \
            >make.log 2>&1 || { tail -100 make.log; exit 1; }
    )
}

find_setfacl_nodef() {
    # upstream: runtests.sh:205-215 - detect the platform's command for
    # removing default ACLs from a directory.  The ACL tests rely on this
    # variable being exported into their environment.
    local probe_dir=$1
    if setacl -k u::7,g::5,o:5 "$probe_dir" 2>/dev/null; then
        echo 'setacl -k'
    elif setfacl --help 2>&1 | grep -E ' -k,|\[-[a-z]*k' >/dev/null 2>&1; then
        echo 'setfacl -k'
    elif setfacl -s u::7,g::5,o:5 "$probe_dir" 2>/dev/null; then
        echo 'setfacl -s u::7,g::5,o:5'
    else
        echo 'true'
    fi
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

# Backdate the mtime of the upstream source root so tests that reference the
# cwd's parent directory ("..") get a stable, old timestamp.
#
# The tests run with cwd = $upstream_src_dir, so ".." resolves to
# $upstream_src_root. delay-updates.test does
#   touch -r .. "$todir/foo"
# to age the destination file, then writes a fresh source file and expects the
# two mtimes to differ so the quick-check (same size + same mtime => skip)
# forces a transfer. On a cold CI run the tarball is extracted moments before
# the tests execute, so $upstream_src_root's mtime lands in the same wall-clock
# second as the freshly written source file. The mtimes then collide, the
# quick-check skips the transfer, the stale destination is left in place, and
# the test's dir/file diff fails. Warm-cache runs use an already-old source
# root and pass, which is exactly the observed intermittency. Pinning the mtime
# to a fixed epoch makes ".." deterministically old. Nothing writes directly
# into $upstream_src_root during a run (scratch lives under $log_root), so the
# stamp survives the whole test loop. Both oc-rsync and upstream rsync 3.4.4
# skip under the collision, so this is a harness-timing fix, not a behavioural
# divergence.
stabilize_srcroot_mtime() {
    touch -t 200001010000 "$upstream_src_root" 2>/dev/null || true
}

prep_scratch() {
    local sd=$1
    [[ -d "$sd" ]] && chmod -R u+rwX "$sd" 2>/dev/null && rm -rf "$sd"
    mkdir -p "$sd"
    # upstream: runtests.sh:254 - clear default ACLs and setgid to avoid
    # confusing tests that depend on inheritable permission state.
    $setfacl_nodef "$sd" 2>/dev/null || true
    chmod g-s "$sd" 2>/dev/null || true
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
            gha_annotate_fail "upstream testsuite UPASS" \
                "Test '$testbase' passed but is listed in known_failures.conf; remove it (log: ${log})"
            return 4
        fi
        echo "XFAIL   $testbase"
        # Surface WHY the known failure still fails so a CI-only divergence
        # (a test that passes on a dev host but fails on the runner) is
        # diagnosable from the job log without re-running locally. Dump the
        # tail of the captured test log, which holds the failing checkdiff.
        if [[ -s "$log" ]]; then
            echo "        --- last 40 lines of ${testbase}.log (XFAIL detail) ---"
            tail -n 40 "$log" | sed 's/^/        /'
            echo "        --- end ${testbase}.log ---"
        fi
        return 3
    fi

    case $result in
        0)   echo "PASS    $testbase";                            return 0 ;;
        77)  echo "SKIP    $testbase";                            return 1 ;;
        78)  echo "XFAIL   $testbase  (test_xfail self-marked)";  return 3 ;;
        124) echo "FAIL    $testbase  (timed out after ${testrun_timeout}s)"
             gha_annotate_fail "upstream testsuite FAIL" \
                 "Test '$testbase' timed out after ${testrun_timeout}s (log: ${log})" ;;
        *)   echo "FAIL    $testbase  (exit $result)"
             gha_annotate_fail "upstream testsuite FAIL" \
                 "Test '$testbase' FAILED with exit $result (log: ${log})" ;;
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

# Append a markdown summary of the run to $GITHUB_STEP_SUMMARY when set.
# This is GHA-only - outside CI the env var is unset and this is a no-op.
# The summary surfaces the per-test FAIL list at-a-glance on the job page,
# without requiring the reader to open the full job log.
emit_gha_step_summary() {
    local summary_file=${GITHUB_STEP_SUMMARY:-}
    [[ -z "$summary_file" ]] && return 0

    {
        echo "## Upstream testsuite (per-test results)"
        echo
        echo "| Result | Count |"
        echo "|--------|------:|"
        echo "| PASS   | $passed |"
        echo "| FAIL   | $failed |"
        echo "| XFAIL  | $xfail |"
        echo "| UPASS  | ${#unexpected_passes[@]} |"
        echo "| SKIP   | $skipped |"
        echo
        if (( ${#failed_tests[@]} )); then
            echo "### Failures"
            echo
            local t
            for t in "${failed_tests[@]}"; do
                echo "- \`$t\` (log: \`${log_root}/${t}.log\`)"
            done
            echo
        fi
        if (( ${#unexpected_passes[@]} )); then
            echo "### Unexpected passes (remove from known_failures.conf)"
            echo
            local t
            for t in "${unexpected_passes[@]}"; do
                echo "- \`$t\`"
            done
            echo
        fi
    } >>"$summary_file"
}

# Git-ref mode driver: delegate to upstream's own runtests.py.
#
# The 3.5.0dev testsuite is Python (runtests.py + testsuite/*_test.py), so we
# do NOT iterate *.test scripts as the tarball path does. Instead we build the
# upstream helper programs the suite needs (`make check-progs` == the `all`
# target plus CHECK_PROGS/CHECK_SYMLINKS, per upstream Makefile.in:381) and
# then invoke upstream's runtests.py with --rsync-bin pointed at oc-rsync. That
# flag is the master analog of exporting $RSYNC=oc-rsync in the tarball path.
#
# runtests.py prints one "PASS/FAIL/SKIP/XFAIL <testbase>" line per test and a
# trailing "overall result is N" (N = failure count). This is informational
# tracking of a moving target, so there is no known-failures gate: we surface
# every divergence (the FAIL/XFAIL set) and propagate runtests.py's own exit
# code, which the nightly workflow reports without blocking any PR.
run_git_ref_mode() {
    ensure_oc_rsync
    ensure_upstream_src

    echo "==> Building upstream (git ${upstream_ref}) + testsuite helpers..." >&2
    (
        cd "$upstream_src_dir"
        if [[ ! -f configure.sh ]]; then
            ./configure --disable-debug --disable-md2man --disable-iconv \
                --disable-zstd --disable-lz4 >configure.log 2>&1 \
                || { tail -80 configure.log; exit 1; }
        fi
        # check-progs builds `all` + CHECK_PROGS + CHECK_SYMLINKS: exactly the
        # tools runtests.py needs (upstream Makefile.in:381).
        make check-progs >make.log 2>&1 || { tail -120 make.log; exit 1; }
    )

    rm -rf "$log_root"
    mkdir -p "$log_root"
    local output_log="${log_root}/runtests-output.log"

    # Permission-safe scratch cleanup. A test that leaves a mode-0 directory
    # behind (e.g. xattrs/, recv-discard-nullderef/) makes a plain `rm -rf`
    # throw for the non-root runner, and runtests.py's per-test prep_scratch
    # PermissionErrors then cascade into every later test - inflating the FAIL
    # count with pure environment noise. Drive the scratch tree from a
    # dedicated, mode-tagged directory under $log_root (never a bind mount) and
    # force-clear it so a poisoned tree from a prior leg can never wedge this
    # one. `chmod -R u+rwX` restores owner traversal on any 0-mode dir before
    # the delete; both run under the current euid (root in the sudo leg, the
    # runner user otherwise), so the owner always holds the bit.
    local mode_tag="nonroot"
    [[ "${EUID:-$(id -u)}" -eq 0 ]] && mode_tag="root"
    # Build the runtests.py argv. Include the base program so the array is
    # never empty - portable across bash 3.2 (macOS), which errors on an
    # empty "${arr[@]}" expansion under `set -u`.
    local transport_tag="pipe"
    local -a runtests_argv
    runtests_argv=(python3 ./runtests.py)
    if [[ "${USE_TCP:-no}" == "yes" ]]; then
        # --use-tcp runs daemon/proxy tests against a real loopback rsyncd
        # (RSYNC_TEST_USE_TCP=1) instead of degrading/SKIPping under the secure
        # stdio-pipe default. Un-skips daemon-chroot-acl + proxy-response-line-
        # too-long. Binds 127.0.0.1:<high-port>, needs no privilege.
        transport_tag="tcp"
        runtests_argv+=(--use-tcp)
    fi
    runtests_argv+=(
        --rsync-bin="$oc_rsync_bin"
        --tooldir="$upstream_src_dir"
        --srcdir="$upstream_src_dir"
        --timeout="$testrun_timeout"
    )
    local scratch_home="${log_root}/scratch-${mode_tag}-${transport_tag}"
    chmod -R u+rwX "$scratch_home" 2>/dev/null || true
    rm -rf "$scratch_home"
    mkdir -p "$scratch_home"

    local rc=0
    (
        cd "$upstream_src_dir"
        # scratchbase -> runtests.py places $scratchbase/testtmp here, off the
        # source tree, so the cleanup above owns the whole scratch lifecycle.
        scratchbase="$scratch_home" "${runtests_argv[@]}"
    ) 2>&1 | tee "$output_log"
    rc=${PIPESTATUS[0]}

    # Force-clear the scratch tree again so the NEXT leg (or a re-run on the
    # same self-hosted runner) never inherits a mode-0 dir from this leg.
    chmod -R u+rwX "$scratch_home" 2>/dev/null || true
    rm -rf "$scratch_home" 2>/dev/null || true

    emit_git_ref_step_summary "$output_log" "$rc"
    return "$rc"
}

# Write a $GITHUB_STEP_SUMMARY table for a git-ref (runtests.py) run: PASS/FAIL/
# SKIP/XFAIL counts plus the FAIL/XFAIL test names. GHA-only; no-op locally.
emit_git_ref_step_summary() {
    local output_log=$1 rc=$2
    local summary_file=${GITHUB_STEP_SUMMARY:-}
    [[ -z "$summary_file" ]] && return 0
    [[ -f "$output_log" ]] || return 0

    local p f s x
    p=$(grep -c '^PASS ' "$output_log" || true)
    f=$(grep -c '^FAIL ' "$output_log" || true)
    s=$(grep -c '^SKIP ' "$output_log" || true)
    x=$(grep -c '^XFAIL ' "$output_log" || true)

    {
        echo "## 3.5.0dev testsuite (RsyncProject ${upstream_ref})"
        echo
        echo "Informational tracker of the moving upstream target."
        echo "runtests.py overall exit code: \`${rc}\`"
        echo
        echo "| Result | Count |"
        echo "|--------|------:|"
        echo "| PASS   | $p |"
        echo "| FAIL   | $f |"
        echo "| XFAIL  | $x |"
        echo "| SKIP   | $s |"
        echo
        local fails
        fails=$(grep -E '^FAIL ' "$output_log" | awk '{print $2}' || true)
        if [[ -n "$fails" ]]; then
            echo "### Failures (divergence set)"
            echo
            local t
            while IFS= read -r t; do
                [[ -n "$t" ]] && echo "- \`$t\`"
            done <<<"$fails"
            echo
        fi
        local xfails
        xfails=$(grep -E '^XFAIL ' "$output_log" | awk '{print $2}' || true)
        if [[ -n "$xfails" ]]; then
            echo "### Expected failures (XFAIL)"
            echo
            local t
            while IFS= read -r t; do
                [[ -n "$t" ]] && echo "- \`$t\`"
            done <<<"$xfails"
            echo
        fi
    } >>"$summary_file"
}

main() {
    if [[ "$git_ref_mode" == "yes" ]]; then
        run_git_ref_mode
        exit $?
    fi

    ensure_oc_rsync
    ensure_upstream_src
    build_upstream_helpers
    setup_test_env
    stabilize_srcroot_mtime

    rm -rf "$log_root"
    mkdir -p "$log_root"
    scratchbase="${log_root}/scratch"
    mkdir -p "$scratchbase"

    # upstream: runtests.sh:205-217 - detect and export setfacl_nodef so
    # ACL tests can clear default ACLs from directories.
    setfacl_nodef=$(find_setfacl_nodef "$scratchbase")
    export setfacl_nodef

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
    emit_gha_step_summary

    if (( failed > 0 || ${#unexpected_passes[@]} > 0 )); then
        exit 1
    fi
}

main "$@"
