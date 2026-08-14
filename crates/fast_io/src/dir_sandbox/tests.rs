//! Unit tests for [`DirSandbox`](super::DirSandbox).
//!
//! Exercise the stack/cache invariants on tempdirs and pin the two
//! *different* policies that apply at the two opens.
//!
//! The root open ([`secure_open_dir`](crate::secure_open_dir::secure_open_dir))
//! is a bootstrap against `AT_FDCWD` with an absolute path, so it refuses
//! symlinks outright.
//!
//! The descent open ([`super::DirSandbox::enter`]) is anchored on a dirfd and
//! discriminates by destination instead: an in-tree symlink is followed, an
//! escape is refused with `EXDEV`. That mirrors upstream `ds_descend()`
//! (`syscall.c:2891`). Earlier revisions of this file asserted the descent
//! refused every symlink; that was stricter than upstream and the assertion
//! pinned the divergence rather than the contract.

use std::os::fd::AsRawFd;
use std::os::unix::fs::symlink;
use std::sync::Arc;
use std::thread;

use tempfile::tempdir;

use super::DirSandbox;

/// `tempdir()` may return a path under a symlinked prefix (macOS
/// `/tmp -> /private/tmp`, some CI runners stage `/tmp` through a
/// symlink). `secure_open_dir` refuses such paths under
/// `RESOLVE_NO_SYMLINKS`, so every test that opens a tempdir as the
/// sandbox root first canonicalises.
fn canonical_tempdir() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempdir().expect("tempdir");
    let canon = std::fs::canonicalize(dir.path()).expect("canonicalize tempdir");
    (dir, canon)
}

#[test]
fn open_root_yields_live_fd() {
    let (_keep, root) = canonical_tempdir();
    let sandbox = DirSandbox::open_root(&root).expect("open root");
    assert!(sandbox.current_dirfd().as_raw_fd() >= 0);
    assert_eq!(sandbox.depth(), 0);
}

#[test]
fn open_root_rejects_symlink_root() {
    let (_keep, root) = canonical_tempdir();
    let target = root.join("real");
    std::fs::create_dir(&target).expect("create real dir");
    let link = root.join("link");
    symlink(&target, &link).expect("create symlink");

    let err = DirSandbox::open_root(&link).expect_err("symlink root must be rejected");
    let code = err.raw_os_error();
    // Linux + openat2 returns ELOOP; Linux + plain open(O_NOFOLLOW)
    // also returns ELOOP; macOS/BSD evaluate O_DIRECTORY before
    // O_NOFOLLOW and return ENOTDIR for symlink-to-directory.
    assert!(
        code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
        "expected ELOOP or ENOTDIR for symlink root, got: {err}"
    );
}

#[test]
fn enter_and_exit_balance_the_stack() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("a")).expect("mkdir a");
    std::fs::create_dir(root.join("a/b")).expect("mkdir a/b");

    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    let root_raw = sandbox.current_dirfd().as_raw_fd();
    assert_eq!(sandbox.depth(), 0);

    sandbox.enter(std::ffi::OsStr::new("a")).expect("enter a");
    assert_eq!(sandbox.depth(), 1);
    let a_raw = sandbox.current_dirfd().as_raw_fd();
    assert_ne!(a_raw, root_raw, "entering must hand out a new fd");

    sandbox.enter(std::ffi::OsStr::new("b")).expect("enter b");
    assert_eq!(sandbox.depth(), 2);
    let b_raw = sandbox.current_dirfd().as_raw_fd();
    assert_ne!(b_raw, a_raw);

    sandbox.exit();
    assert_eq!(sandbox.depth(), 1);
    assert_eq!(
        sandbox.current_dirfd().as_raw_fd(),
        a_raw,
        "exit must restore the prior parent dirfd"
    );

    sandbox.exit();
    assert_eq!(sandbox.depth(), 0);
    assert_eq!(
        sandbox.current_dirfd().as_raw_fd(),
        root_raw,
        "exit to empty must return the root"
    );
}

#[test]
fn exit_on_empty_stack_is_noop() {
    let (_keep, root) = canonical_tempdir();
    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    sandbox.exit();
    sandbox.exit();
    assert_eq!(sandbox.depth(), 0);
}

/// An **in-tree** symlinked subdirectory must be descended, not refused.
///
/// upstream: `syscall.c:2891` `ds_descend()` splices a relative in-tree
/// symlink target back into the walk. Refusing it is stricter than upstream
/// and turns an ordinary destination layout into a hard failure.
///
/// ⚠ The symlink target must be **relative**. An absolute target is refused
/// under `RESOLVE_BENEATH` even when it resolves inside the anchor, and
/// upstream refuses absolute targets too (`syscall.c:2953`) - so a fixture
/// built with `symlink(root.join("real"), ...)` passes for the wrong reason
/// and proves nothing about this contract.
#[test]
fn enter_follows_in_tree_symlink_child() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("real")).expect("create real dir");
    symlink("real", root.join("link")).expect("relative in-tree symlink");

    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    sandbox
        .enter(std::ffi::OsStr::new("link"))
        .expect("in-tree symlinked subdirectory must be descended");
    assert_eq!(sandbox.depth(), 1);
    sandbox.exit();
    assert_eq!(sandbox.depth(), 0);
}

/// A **relative** symlink whose target escapes the anchor must still fail,
/// and fail as an escape (`EXDEV`) rather than as "there was a symlink".
///
/// This is the negative half of [`enter_follows_in_tree_symlink_child`]: the
/// two together say the policy discriminates by *destination*, not by the
/// mere presence of a symlink.
#[test]
fn enter_refuses_relative_symlink_that_escapes() {
    let (_keep, root) = canonical_tempdir();
    symlink("../outside", root.join("esc")).expect("relative escaping symlink");

    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    let err = sandbox
        .enter(std::ffi::OsStr::new("esc"))
        .expect_err("a symlink leaving the anchor must be refused");
    let code = err.raw_os_error();
    assert!(
        code == Some(libc::EXDEV) || code == Some(libc::ELOOP) || code == Some(libc::ENOENT),
        "expected an escape refusal, got: {err}"
    );
    assert_eq!(sandbox.depth(), 0);
}

#[test]
fn enter_rejects_missing_child() {
    let (_keep, root) = canonical_tempdir();
    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    let err = sandbox
        .enter(std::ffi::OsStr::new("does-not-exist"))
        .expect_err("missing child must error");
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
    assert_eq!(sandbox.depth(), 0);
}

#[test]
fn enter_rejects_file_child() {
    let (_keep, root) = canonical_tempdir();
    std::fs::write(root.join("file"), b"x").expect("write file");
    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    let err = sandbox
        .enter(std::ffi::OsStr::new("file"))
        .expect_err("file child must error");
    assert_eq!(err.raw_os_error(), Some(libc::ENOTDIR));
    assert_eq!(sandbox.depth(), 0);
}

#[test]
fn secondary_is_idempotent() {
    let (_keep_root, root) = canonical_tempdir();
    let (_keep_other, other) = canonical_tempdir();
    let sandbox = DirSandbox::open_root(&root).expect("open root");
    assert_eq!(sandbox.secondary_count(), 0);

    let fd1 = sandbox.secondary(&other).expect("register operand");
    assert_eq!(sandbox.secondary_count(), 1);

    let fd2 = sandbox.secondary(&other).expect("re-lookup operand");
    assert_eq!(sandbox.secondary_count(), 1);
    assert!(
        Arc::ptr_eq(&fd1, &fd2),
        "second call must return the same Arc"
    );
}

#[test]
fn secondary_rejects_symlink_operand() {
    let (_keep_root, root) = canonical_tempdir();
    let (_keep_other, other) = canonical_tempdir();
    let target = other.join("real");
    std::fs::create_dir(&target).expect("create real");
    let link = other.join("link");
    symlink(&target, &link).expect("symlink");

    let sandbox = DirSandbox::open_root(&root).expect("open root");
    let err = sandbox
        .secondary(&link)
        .expect_err("symlink operand must be rejected");
    let code = err.raw_os_error();
    assert!(
        code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
        "expected ELOOP or ENOTDIR for symlink operand, got: {err}"
    );
    assert_eq!(sandbox.secondary_count(), 0);
}

#[test]
fn secondary_concurrent_registrations_collapse_to_one() {
    let (_keep_root, root) = canonical_tempdir();
    let (_keep_other, other) = canonical_tempdir();
    let sandbox = Arc::new(DirSandbox::open_root(&root).expect("open root"));

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let sandbox = Arc::clone(&sandbox);
            let other = other.clone();
            thread::spawn(move || sandbox.secondary(&other).expect("register"))
        })
        .collect();

    let mut fds = Vec::new();
    for handle in handles {
        fds.push(handle.join().expect("thread"));
    }

    // Every thread must observe the same cached Arc, and the cache
    // must contain exactly one entry regardless of registration races.
    let first = &fds[0];
    for other in &fds[1..] {
        assert!(
            Arc::ptr_eq(first, other),
            "all threads must share one operand handle"
        );
    }
    assert_eq!(sandbox.secondary_count(), 1);
}

#[test]
fn root_arc_clones_share_owner() {
    let (_keep, root) = canonical_tempdir();
    let sandbox = DirSandbox::open_root(&root).expect("open root");
    let arc1 = sandbox.root_arc();
    let arc2 = sandbox.root_arc();
    assert!(Arc::ptr_eq(&arc1, &arc2));
    assert_eq!(arc1.as_raw_fd(), sandbox.root_dirfd().as_raw_fd());
}

/// EDG-SANDBOX.2 contract test - chdir-symlink-race trap.
///
/// Locks the documented error-class contract that the
/// receiver's silent-skip audit (`docs/audits/edg-sandbox-silent-skip.md`)
/// and the PR #5565 refined error discrimination both rely on:
/// `DirSandbox::enter` MUST refuse to traverse a symlink at the leaf,
/// even when the symlink's target points outside the sandbox root (the
/// classic chdir-symlink-race shape where an attacker drops
/// `subdir -> ../outside` between the receiver's `mkdir` and its first
/// per-entry syscall).
///
/// The kernel rejects with `ELOOP` on Linux + `openat2(RESOLVE_NO_SYMLINKS)`
/// and on plain `openat(O_NOFOLLOW)`; macOS/BSD evaluates `O_DIRECTORY`
/// before `O_NOFOLLOW` and returns `ENOTDIR` for a symlink-to-directory.
/// On Linux + `openat2(RESOLVE_BENEATH)` a `..` segment that escapes the
/// root surfaces as `EXDEV`. All three are valid refusal classes; the
/// invariant is that `enter` never silently succeeds and the stack stays
/// untouched.
#[test]
fn enter_through_symlink_to_outside_refuses() {
    let (_keep_root, root) = canonical_tempdir();
    // The "outside" target lives in a sibling tempdir so the symlink
    // genuinely points outside the sandbox root. The chdir-symlink-race
    // POC drops a similar shape mid-transfer to redirect per-entry
    // syscalls to an attacker-chosen parent.
    let (_keep_outside, outside) = canonical_tempdir();
    symlink(&outside, root.join("subdir")).expect("plant trap symlink");

    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    let err = sandbox
        .enter(std::ffi::OsStr::new("subdir"))
        .expect_err("symlink trap must be refused");
    let code = err.raw_os_error();
    assert!(
        code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR) || code == Some(libc::EXDEV),
        "expected ELOOP, ENOTDIR, or EXDEV for symlink-to-outside trap, got: {err}"
    );
    // The descent stack must stay empty so the receiver's subsequent
    // `current_dirfd()` call still anchors on the sandbox root, not on
    // an attacker-controlled descriptor.
    assert_eq!(sandbox.depth(), 0);
}

/// EDG-SANDBOX.2 positive contract test.
///
/// Sibling of [`enter_through_symlink_to_outside_refuses`]: confirms that a
/// real on-tree subdirectory is accepted so the audit's "refine error
/// discrimination" rule (PR #5565) does not regress the happy path. A
/// receiver that fails closed on every error class must still let
/// legitimate descents through.
#[test]
fn enter_to_legitimate_subdir_returns_ok() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("subdir")).expect("mkdir subdir");

    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    sandbox
        .enter(std::ffi::OsStr::new("subdir"))
        .expect("legitimate subdir must succeed");
    assert_eq!(sandbox.depth(), 1);
    sandbox.exit();
    assert_eq!(sandbox.depth(), 0);
}

/// The seam, exercised through the arm that is already live.
///
/// An operator-trusted policy must resolve a peer tail exactly as the
/// unpolicied walk does today: an in-tree relative symlink is followed.
/// This pins the `NoExclude` arm to `RESOLVE_BENEATH` semantics so the
/// oracle arm (tasks 599/600), which replaces the mechanism, cannot
/// silently change this one.
///
/// # Upstream Reference
///
/// - `syscall.c:2891` `ds_descend()` - follows a relative in-tree target.
#[test]
fn operator_trusted_policy_follows_a_relative_in_tree_symlink() {
    let (_guard, root) = canonical_tempdir();
    std::fs::create_dir(root.join("real")).expect("mkdir real");
    std::fs::write(root.join("real").join("marker"), b"x").expect("write marker");
    symlink("real", root.join("sub")).expect("symlink sub -> real");

    let sandbox = DirSandbox::open_dest_anchor_with_policy(
        &root,
        std::path::Path::new("sub"),
        super::ConfinePolicy::operator_trusted(),
    )
    .expect("an in-tree relative symlink must resolve under the operator-trusted policy");

    sandbox
        .lstat_at(std::ffi::OsStr::new("marker"))
        .expect("the sandbox must be anchored at real/, reached through sub");
}

/// The descriptor-exhaustion hint must fire for exactly the two errnos
/// upstream tests for, and for nothing else.
///
/// The interesting negative is `ENOENT`: every descent that walks past a
/// missing component produces one, so a predicate that fired on any error
/// would print the hint on ordinary traffic. Testing only the positives
/// would not catch that - the mutation `exhausted = true` survives a
/// positives-only suite.
///
/// upstream: syscall.c:2924 `if (errno == EMFILE || errno == ENFILE)`.
#[test]
fn fd_exhaustion_predicate_matches_only_emfile_and_enfile() {
    use rustix::io::Errno;

    for errno in [Errno::MFILE, Errno::NFILE] {
        let latch = std::sync::atomic::AtomicBool::new(false);
        let err = std::io::Error::from_raw_os_error(errno.raw_os_error());
        assert!(
            super::should_warn_fd_exhaustion(&err, &latch),
            "{errno:?} is a descriptor-exhaustion failure and must warn"
        );
    }

    for errno in [
        Errno::NOENT,
        Errno::ACCESS,
        Errno::LOOP,
        Errno::XDEV,
        Errno::NOTDIR,
    ] {
        let latch = std::sync::atomic::AtomicBool::new(false);
        let err = std::io::Error::from_raw_os_error(errno.raw_os_error());
        assert!(
            !super::should_warn_fd_exhaustion(&err, &latch),
            "{errno:?} is an ordinary resolution failure and must stay silent"
        );
        assert!(
            !latch.load(std::sync::atomic::Ordering::Relaxed),
            "{errno:?} must not consume the one-shot latch"
        );
    }
}

/// The hint is one-shot: a deep tree hits the ceiling once per component
/// and would otherwise flood stderr with an identical line.
///
/// upstream: syscall.c:2926-2928 `static int warned = 0; if (!warned)`.
#[test]
fn fd_exhaustion_hint_is_emitted_at_most_once() {
    use rustix::io::Errno;

    let latch = std::sync::atomic::AtomicBool::new(false);
    let err = std::io::Error::from_raw_os_error(Errno::MFILE.raw_os_error());

    assert!(
        super::should_warn_fd_exhaustion(&err, &latch),
        "the first exhaustion must warn"
    );
    for attempt in 2..=16 {
        assert!(
            !super::should_warn_fd_exhaustion(&err, &latch),
            "attempt {attempt} must stay silent once the latch is claimed"
        );
    }
}

/// Concurrent claimants still produce exactly one line.
///
/// The carrier hands `BorrowedFd` copies to rayon workers, so several
/// threads can hit the ceiling in the same instant. A non-atomic
/// load-then-store latch would let more than one through.
#[test]
fn fd_exhaustion_latch_admits_exactly_one_concurrent_claimant() {
    use rustix::io::Errno;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let latch = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let claims = Arc::new(AtomicUsize::new(0));

    let handles: Vec<_> = (0..16)
        .map(|_| {
            let latch = Arc::clone(&latch);
            let claims = Arc::clone(&claims);
            thread::spawn(move || {
                let err = std::io::Error::from_raw_os_error(Errno::MFILE.raw_os_error());
                if super::should_warn_fd_exhaustion(&err, &latch) {
                    claims.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("join claimant");
    }

    assert_eq!(
        claims.load(Ordering::Relaxed),
        1,
        "the latch must admit exactly one claimant across all threads"
    );
}

/// The hint text is upstream's, byte for byte, with no `rsync warning:`
/// envelope.
///
/// `rprintf(FWARNING, ...)` is routed to stderr verbatim by `rwrite()`
/// (log.c:341); the `rsync warning: ... (code N) at FILE(LINE)` wording is
/// spelled out at its own call site (log.c:956) and does not apply here.
/// Pinning the text is what stops a later "improvement" from wrapping it.
///
/// upstream: syscall.c:2930-2931.
#[test]
fn fd_exhaustion_hint_text_matches_upstream_verbatim() {
    assert_eq!(
        super::FD_EXHAUSTION_HINT,
        "out of file descriptors resolving a deep path; raise the open-file limit (e.g. `ulimit -n`)"
    );
    assert!(
        !super::FD_EXHAUSTION_HINT.contains("rsync warning:"),
        "upstream's FWARNING at this site carries no envelope"
    );
    assert!(
        !super::FD_EXHAUSTION_HINT.ends_with('\n'),
        "the newline belongs to the emit, not the text"
    );
}

/// Cross-check against a real kernel refusal: `enter()` must emit the hint
/// on stderr and must still report `EMFILE` to its caller.
///
/// This is the only cell that exercises the wiring inside `openat_dir`
/// rather than the predicate in isolation. Without the stderr half,
/// deleting the `eprintln!` from `openat_dir` leaves every other test in
/// this file green - the predicate stays correct and becomes dead code.
///
/// The errno half is the port's answer to upstream's `int e = errno; ...
/// errno = e;`. Upstream needs the save/restore because `rprintf` can
/// clobber the global `errno`; the Rust port owns the failure as a moved
/// [`std::io::Error`], so nothing the emit does can reach it. Asserting
/// the errno here is what makes that a checked claim rather than a stated
/// one: a refactor that re-derives the error after the emit (from a fresh
/// `Errno::last_os_error()`, say) changes what this sees.
///
/// Both the `RLIMIT_NOFILE` drop and the stderr redirect are
/// process-global. That is safe because nextest runs each test in its own
/// process. Every descriptor the test needs is allocated *before* the
/// limit drops, and both globals are restored before the assertions so a
/// failing assert can still allocate for its own output.
///
/// upstream: syscall.c:2924-2936.
#[test]
fn real_fd_exhaustion_warns_once_and_surfaces_emfile_to_the_caller() {
    use rustix::io::dup;
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
    use rustix::stdio::{dup2_stderr, stderr};
    use std::io::{Read, Seek, SeekFrom};

    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("subdir")).expect("mkdir subdir");
    let mut sandbox = DirSandbox::open_root(&root).expect("open root");

    // Allocate every descriptor up front - none of this can succeed once
    // the limit is down.
    let mut captured = tempfile::tempfile().expect("stderr capture file");
    let saved_stderr = dup(stderr()).expect("save stderr");
    let original = getrlimit(Resource::Nofile);

    dup2_stderr(&captured).expect("redirect stderr");

    // Existing descriptors keep working; only new allocations are refused,
    // because the kernel checks the limit when it picks an fd number.
    setrlimit(
        Resource::Nofile,
        Rlimit {
            current: Some(3),
            maximum: original.maximum,
        },
    )
    .expect("lower RLIMIT_NOFILE");

    let outcome = sandbox.enter(std::ffi::OsStr::new("subdir"));

    setrlimit(Resource::Nofile, original).expect("restore RLIMIT_NOFILE");
    dup2_stderr(&saved_stderr).expect("restore stderr");

    let mut emitted = String::new();
    captured.seek(SeekFrom::Start(0)).expect("rewind capture");
    captured
        .read_to_string(&mut emitted)
        .expect("read captured stderr");

    assert_eq!(
        emitted.matches(super::FD_EXHAUSTION_HINT).count(),
        1,
        "the descent must print the hint exactly once; got: {emitted:?}"
    );

    let err = outcome.expect_err("descent must fail once no descriptor can be allocated");
    assert_eq!(
        err.raw_os_error(),
        Some(rustix::io::Errno::MFILE.raw_os_error()),
        "the caller must still see EMFILE, not an error re-derived after the hint"
    );
}
