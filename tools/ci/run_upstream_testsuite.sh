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
    for dir in /usr/local/bin /usr/bin; do
        [[ -d "$dir" && -w "$dir" ]] || continue
        path_world_traversable "$dir" || continue
        local dst="${dir}/oc-rsync"
        if cp -f "$src" "$dst" 2>/dev/null && chmod 0755 "$dst" 2>/dev/null; then
            echo "$dst"
            return 0
        fi
    done
    # No writable world-traversable dir: keep the original path.
    echo "$src"
    return 0
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
    runtests_argv+=(
        --rsync-bin="$rsync_bin_published"
        --tooldir="$upstream_src_dir"
        --srcdir="$upstream_src_dir"
        --timeout="$testrun_timeout"
    )
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

# Ensure the loop-ext4 scratch image is always unmounted and removed, even on
# an early exit or failure. No-op when no image was mounted (git-ref mode,
# fallback path, or local dev).
trap cleanup_scratch_fs EXIT

main "$@"
