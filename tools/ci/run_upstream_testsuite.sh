#!/usr/bin/env bash
# run_upstream_testsuite.sh - run upstream rsync's testsuite/ against oc-rsync.
#
# Mirrors the contract of upstream's runtests.py:
#   - exports $RSYNC, $TOOLDIR, $srcdir, $suitedir, $scratchdir per test
#   - sources rsync.fns indirectly (each *.test sources it itself)
#   - exit codes from a test: 0=pass, 77=skip, 78=xfail, anything else=fail
#
# Differences vs upstream runtests.py:
#   - $RSYNC is oc-rsync, not the upstream rsync binary
#   - we still need upstream's helper tools (tls, getgroups, lsh.sh) and
#     config.h/shconfig artifacts; those come from a one-time `./configure`
#     and partial `make` against the upstream source tree
#   - known failures are tracked in tools/ci/upstream_testsuite_known_failures.conf
#
# Usage:
#   tools/ci/run_upstream_testsuite.sh                # run the whole suite
#   WHICHTESTS=00-hello.test tools/ci/...sh           # run a single test
#   UPSTREAM_VERSION=3.4.4 tools/ci/...sh             # pin an older release
#   PRESERVE_SCRATCH=yes tools/ci/...sh               # keep per-test scratch dirs
#
# Python-suite options (see run_python_suite_mode):
#   UPSTREAM_PEER_BIN=<path>  # --rsync-bin2: peer rsync for the daemon side and
#                             # remote-shell --rsync-path. Pointing it at a real
#                             # upstream build is what makes the run a VERSION-
#                             # MIXING run; without it the suite is oc vs oc.
#   EXPECT_RESULT=<file>      # --expect-result: expected-outcome manifest.
#   EMIT_EXPECT_RESULT=<file> # write an EXPECT_RESULT manifest from this run,
#                             # so the ledger is generated, never hand-typed.
#   EXPECT_SKIPPED=<spec>     # --expect-skipped, e.g.
#                             # "@testsuite/skiplist/common.txt,@.../linux.txt"
#   USE_TCP=yes               # --use-tcp: run the daemon tests against a real
#                             # rsyncd bound to 127.0.0.1 instead of the secure
#                             # stdio-pipe default, which opens no socket.
#   DAEMON_TESTS_ONLY=yes     # --daemon-tests-only: run only the tests that can
#                             # reach the daemon transport. Upstream intends this
#                             # to pair with USE_TCP, after a full default-
#                             # transport run: the tests it drops never call
#                             # start_test_daemon(), so they cannot observe
#                             # --use-tcp and would just repeat themselves.
#
# Git-ref mode (any RsyncProject ref, e.g. a post-release dev branch):
#   UPSTREAM_VERSION=master tools/ci/...sh            # git-clone + build master
#   UPSTREAM_REF=<sha-or-tag> tools/ci/...sh          # any RsyncProject ref
#   UPSTREAM_GIT_URL=<url> UPSTREAM_REF=... tools/ci/...sh
#
# WHICH RUNNER: chosen from what the extracted tree ships, not from the version
# string. rsync moved its testsuite from shell `*.test` scripts to Python
# (runtests.py + testsuite/*_test.py) between 3.4.4 and 3.5.0, so one
# release-tarball path has to serve both. A tree shipping `testsuite/*_test.py`
# delegates to upstream's own runner (keeping the oracle upstream's, and
# unlocking --rsync-bin2 version mixing and --expect-result manifests); a tree
# with `*.test` takes the shell loop below. Neither can select zero tests
# silently: the shell loop refuses an empty match, and an empty manifest is
# refused too.
#
# ⚠ The `*_test.py` half of `python_suite_available` is LOAD-BEARING, and the
# `runtests.py` half alone would be WRONG. 3.4.4 ALSO ships a runtests.py - its
# own docstring says "Invokes test scripts from testsuite/", i.e. it is the
# older shell-script driver, same filename and a different contract. Measured:
# 3.4.4 = runtests.py + 57 `*.test` + 0 `*_test.py`; 3.5.0 = runtests.py + 0
# `*.test` + 345 `*_test.py`. So do NOT "simplify" the predicate to a bare
# `[[ -f runtests.py ]]`: that routes 3.4.4 into the Python runner, which
# cannot drive it.

set -euo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
# Default to the current upstream stable release. Every CI caller passes
# UPSTREAM_VERSION explicitly, so this default is what a bare local invocation
# gets - and 3.4.4 silently routed those runs into the 57-cell `*.test` shell
# suite instead of the 345-cell Python suite the committed expect-manifests are
# written against, which reads as a passing run over a fifth of the coverage.
upstream_version="${UPSTREAM_VERSION:-3.5.0}"
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
    upstream_label="git ${upstream_ref}"
else
    upstream_src_dir="${upstream_src_root}/rsync-${upstream_version}"
    upstream_label="release ${upstream_version}"
fi
# Echo $1 unchanged when it is empty or already absolute, otherwise resolved
# against the workspace root.
#
# EVERY operator-supplied path must go through this before it reaches
# runtests.py. The suite is invoked from inside the extracted upstream tree
# (`cd "$upstream_src_dir"` below) and the individual test scripts cd further
# still, so a relative path handed across that boundary resolves against the
# wrong directory. MEASURED: `--expect-result tools/ci/upstream-3.5.0-expect.
# root.txt` reached runtests.py verbatim and died with FileNotFoundError -
# after this script had already stat'd the very same file successfully from
# the workspace root, which is why the validation block above it could not
# catch the problem it was standing next to.
absolutize_under_workspace() {
    case "$1" in
        '' | /*) printf '%s' "$1" ;;
        *) printf '%s/%s' "$workspace_root" "$1" ;;
    esac
}

# Optional peer binary for the Python suite's --rsync-bin2: the rsync used for
# the daemon side and for remote-shell --rsync-path. Setting it to a real
# upstream build is what turns the run into a version-MIXING run (oc on one end
# of the wire, upstream on the other) rather than oc-against-oc.
upstream_peer_bin="$(absolutize_under_workspace "${UPSTREAM_PEER_BIN:-}")"
# Expected-outcome manifest (runtests.py --expect-result). When set, ONLY the
# listed tests run and every outcome must match, an unexpected PASS included.
expect_result_file="$(absolutize_under_workspace "${EXPECT_RESULT:-}")"
# Composable expected-skip spec (runtests.py --expect-skipped), e.g.
# "@testsuite/skiplist/common.txt,@testsuite/skiplist/linux.txt". Deliberately
# NOT absolutized: its entries are `@`-prefixed and are documented relative to
# the upstream tree, which is the cwd runtests.py already runs in.
expect_skipped_spec="${EXPECT_SKIPPED:-}"
oc_rsync_bin="$(absolutize_under_workspace \
    "${OC_RSYNC_BIN:-${workspace_root}/target/release/oc-rsync}")"
known_failures_conf="${workspace_root}/tools/ci/upstream_testsuite_known_failures.conf"
log_root="${workspace_root}/target/interop/upstream-testsuite"

# Identity for THIS invocation. Every artefact this script writes OUTSIDE the
# workspace is named from it, so two concurrent runs on one host cannot clobber
# each other. $$ is unique among live processes; the epoch suffix keeps a stale
# directory from a dead run with a recycled pid from being mistaken for ours.
#
# WHY the parent computes this rather than the publishing helper:
# publish_oc_rsync_bin() is called in a `$(...)` command substitution, so it runs
# in a SUBSHELL and any variable it assigns is discarded before the caller can
# read it. Deriving both the publish path and the cleanup path from an id the
# parent already holds keeps the two in agreement without threading state out of
# a subshell.
uts_run_id="$$-$(date +%s)"
# Install dirs searched for a world-traversable home for the published binary,
# in preference order. Shared by publish_oc_rsync_bin() and its cleanup so the
# two can never disagree about where to look.
published_bin_dirs=(/usr/local/bin /usr/bin)
published_bin_prefix="oc-rsync-uts."
testrun_timeout="${TESTRUN_TIMEOUT:-300}"

# State for the xattr-capable loop-mounted scratch filesystem (see
# setup_scratch_fs). Empty until a loop mount succeeds; the EXIT trap reads
# these to unmount and delete the image.
xattr_fs_image=""
xattr_fs_mount=""

KNOWN_FAILURES=()
if [[ -f "$known_failures_conf" ]]; then
    # shellcheck source=/dev/null
    source "$known_failures_conf"
fi

is_known_failure() {
    local name=$1
    local kf
    # `${arr[@]+"${arr[@]}"}` because bash 3.2 (the macOS system bash) treats a
    # plain "${arr[@]}" on an EMPTY array as an unbound variable under `set -u`.
    # The ledger is empty today, so on macOS every test aborted here, was
    # miscounted as a skip, and the run still exited 0 - reporting success
    # having executed nothing. CI is bash 5 and unaffected, which is exactly
    # why this stayed invisible.
    for kf in ${KNOWN_FAILURES[@]+"${KNOWN_FAILURES[@]}"}; do
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
    # Deliberately NOT guarded on a hand-listed set of built binaries: such a
    # guard returns early on a tree that has the old helpers, so a newly
    # required one is never built. `make check-progs` is idempotent and cheap
    # when everything is current, so it is safe to run unconditionally.
    echo "==> Configuring and building upstream helper tools..." >&2
    (
        cd "$upstream_src_dir"
        if [[ ! -f shconfig ]]; then
            ./configure --disable-debug --disable-md2man --disable-iconv \
                --disable-zstd --disable-lz4 >configure.log 2>&1 \
                || { tail -50 configure.log; exit 1; }
        fi
        # Build the upstream rsync binary (some tests reference $TOOLDIR/rsync)
        # plus every helper program the testsuite requires, via upstream's OWN
        # target rather than a hand-copied list. `check-progs` expands to
        # `all $(CHECK_PROGS) $(CHECK_COMPILE_OBJS) $(CHECK_SYMLINKS)`, so the
        # set stays correct as upstream adds helpers.
        #
        # A hand-maintained list is what this replaces, and it had already
        # drifted: 3.5.0's CHECK_PROGS names 17 programs, the list named 8.
        # Missing t_rename_secure and t_symlink_secure made
        # rename-mixed-parent-symlink-race and
        # symlink-mknod-fakesuper-symlink-race fail for want of a binary -
        # neither test invokes oc-rsync at all - and t_acl, t_iwildmatch,
        # t_clean_fname, t_safe_arg, t_hashtable_overflow, testrun and simdtest
        # were absent too. A missing helper reads as an oc defect, so the copy
        # must not exist.
        #
        # upstream: rsync-3.5.0/Makefile.in:60-62 CHECK_PROGS, :440 check-progs
        make check-progs >make.log 2>&1 || { tail -100 make.log; exit 1; }
    )
}

find_setfacl_nodef() {
    # upstream: runtests.py find_setfacl_nodef() (3.5.0 runtests.py:146-161,
    # 3.4.4 runtests.py:67-82) - detect the platform's command for removing
    # default ACLs from a directory.  The ACL tests rely on this variable being
    # exported into their environment.  Anchored on the function name because
    # the line offsets move between releases.
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
    # upstream: runtests.py prep_scratch() (3.5.0 runtests.py:230-241) - clear
    # default ACLs and setgid to avoid confusing tests that depend on
    # inheritable permission state.
    $setfacl_nodef "$sd" 2>/dev/null || true
    chmod g-s "$sd" 2>/dev/null || true
    ln -sfn "$srcdir" "$sd/src"
}

# Run a command with root privilege: directly when already root, else via
# passwordless sudo. Returns non-zero (without prompting) when neither is
# available, so callers can fall back cleanly.
priv() {
    if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
        "$@"
    elif command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
        sudo -n "$@"
    else
        return 1
    fi
}

# True (0) iff $mp is a mount point. Prefers mountpoint(1); falls back to
# /proc/mounts so the check works even where util-linux is trimmed.
is_mounted() {
    local mp=$1
    if command -v mountpoint >/dev/null 2>&1; then
        mountpoint -q "$mp" 2>/dev/null
        return
    fi
    grep -qF " $mp " /proc/mounts 2>/dev/null
}

# True (0) iff $dir's filesystem honours user.* extended attributes. Probes by
# actually setting one on a throwaway file, since a mount can advertise support
# yet reject it (overlay/tmpfs/some CI runners).
fs_supports_user_xattr() {
    local dir=$1 probe rc=1
    [[ -d "$dir" && -w "$dir" ]] || return 1
    probe=$(mktemp "${dir}/.xattr-probe.XXXXXX" 2>/dev/null) || return 1
    if setfattr -n user.ocprobe -v 1 "$probe" 2>/dev/null; then
        rc=0
    fi
    rm -f "$probe" 2>/dev/null || true
    return $rc
}

# Set $scratchbase to a directory backed by a filesystem that supports user.*
# xattrs. xattrs.test (and hlink-xattrs) probe user.* support and self-SKIP
# when it is missing, so on a runner whose workspace filesystem rejects user.*
# xattrs their coverage is silently lost. We loop-mount a small ext4 image
# (ext4 enables user_xattr by default) and host the scratch tree there.
#
# Works for both legs: the harness runs as root on the sudo leg (priv() runs
# mount directly) and as the unprivileged runner on the non-root leg (priv()
# uses passwordless sudo, then chowns the mount so the unprivileged suite can
# write to it).
#
# Falls back to the given base (current behaviour) when a loop mount cannot be
# built. The fallback is always logged - never a silent skip - and warns
# explicitly when the fallback filesystem also lacks user.* xattr support.
setup_scratch_fs() {
    local default_base=$1
    scratchbase="$default_base"

    local img="${log_root}/xattr-scratch.img"
    local mnt="${log_root}/xattr-scratch"

    if ! command -v mkfs.ext4 >/dev/null 2>&1; then
        scratch_fs_fallback "$default_base" "mkfs.ext4 not found"
        return 0
    fi
    if ! priv true 2>/dev/null; then
        scratch_fs_fallback "$default_base" "no root/passwordless-sudo for loop mount"
        return 0
    fi

    mkdir -p "$mnt"
    rm -f "$img"
    # 1 GiB is ample for the whole suite's scratch trees (passing tests are
    # cleaned immediately; only preserved failures accumulate).
    if ! { fallocate -l 1024M "$img" 2>/dev/null || \
           dd if=/dev/zero of="$img" bs=1M count=1024 status=none 2>/dev/null; }; then
        scratch_fs_fallback "$default_base" "could not allocate loop image"
        rm -f "$img"
        return 0
    fi
    # -O ^has_journal keeps the throwaway image small and fast; user_xattr is
    # an ext4 default but we mount it explicitly for clarity.
    if ! mkfs.ext4 -q -F -O ^has_journal "$img" >/dev/null 2>&1; then
        scratch_fs_fallback "$default_base" "mkfs.ext4 failed"
        rm -f "$img"
        return 0
    fi
    if ! priv mount -o loop,user_xattr "$img" "$mnt" 2>/dev/null; then
        scratch_fs_fallback "$default_base" "loop mount failed"
        rm -f "$img"
        return 0
    fi
    # Hand ownership to the current euid so the suite (unprivileged on the
    # non-root leg) can create its per-test scratch trees.
    priv chown "$(id -u):$(id -g)" "$mnt" 2>/dev/null || true
    priv chmod 0755 "$mnt" 2>/dev/null || true

    if ! fs_supports_user_xattr "$mnt"; then
        scratch_fs_fallback "$default_base" "mounted ext4 rejected user.* xattr"
        priv umount "$mnt" 2>/dev/null || priv umount -l "$mnt" 2>/dev/null || true
        rm -f "$img"
        return 0
    fi

    xattr_fs_image="$img"
    xattr_fs_mount="$mnt"
    scratchbase="${mnt}/scratch"
    mkdir -p "$scratchbase"
    echo "==> xattr-capable scratch fs: loop-ext4 at ${mnt} (user_xattr verified)" >&2
    return 0
}

# Fall back to the workspace scratch base, logging why. Warns loudly when that
# filesystem cannot set user.* xattrs, so xattrs.test's skip is never silent.
scratch_fs_fallback() {
    local default_base=$1 reason=$2
    scratchbase="$default_base"
    mkdir -p "$scratchbase"
    if fs_supports_user_xattr "$scratchbase"; then
        echo "==> loop-ext4 scratch unavailable (${reason}); native FS supports user.* xattrs, using ${scratchbase}" >&2
    else
        echo "==> WARNING: loop-ext4 scratch unavailable (${reason}) and native FS lacks user.* xattr support; xattrs.test will SKIP" >&2
    fi
}

# Unmount and delete the loop-ext4 scratch image. Idempotent; safe to call from
# the EXIT trap and again at the top of a re-run.
cleanup_scratch_fs() {
    [[ -n "$xattr_fs_mount" ]] || return 0
    # Restore owner traversal so cleanup can descend any mode-0 dir a test left.
    priv chmod -R u+rwX "$xattr_fs_mount" 2>/dev/null || true
    if is_mounted "$xattr_fs_mount"; then
        priv umount "$xattr_fs_mount" 2>/dev/null || \
            priv umount -l "$xattr_fs_mount" 2>/dev/null || true
    fi
    rmdir "$xattr_fs_mount" 2>/dev/null || true
    rm -f "$xattr_fs_image" 2>/dev/null || true
    xattr_fs_mount=""
    xattr_fs_image=""
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

# True (0) iff every component of the absolute path $1 has its o+x bit set, so
# a CAP_DAC_OVERRIDE-dropped root or a dropped uid can traverse it. Unknown
# (stat unavailable) counts as not-traversable - we only claim traversable when
# we can prove it.
path_world_traversable() {
    local target=$1 p="" comp mode
    local -a parts
    IFS='/' read -r -a parts <<<"${target#/}"
    for comp in "${parts[@]}"; do
        [[ -z "$comp" ]] && continue
        p="${p}/${comp}"
        mode=$(stat -c '%a' "$p" 2>/dev/null || stat -f '%Lp' "$p" 2>/dev/null || echo "")
        [[ -n "$mode" ]] || return 1
        (( (0"$mode" & 1) != 0 )) || return 1
    done
    return 0
}

# Publish the oc-rsync binary to a world-traversable path and echo that path.
#
# WHY (root leg, setpriv): the 3.5.0dev fake-super/uid tests run rsync via
# setpriv with CAP_DAC_OVERRIDE dropped (partial_nowrite_test.py:65
# "setpriv --inh-caps -all --bounding-set -all"). The default binary lives at
# $GITHUB_WORKSPACE/target/release/oc-rsync, i.e. under /home/runner, which is
# mode 0750 and owned by the runner user. Without CAP_DAC_OVERRIDE even root
# cannot TRAVERSE /home/runner, so execve() of that path returns ENOENT and
# setpriv prints "failed to execute .../oc-rsync: No such file or directory".
# The Python harness then throws FileNotFoundError on the test's from-dir and
# the failure cascades. The test's own mount-namespace remount only covers the
# cwd (the upstream source tree), not target/release, so the binary stays
# unreachable. Copying it to a path whose every component is o+x (e.g.
# /usr/local/bin, all 0755 on the runner) removes the traversal barrier for
# both the cap-dropped root leg and any dropped-uid exec. Prefer copying the
# binary OUT of the runner HOME over chmod'ing HOME itself.
#
# Falls back to the original path when no world-traversable install dir is
# writable (local dev), so non-CI runs are unchanged.
publish_oc_rsync_bin() {
    local src=$1
    local dir
    for dir in "${published_bin_dirs[@]}"; do
        [[ -d "$dir" && -w "$dir" ]] || continue
        path_world_traversable "$dir" || continue
        # A per-RUN directory, not a fixed filename: two concurrent runs on one
        # host would otherwise publish over each other and each would test the
        # other's binary. The directory is 0755 so it stays traversable for the
        # cap-dropped root leg, and the binary keeps the basename `oc-rsync`
        # because argv[0] is load-bearing for mode dispatch.
        local run_dir="${dir}/${published_bin_prefix}${uts_run_id}"
        local dst="${run_dir}/oc-rsync"
        if mkdir -p "$run_dir" 2>/dev/null \
            && chmod 0755 "$run_dir" 2>/dev/null \
            && cp -f "$src" "$dst" 2>/dev/null \
            && chmod 0755 "$dst" 2>/dev/null; then
            echo "$dst"
            return 0
        fi
        # Partial failure leaves the directory for cleanup_published_bin(),
        # which resolves the same deterministic path.
    done
    # No writable world-traversable dir: keep the original path.
    echo "$src"
    return 0
}

# Remove this run's published-binary directory from every candidate dir.
#
# Guarded on a non-empty run id and built from a fixed prefix, so the path can
# never widen to the install dir itself. Runs from the EXIT trap, so it also
# collects the partial-failure case above.
cleanup_published_bin() {
    [[ -n "${uts_run_id:-}" ]] || return 0
    local dir run_dir
    for dir in "${published_bin_dirs[@]}"; do
        run_dir="${dir}/${published_bin_prefix}${uts_run_id}"
        [[ -d "$run_dir" ]] || continue
        rm -rf -- "$run_dir"
    done
}

# Echo a world-traversable base dir to host the runtests.py scratch tree, or
# the given fallback when none is usable.
#
# WHY (root leg, mount namespace): partial_nowrite_test.py, when running as
# root on Linux, unshares a mount namespace and mounts a fresh tmpfs OVER the
# first non-root, non-world-x parent of cwd (chown_target). On the runner that
# parent is /home/runner, so the tmpfs SHADOWS everything beneath it - including
# a scratch tree under target/interop (which lives under /home/runner). The
# test's from-dir then vanishes inside the namespace and rsync fails link_stat
# with ENOENT, a pure harness artifact. Hosting the scratch OUTSIDE the shadowed
# parent (e.g. /tmp, mode 1777, all components world-x, never chown_target since
# it is world-x) keeps the from/to/chk dirs visible after the tmpfs mount.
world_traversable_scratch_base() {
    local fallback=$1
    local base
    for base in "${TMPDIR:-}" /tmp; do
        [[ -n "$base" && -d "$base" && -w "$base" ]] || continue
        path_world_traversable "$base" || continue
        echo "$base"
        return 0
    done
    echo "$fallback"
    return 0
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
# Does the extracted upstream tree ship the Python testsuite?
#
# This is a question about the TREE, not about the version string or how it was
# obtained. rsync migrated the suite from shell `*.test` scripts to Python
# (`runtests.py` + `testsuite/*_test.py`) between 3.4.4 and 3.5.0, so the same
# release-tarball path now has to drive two different runners. Dispatching on
# shape means a future release needs no version list edited here, and a tree
# that ships neither is caught by the zero-test guard rather than passing
# vacuously.
# Echo how many tests THIS invocation will select - the size an --expect-result
# manifest has to cover. Normally every *_test.py the tree ships; under
# --daemon-tests-only, the daemon-transport subset.
#
# That subset is obtained by importing upstream's OWN select_daemon_tests()
# rather than re-deriving its token list here. The list is deliberately
# over-broad ("a false positive only costs runtime, while a false negative would
# silently drop real coverage") and grows with the suite, so a second copy would
# drift - and a drifted count makes the guard below accept a manifest that
# covers less than the run, which is the one thing it exists to prevent. Fails
# loud rather than falling back: a silent 0 would disable the guard outright.
# upstream: runtests.py select_daemon_tests() (3.5.0 runtests.py:311-327)
selected_test_count() {
    if [[ "${DAEMON_TESTS_ONLY:-no}" != "yes" ]]; then
        find "${upstream_src_dir}/testsuite" -maxdepth 1 -name '*_test.py' \
            | wc -l | tr -d ' '
        return 0
    fi
    local count
    if ! count=$(cd "$upstream_src_dir" && python3 - <<'PY'
import glob, importlib.util, sys

spec = importlib.util.spec_from_file_location('upstream_runtests', 'runtests.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
keep, _dropped = module.select_daemon_tests(sorted(glob.glob('testsuite/*_test.py')))
if not keep:
    sys.exit('select_daemon_tests() selected no tests')
print(len(keep))
PY
    ); then
        echo "ERROR: could not compute the daemon-transport test count from" >&2
        echo "       ${upstream_src_dir}/runtests.py select_daemon_tests()." >&2
        exit 1
    fi
    echo "$count"
}

python_suite_available() {
    [[ -f "${upstream_src_dir}/runtests.py" ]] || return 1
    compgen -G "${upstream_src_dir}/testsuite/*_test.py" >/dev/null 2>&1
}

# --------------------------------------------------------------------------
# Legacy-behaviour oracles (upstream's old_versions/ archive).
#
# The 3.5.0 suite ships tests that pin a behaviour against a REAL old rsync
# when one is on disk and degrade when it is not. MEASURED over the 345 tests
# of the 3.5.0 tarball, three consult the archive from CODE (the rest only
# mention it in a docstring), and they degrade three different ways:
#
#   daemon-symlink-escape-matrix  old_versions/rsync_3.2.7  root + tcp
#       100 of its 200 cells (`insecure links = yes`) take their expected
#       value from a live 3.2.7 daemon when the binary is there and from
#       static_followed(), a hand-written prediction of 3.2.7, when it is not:
#           want, src = attempt(url_oracle, ...)[0], '327'
#           want, src = static_followed(...),        'contract'
#       SILENT: which one ran is printed only on the test's own stdout, and
#       runtests.py shows that just when a test does NOT pass (3.5.0
#       runtests.py:621 `show_log = always_log or (result not in (Exit.PASS,
#       Exit.SKIP, Exit.XFAIL))`). A green leg therefore cannot be read to say
#       which contract it enforced.
#
#   daemon-auth-digest-floor      old_versions/rsync_3.1.3  any leg
#       SILENT in the same way: `raise SystemExit(0)` drops the md5-downgrade
#       case and the test still reports PASS.
#
#   daemon-max-alloc-zero         old_versions/rsync_3.2.7  any leg
#       COUNTED: `test_skipped(f"{OLD_CLIENT} not present")`, which lands in
#       the run's skip column and in the expect manifests. This is the outcome
#       the other two should have had.
#
# The absence is not an edge case, it is every run: upstream ships
# old_versions/README.md and old_versions/build_static.sh but no binaries -
# MEASURED on a pristine 3.5.0 extraction, that directory holds exactly those
# two files - and build_static.sh wants a local rsync git worktree
# (RSYNC_REPO, default ../rsync.4) that CI does not have.
#
# LEGACY_ORACLES=on builds what the reachable consumers ask for, so those cells
# assert against the release instead of a prediction of it. Off, the same
# enumeration still runs and every degraded consumer is NAMED, because the one
# thing worse than a weak assertion is a weak assertion nobody can see.
#
# Off by DEFAULT, and per leg: putting a binary in old_versions/ un-skips
# daemon-max-alloc-zero, i.e. it MOVES expect-manifest rows. Those rows may
# only be re-baselined from a measured run (EMIT_EXPECT_RESULT), so a leg opts
# in when someone is ready to measure the move - never as a side effect.
# --------------------------------------------------------------------------

# on: build every oracle a consumer on this leg can reach. off: build none and
# report which consumers are running degraded.
legacy_oracles_mode="${LEGACY_ORACLES:-off}"
case "$legacy_oracles_mode" in
    on | off) ;;
    *)
        echo "ERROR: LEGACY_ORACLES must be 'on' or 'off', got '${legacy_oracles_mode}'." >&2
        exit 1
        ;;
esac

# Emit a GitHub Actions warning annotation. Same shape as gha_annotate_fail,
# and a no-op outside GHA. Used where a degradation must be visible without
# failing the leg.
gha_annotate_warn() {
    [[ -z "${GITHUB_ACTIONS:-}" ]] && return 0
    local title=$1 message=$2
    local sanitized=${message//$'\n'/ }
    sanitized=${sanitized//$'\r'/ }
    sanitized=${sanitized//%/%25}
    printf '::warning file=tools/ci/run_upstream_testsuite.sh,title=%s::%s\n' \
        "$title" "$sanitized"
}

# One TSV row per (oracle version, consuming test): version, test base name,
# whether that test needs the TCP transport, whether it needs root.
#
# DISCOVERED from the tests, never listed here. A second copy of "3.2.7" in
# this file would drift the moment upstream retargets the oracle, and the drift
# is exactly the invisible kind: the consumer would degrade to its fallback and
# keep passing. Reading the version out of the test that stats it makes the two
# unable to disagree. Same for the preconditions - `require_tcp()` and the
# `os.geteuid()` gate are the test's own, so a future consumer with different
# ones is classified by ITS source, not by this leg's assumptions.
#
# The scan is over the AST, not the file text. MEASURED: a plain grep for
# `old_versions` + `rsync_<ver>` over the 3.5.0 testsuite returns 10 files, and
# 7 of them only say "Cross-version: expected identical against
# --rsync-bin=old_versions/rsync_3.2.7" in a DOCSTRING - they never stat it. A
# text scan would build oracles for tests that cannot consume them and, worse,
# would report those tests as degraded when they are not. Docstrings are
# excluded explicitly and comments never enter the AST, so what is left is the
# string literals real code evaluates.
legacy_oracle_requirements() {
    (cd "$upstream_src_dir" && python3 - <<'PY'
import ast
import glob
import os
import re
import sys

BINARY = re.compile(r'^rsync_(\d+\.\d+(?:\.\d+)?)$')

def code_string_literals(tree):
    """Every str constant the module evaluates, minus its docstrings."""
    docstrings = set()
    for node in ast.walk(tree):
        if not isinstance(node, (ast.Module, ast.ClassDef,
                                 ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        body = getattr(node, 'body', None)
        if (body and isinstance(body[0], ast.Expr)
                and isinstance(body[0].value, ast.Constant)
                and isinstance(body[0].value.value, str)):
            docstrings.add(id(body[0].value))
    return [n.value for n in ast.walk(tree)
            if isinstance(n, ast.Constant) and isinstance(n.value, str)
            and id(n) not in docstrings]

rows = []
for path in sorted(glob.glob(os.path.join('testsuite', '*_test.py'))):
    with open(path, encoding='utf-8') as fh:
        text = fh.read()
    if 'old_versions' not in text:
        continue
    try:
        tree = ast.parse(text, filename=path)
    except SyntaxError as exc:
        sys.exit('%s: cannot parse to find its legacy-oracle use: %s' % (path, exc))
    literals = code_string_literals(tree)
    if 'old_versions' not in literals:
        continue                      # docstring/comment mention only
    versions = sorted({m.group(1) for m in
                       (BINARY.match(lit) for lit in literals) if m})
    if not versions:
        # The test evaluates 'old_versions' but names no rsync_<version>
        # literal, so the binary it wants cannot be derived - e.g. it moved to
        # an f-string. Guessing would build the wrong oracle and the test would
        # degrade exactly as if none had been built, silently.
        sys.exit('%s uses old_versions/ but names no rsync_<version> literal; '
                 'the oracle it wants cannot be derived from its source' % path)
    needs_tcp = 'yes' if 'require_tcp(' in text else 'no'
    needs_root = 'yes' if 'geteuid()' in text else 'no'
    base = os.path.basename(path)[:-len('_test.py')]
    for version in versions:
        rows.append('\t'.join((version, base, needs_tcp, needs_root)))
print('\n'.join(rows))
PY
    )
}

# Put every legacy oracle a test on THIS leg can reach onto disk, or - with
# LEGACY_ORACLES=off - name the consumers that will run degraded.
#
# Scoped to reachable consumers: an oracle no selected test can consult buys
# nothing, and a build is not free. Each skip is announced with its reason, so
# "not built" is never confused with "built and unused".
ensure_legacy_oracles() {
    local requirements
    if ! requirements=$(legacy_oracle_requirements); then
        echo "ERROR: could not enumerate the testsuite's legacy-oracle needs." >&2
        echo "       Refusing to continue: an unreadable requirement list would" >&2
        echo "       build nothing and hand every oracle-backed cell to its" >&2
        echo "       fallback, which is the failure this enumeration exists to" >&2
        echo "       prevent." >&2
        exit 1
    fi
    if [[ -z "$requirements" ]]; then
        echo "==> Legacy oracles: no test in this tree consults old_versions/." >&2
        return 0
    fi

    local old_versions_dir="${upstream_src_dir}/old_versions"
    local oracle_workdir="${workspace_root}/target/interop/old-versions-build"
    local euid=${EUID:-$(id -u)}
    local built=0 unreachable=0 degraded=0
    local degraded_names=""
    local version test_name needs_tcp needs_root reason
    while IFS=$'\t' read -r version test_name needs_tcp needs_root; do
        [[ -n "$version" ]] || continue
        reason=""
        if [[ "$needs_tcp" == "yes" && "${USE_TCP:-no}" != "yes" ]]; then
            reason="it needs the TCP transport, this leg runs the stdio-pipe default"
        elif [[ "$needs_root" == "yes" && "$euid" -ne 0 ]]; then
            reason="it needs root, this leg runs as uid ${euid}"
        elif [[ -n "$expect_result_file" ]] \
            && ! grep -qE "^[[:space:]]*${test_name}[[:space:]]" "$expect_result_file"; then
            reason="the expected-outcome manifest does not name it, so it will not run"
        fi
        if [[ -n "$reason" ]]; then
            echo "==> Legacy oracle rsync ${version}: not needed - ${test_name} cannot run on this leg (${reason})." >&2
            unreachable=$((unreachable + 1))
            continue
        fi
        if [[ "$legacy_oracles_mode" == "off" ]]; then
            echo "==> Legacy oracle rsync ${version}: NOT BUILT (LEGACY_ORACLES=off) - ${test_name} runs on this leg and will assert against its fallback, not against rsync ${version}." >&2
            degraded=$((degraded + 1))
            degraded_names+="${degraded_names:+, }${test_name} (rsync ${version})"
            continue
        fi
        echo "==> Legacy oracle rsync ${version}: ${test_name} runs on this leg and asserts against it." >&2
        if ! bash "${workspace_root}/tools/ci/build_old_rsync_oracle.sh" \
            "$version" "$old_versions_dir" "$oracle_workdir" >&2; then
            echo "ERROR: could not put the rsync ${version} oracle on disk for ${test_name}." >&2
            echo "       This is fatal ON PURPOSE. Missing, that test does not fail:" >&2
            echo "       it swaps the live oracle for its fallback, asserts the weaker" >&2
            echo "       contract and still reports PASS - so the leg would report" >&2
            echo "       success over a contract it never checked." >&2
            gha_annotate_fail "legacy rsync oracle unavailable" \
                "${test_name} asserts against a real rsync ${version} daemon; the build of ${old_versions_dir}/rsync_${version} failed, and without it the test degrades to its fallback and still passes."
            exit 1
        fi
        built=$((built + 1))
    done <<< "$requirements"
    if (( degraded > 0 )); then
        gha_annotate_warn "legacy rsync oracles not built" \
            "LEGACY_ORACLES=off on this leg, so ${degraded} test(s) assert against a static fallback rather than the release they name: ${degraded_names}. Set legacy_oracles: 'on' for this leg once its expect manifest can be re-measured."
    fi
    echo "==> Legacy oracles: ${built} on disk, ${degraded} consumer(s) running degraded, ${unreachable} not needed on this leg." >&2
}

# Drive upstream's own runtests.py against oc-rsync.
#
# Used for any tree shipping the Python suite - the 3.5.0 release tarball and
# RsyncProject git refs alike. Delegating to upstream's runner rather than
# re-driving `*_test.py` ourselves keeps the oracle upstream's, and gives us
# --rsync-bin2 (version mixing over the wire) and --expect-result (an
# expected-outcome manifest that fails on an unexpected PASS as well as a
# regression) for free.
run_python_suite_mode() {
    ensure_oc_rsync
    ensure_upstream_src

    echo "==> Building upstream (${upstream_label}) + testsuite helpers..." >&2
    (
        cd "$upstream_src_dir"
        # `shconfig` is what ./configure PRODUCES, so it is the honest
        # already-configured test for both inputs. A release tarball ships
        # configure.sh itself, so keying on that file skipped ./configure for a
        # tarball and the build then failed for want of shconfig/config.h. A
        # fresh git checkout ships a stub ./configure that bootstraps
        # configure.sh via prepare-source, so this predicate holds there too.
        if [[ ! -f shconfig ]]; then
            ./configure --disable-debug --disable-md2man --disable-iconv \
                --disable-zstd --disable-lz4 >configure.log 2>&1 \
                || { tail -80 configure.log; exit 1; }
        fi
        # check-progs builds `all` + CHECK_PROGS + CHECK_SYMLINKS: exactly the
        # tools runtests.py needs (upstream Makefile.in:381).
        make check-progs >make.log 2>&1 || { tail -120 make.log; exit 1; }
    )

    # After the tree is extracted (the requirements are read out of it) and
    # before runtests.py runs (the tests stat the binary at import time).
    ensure_legacy_oracles

    rm -rf "$log_root"
    mkdir -p "$log_root"
    local output_log="${log_root}/runtests-output.log"

    # Point --rsync-bin at a world-traversable copy of the binary so the root
    # leg's setpriv (CAP_DAC_OVERRIDE-dropped) exec can reach it - see
    # publish_oc_rsync_bin(). Non-CI runs where no such dir is writable fall
    # back to the original path, so behaviour there is unchanged.
    local rsync_bin_published
    rsync_bin_published=$(publish_oc_rsync_bin "$oc_rsync_bin")
    if [[ "$rsync_bin_published" != "$oc_rsync_bin" ]]; then
        echo "==> Published oc-rsync to ${rsync_bin_published} (setpriv-reachable)" >&2
    fi

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
    # Narrow to the tests that can observe the transport. Upstream ships this
    # for exactly one purpose: "Intended for a --use-tcp pass that follows a
    # full default-transport run: the tests this drops never call
    # start_test_daemon(), so they cannot observe --use-tcp and would just
    # repeat themselves." Selecting it WITHOUT --use-tcp would run a strict
    # subset of a pass already covered, so refuse rather than silently shrink
    # coverage - the same failure mode the manifest guard below defends.
    # upstream: runtests.py --daemon-tests-only (3.5.0 runtests.py:130-136)
    if [[ "${DAEMON_TESTS_ONLY:-no}" == "yes" ]]; then
        if [[ "${USE_TCP:-no}" != "yes" ]]; then
            echo "ERROR: DAEMON_TESTS_ONLY=yes requires USE_TCP=yes." >&2
            echo "       Without --use-tcp it selects a strict subset of the" >&2
            echo "       default-transport run, shrinking coverage for nothing." >&2
            exit 1
        fi
        runtests_argv+=(--daemon-tests-only)
    fi
    runtests_argv+=(
        --rsync-bin="$rsync_bin_published"
        --tooldir="$upstream_src_dir"
        --srcdir="$upstream_src_dir"
        --timeout="$testrun_timeout"
    )
    # --rsync-bin2 is the peer binary for the daemon side and for remote-shell
    # --rsync-path. Pointing it at a real upstream build puts oc on one end of
    # the wire and upstream on the other: this, not the wire-format interop
    # cells, is what actually tests compatibility with a given release. Absent,
    # the suite runs oc against oc and proves only self-consistency.
    if [[ -n "$upstream_peer_bin" ]]; then
        if [[ ! -x "$upstream_peer_bin" ]]; then
            echo "ERROR: UPSTREAM_PEER_BIN is set but not executable: ${upstream_peer_bin}" >&2
            echo "       Refusing to fall back to oc-vs-oc, which would report" >&2
            echo "       a version-mixing pass without ever mixing versions." >&2
            exit 1
        fi
        echo "==> Peer (--rsync-bin2): ${upstream_peer_bin}" >&2
        "$upstream_peer_bin" --version 2>&1 | head -n1 | sed 's/^/    /' >&2
        runtests_argv+=(--rsync-bin2="$upstream_peer_bin")
    fi
    # An expected-outcome manifest runs ONLY the tests it lists, so a truncated
    # manifest silently shrinks coverage rather than failing. Refuse a missing
    # file outright, and report the count we are about to hold ourselves to.
    if [[ -n "$expect_result_file" ]]; then
        if [[ ! -f "$expect_result_file" ]]; then
            echo "ERROR: EXPECT_RESULT does not exist: ${expect_result_file}" >&2
            exit 1
        fi
        local expect_count
        expect_count=$(grep -cvE '^[[:space:]]*(#|$)' "$expect_result_file" || true)
        if (( expect_count == 0 )); then
            echo "ERROR: EXPECT_RESULT lists no tests: ${expect_result_file}" >&2
            echo "       runtests.py runs only what the manifest names, so an" >&2
            echo "       empty manifest would pass without running anything." >&2
            exit 1
        fi
        # A zero-test guard is not enough. MEASURED against runtests.py: dropping
        # ONE line shrinks the run set with NO diagnostic and still exits 0 (2
        # tests -> 1 test, no warning), so coverage can erode a line at a time
        # while the gate stays green. Only an outcome MISMATCH is self-reporting.
        # Require the manifest to name every test the extracted tree ships, so
        # the ledger cannot quietly cover less than the suite. Derived from the
        # tree rather than hardcoded, so an upstream bump that adds or removes
        # tests re-baselines instead of tripping a stale constant.
        local suite_count
        suite_count=$(selected_test_count)
        if (( suite_count > 0 && expect_count < suite_count )); then
            echo "ERROR: EXPECT_RESULT covers ${expect_count} of ${suite_count} tests: ${expect_result_file}" >&2
            echo "       --expect-result runs ONLY what it names, so the missing" >&2
            echo "       $(( suite_count - expect_count )) would silently not run." >&2
            echo "       Regenerate with EMIT_EXPECT_RESULT rather than editing." >&2
            exit 1
        fi
        echo "==> Expected-outcome manifest: ${expect_result_file} (${expect_count}/${suite_count} tests)" >&2
        runtests_argv+=(--expect-result="$expect_result_file")
    fi
    if [[ -n "$expect_skipped_spec" ]]; then
        runtests_argv+=(--expect-skipped="$expect_skipped_spec")
    fi
    # Host the scratch tree under a world-traversable base OUTSIDE the parent
    # that partial_nowrite_test.py shadows with a tmpfs (see
    # world_traversable_scratch_base). Falls back to $log_root off-CI, so local
    # runs (no root leg, no mount namespace) are unchanged.
    local scratch_base
    scratch_base=$(world_traversable_scratch_base "$log_root")
    local scratch_home="${scratch_base}/oc-rsync-uts-scratch-${mode_tag}-${transport_tag}"
    chmod -R u+rwX "$scratch_home" 2>/dev/null || true
    rm -rf "$scratch_home"
    mkdir -p "$scratch_home"
    if [[ "$scratch_base" != "$log_root" ]]; then
        echo "==> Scratch tree under ${scratch_home} (outside the tmpfs-shadowed HOME)" >&2
    fi

    # `|| rc=...` is load-bearing, not defensive. Under `set -e` + `pipefail` a
    # bare failing pipeline aborts the script AT THIS LINE, so `rc=${PIPESTATUS[0]}`
    # and everything after it - the step summary AND the expect-result manifest -
    # never ran whenever the suite failed. That made the manifest generator
    # reachable only when every test passed, i.e. exactly when no manifest is
    # needed. Putting the pipeline in an OR list exempts it from `set -e` while
    # PIPESTATUS still carries runtests.py's own status, which `return "$rc"`
    # below propagates unchanged.
    local rc=0
    (
        cd "$upstream_src_dir"
        # scratchbase -> runtests.py places $scratchbase/testtmp here, off the
        # source tree, so the cleanup above owns the whole scratch lifecycle.
        scratchbase="$scratch_home" "${runtests_argv[@]}"
    ) 2>&1 | tee "$output_log" || rc=${PIPESTATUS[0]}

    # Force-clear the scratch tree again so the NEXT leg (or a re-run on the
    # same self-hosted runner) never inherits a mode-0 dir from this leg.
    chmod -R u+rwX "$scratch_home" 2>/dev/null || true
    rm -rf "$scratch_home" 2>/dev/null || true

    emit_git_ref_step_summary "$output_log" "$rc"
    emit_expect_result_manifest "$output_log"
    return "$rc"
}

# Write an --expect-result manifest from a completed runtests.py run.
#
# The consuming side already exists (EXPECT_RESULT above). Without a generator
# the ledger has to be hand-curated, which is precisely how it drifts out of
# agreement with the suite - and because --expect-result runs ONLY the tests it
# names, a stale manifest silently narrows coverage instead of failing. So the
# manifest is derived from a real run, never typed.
#
# runtests.py prints one line per test as `<OUTCOME><spaces><name>` (MEASURED:
# four spaces, not a tab - assuming a tab here parsed zero lines and would have
# emitted an empty manifest). SKIP additionally carries a trailing " (reason)"
# that is NOT part of the name, so split on whitespace and keep field 2: the
# reason falls into $3+ and is dropped without a second rule. Emit the
# `<name> <outcome>` form --expect-result parses, sorted so a re-baseline diffs
# as a set of outcome changes rather than a reordering.
emit_expect_result_manifest() {
    local output_log=$1
    local manifest=${EMIT_EXPECT_RESULT:-}
    [[ -z "$manifest" ]] && return 0
    [[ -f "$output_log" ]] || return 0

    mkdir -p "$(dirname "$manifest")"
    {
        echo "# Generated by tools/ci/run_upstream_testsuite.sh - do not hand-edit."
        echo "# Regenerate: EMIT_EXPECT_RESULT=<path> bash tools/ci/run_upstream_testsuite.sh"
        echo "# Source: ${upstream_label} (${mode_tag}/${transport_tag})"
        awk '
            NF < 2 { next }
            $1 == "PASS"  { print $2 " pass"  }
            $1 == "FAIL"  { print $2 " fail"  }
            $1 == "SKIP"  { print $2 " skip"  }
            $1 == "XFAIL" { print $2 " xfail" }
        ' "$output_log" | sort -u
    } >"$manifest"

    local n
    n=$(grep -cvE '^[[:space:]]*(#|$)' "$manifest" || true)
    # A manifest that names nothing would make a later --expect-result run pass
    # without executing a single test. Refuse to write that.
    if (( n == 0 )); then
        echo "ERROR: emitted manifest lists no tests: ${manifest}" >&2
        echo "       runtests.py output had no '<OUTCOME>  <name>' lines to parse." >&2
        return 1
    fi
    echo "==> Wrote expected-outcome manifest: ${manifest} (${n} tests)" >&2
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
        run_python_suite_mode
        exit $?
    fi

    ensure_oc_rsync
    ensure_upstream_src

    # Pick the runner from what the extracted release actually ships, not from
    # its version number. 3.4.4 ships 57 `*.test` shell scripts and takes the
    # loop below; 3.5.0 ships runtests.py + 345 `*_test.py` and takes upstream's
    # own runner. Before this dispatch existed, pointing UPSTREAM_VERSION at
    # 3.5.0 selected zero `*.test` files and the loop reported success without
    # executing a single test - a vacuous green on a required gate.
    if python_suite_available; then
        run_python_suite_mode
        exit $?
    fi

    build_upstream_helpers
    setup_test_env
    stabilize_srcroot_mtime

    # Tear down a stale loop mount from a prior run killed mid-flight before
    # wiping $log_root, so rm -rf never recurses into a live mount.
    if is_mounted "${log_root}/xattr-scratch"; then
        priv umount "${log_root}/xattr-scratch" 2>/dev/null || \
            priv umount -l "${log_root}/xattr-scratch" 2>/dev/null || true
    fi
    rm -rf "$log_root"
    mkdir -p "$log_root"
    # Host the scratch tree on a user.*-xattr-capable filesystem so xattrs.test
    # runs instead of self-skipping. Falls back to $log_root/scratch (logged)
    # when a loop mount is unavailable.
    setup_scratch_fs "${log_root}/scratch"

    # upstream: runtests.py find_setfacl_nodef() - detect and export
    # setfacl_nodef so ACL tests can clear default ACLs from directories.
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

    # A glob that matches nothing expands to the literal pattern, which the
    # `-e` test above then skips - so the loop body never runs and every
    # counter stays 0. `summarize` would report that as success: a REQUIRED
    # gate passing without executing a single test.
    #
    # This is reachable today by pointing UPSTREAM_VERSION at a release whose
    # testsuite is Python rather than shell. Measured: rsync-3.4.4/testsuite
    # has 57 *.test and 0 *_test.py; rsync-3.5.0/testsuite has 0 *.test and
    # 345 *_test.py. So `UPSTREAM_VERSION=3.5.0` down this path selects zero
    # tests and exits clean. Driving a Python-suite release needs
    # runtests.py (see run_python_suite_mode), not this loop.
    local considered=$((passed + failed + xfail + skipped + ${#unexpected_passes[@]}))
    if (( considered == 0 )); then
        echo "ERROR: no tests matched '${pattern}' in ${suitedir}" >&2
        echo "       Refusing to report success without executing anything." >&2
        echo "       A release whose testsuite is Python (*_test.py + runtests.py)" >&2
        echo "       cannot be driven by this shell-script loop." >&2
        exit 1
    fi

    summarize
    emit_gha_step_summary

    if (( failed > 0 || ${#unexpected_passes[@]} > 0 )); then
        exit 1
    fi
}

# Ensure the loop-ext4 scratch image is always unmounted and removed, even on
# an early exit or failure. No-op when no image was mounted (git-ref mode,
# fallback path, or local dev).
cleanup_run() {
    cleanup_scratch_fs
    cleanup_published_bin
}

# Executed: install the cleanup trap and run. Sourced: define the functions and
# stop, so tools/tests/ can drive one of them (ensure_legacy_oracles) against a
# synthetic tree without launching a 45-minute suite - and without the EXIT trap
# tearing down the sourcing shell's mounts. `$0` is the sourcing program when
# sourced and this file when executed, so the comparison distinguishes the two
# without a mode flag anyone has to remember to pass.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    trap cleanup_run EXIT
    main "$@"
fi
