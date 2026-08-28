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
///
/// ⚠ Whether the descent *can* follow it depends on the mechanism available.
/// `openat2` resolves the symlink inside the kernel and lands beneath the
/// anchor; the portable `openat(O_NOFOLLOW)` fallback cannot express "follow
/// only if it stays inside" and refuses. [`openat_dir`](super::openat_dir)
/// records that as the portable-fallback gap. Both are asserted, keyed on the
/// same runtime predicate the code consults - asserting only the first made
/// this red on every non-Linux target while CI never ran the crate there.
///
/// ⚠ As above, the refusal arm is a **tracked divergence, not the contract**:
/// upstream follows the target (`ds_descend`, `syscall.c:2961`). Parity is
/// task 551; `enter()`'s behaviour is deliberately unchanged here, because
/// making the fallback follow in-tree symlinks is a cross-platform decision
/// and not a test fix.
#[test]
fn enter_follows_in_tree_symlink_child_where_the_kernel_can() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("real")).expect("create real dir");
    symlink("real", root.join("link")).expect("relative in-tree symlink");

    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    let entered = sandbox.enter(std::ffi::OsStr::new("link"));

    if crate::linux_capabilities::openat2_supported() {
        entered.expect("in-tree symlinked subdirectory must be descended");
        assert_eq!(sandbox.depth(), 1);
        sandbox.exit();
    } else {
        let err = entered.expect_err("the O_NOFOLLOW fallback cannot follow it");
        let code = err.raw_os_error();
        assert!(
            code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
            "the fallback refusal is ELOOP or ENOTDIR (BSD orders O_DIRECTORY first); got {err:?}"
        );
    }
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
        code == Some(libc::EXDEV)
            || code == Some(libc::ELOOP)
            || code == Some(libc::ENOENT)
            || code == Some(libc::ENOTDIR),
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
/// The `NoExclude` arm delegates resolution to the kernel, so what it does
/// with an in-tree relative symlink is a property of the *mechanism actually
/// available*, not a single fixed rule:
///
/// - `openat2(RESOLVE_BENEATH)` follows it, matching upstream `ds_descend()`;
/// - the portable `openat(O_NOFOLLOW)` fallback refuses it, which
///   [`openat_dir`](super::openat_dir) documents as stricter than upstream and
///   tracked as the portable-fallback gap.
///
/// ⚠ The fallback arm asserts a **known divergence from upstream, not intended
/// behaviour**. Upstream 3.5.0 follows a relative in-tree target
/// (`ds_descend`, `syscall.c:2961`); oc refuses it wherever `openat2` is
/// unavailable, so every non-Linux target is stricter than the reference
/// implementation. Bringing the fallback to parity is task 551 (U350-4i,
/// cross-platform parity for the resolver), where the macOS, BSD and Windows
/// stories get decided together. This test is the ledger entry for that gap -
/// pinning it silently is the failure mode; naming it is what makes it a
/// record rather than an endorsement.
///
/// Both are pinned here, keyed on the same runtime predicate the code
/// consults. An earlier revision asserted only the first, which made this test
/// fail on every non-Linux target and on any Linux kernel without `openat2` -
/// a green Linux run said nothing about either.
///
/// # Upstream Reference
///
/// - `syscall.c:2891` `ds_descend()` - follows a relative in-tree target.
#[test]
fn operator_trusted_policy_resolution_matches_the_available_mechanism() {
    let (_guard, root) = canonical_tempdir();
    std::fs::create_dir(root.join("real")).expect("mkdir real");
    std::fs::write(root.join("real").join("marker"), b"x").expect("write marker");
    symlink("real", root.join("sub")).expect("symlink sub -> real");

    let walked = DirSandbox::open_dest_anchor_with_policy(
        &root,
        std::path::Path::new("sub"),
        super::ConfinePolicy::operator_trusted(),
    );

    if crate::linux_capabilities::openat2_supported() {
        let sandbox = walked.expect("openat2 RESOLVE_BENEATH must follow an in-tree symlink");
        sandbox
            .lstat_at(std::ffi::OsStr::new("marker"))
            .expect("the sandbox must be anchored at real/, reached through sub");
    } else {
        let err = walked
            .expect_err("the O_NOFOLLOW fallback refuses a symlink the kernel walk would follow");
        let code = err.raw_os_error();
        assert!(
            code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
            "the fallback refusal is ELOOP or ENOTDIR (BSD orders O_DIRECTORY first); got {err:?}"
        );
    }

    // Whichever mechanism is in play, the CONFINED arm follows it - that arm
    // resolves each component itself and does not depend on `openat2`. Without
    // this the test would pass on a fallback host while saying nothing about
    // the resolver 603-607 will actually route onto.
    let confined = DirSandbox::open_dest_anchor_confined(
        &root,
        std::path::Path::new("sub"),
        super::ConfinePolicy::confined(ExcludeNothing),
    )
    .expect("the confined walk follows an in-tree relative symlink on every platform");
    confined
        .lstat_at(std::ffi::OsStr::new("marker"))
        .expect("the confined walk must land in real/");
}

/// Builds a chain of `len` directory symlinks under `dir` named
/// `<stem>0 -> <stem>1 -> ... -> <stem>N -> <target>`, and returns the head
/// name. Each link is relative, because an absolute target is refused in every
/// configuration (upstream `syscall.c:2953`) and would make the fixture pass
/// for the wrong reason.
fn symlink_chain(dir: &std::path::Path, stem: &str, len: usize, target: &str) -> String {
    for i in 0..len {
        let next = if i + 1 == len {
            target.to_owned()
        } else {
            format!("{stem}{}", i + 1)
        };
        symlink(&next, dir.join(format!("{stem}{i}"))).expect("symlink chain link");
    }
    format!("{stem}0")
}

/// An oracle that excludes nothing but still selects the manual walk, so a
/// test can exercise the resolver without also depending on a confinement
/// decision.
#[derive(Debug, Clone, Copy)]
struct ExcludeNothing;

impl super::ConfinementOracle for ExcludeNothing {
    fn outside_confinement(&self, _abspath: &std::path::Path) -> bool {
        false
    }
}

#[test]
fn the_symlink_hop_budget_is_shared_across_the_whole_walk() {
    // Two components, each a 25-link chain: 50 hops in total, but neither
    // component alone exceeds the 40-hop budget.
    //
    // Upstream spends ONE budget for the whole walk - `ds_walk_path` takes
    // `hops` by pointer (`rsync-3.5.0/syscall.c:2966`) and `ds_descend`
    // decrements through it - so this must be refused. A budget reset per
    // component, or per descend, would let it through, which is the bug this
    // test exists to catch.
    let (_guard, root) = canonical_tempdir();
    std::fs::create_dir(root.join("a")).expect("mkdir a");
    std::fs::create_dir(root.join("a").join("b")).expect("mkdir a/b");

    let head_a = symlink_chain(&root, "la", 25, "a");
    let head_b = symlink_chain(&root.join("a"), "lb", 25, "b");

    let err = DirSandbox::open_dest_anchor_confined(
        &root,
        &std::path::Path::new(&head_a).join(&head_b),
        super::ConfinePolicy::confined(ExcludeNothing),
    )
    .expect_err("50 cumulative symlink hops must exhaust the shared 40-hop budget");

    assert_eq!(
        err.raw_os_error(),
        Some(libc::ELOOP),
        "an exhausted hop budget is reported as ELOOP, as upstream does at \
         syscall.c:2955; got {err:?}"
    );
}

#[test]
fn a_chain_within_the_hop_budget_still_resolves() {
    // The control for the test above. Without it, an implementation that
    // refused every symlink chain - or every chain longer than one - would
    // satisfy the budget assertion while being wholly wrong, and the pair
    // would look like a passing suite.
    let (_guard, root) = canonical_tempdir();
    std::fs::create_dir(root.join("real")).expect("mkdir real");
    std::fs::write(root.join("real").join("marker"), b"x").expect("write marker");

    let head = symlink_chain(&root, "ok", 39, "real");

    let sandbox = DirSandbox::open_dest_anchor_confined(
        &root,
        std::path::Path::new(&head),
        super::ConfinePolicy::confined(ExcludeNothing),
    )
    .expect("39 hops is within the 40-hop budget and must resolve");

    sandbox
        .lstat_at(std::ffi::OsStr::new("marker"))
        .expect("the walk must land in real/, reached through the chain");
}

/// Excludes any resolved path whose final component is `hidden`.
#[derive(Debug, Clone, Copy)]
struct ExcludeHidden;

impl super::ConfinementOracle for ExcludeHidden {
    fn outside_confinement(&self, abspath: &std::path::Path) -> bool {
        abspath.file_name() == Some(std::ffi::OsStr::new("hidden"))
    }
}

/// Plants `visible/`, `hidden/`, and `visible/link -> ../hidden`: an in-tree
/// relative symlink whose *nominal* path stays inside `visible/` while its
/// *resolved* path lands in `hidden/`.
fn tree_with_redirect_into_hidden(root: &std::path::Path) {
    std::fs::create_dir(root.join("visible")).expect("mkdir visible");
    std::fs::create_dir(root.join("hidden")).expect("mkdir hidden");
    std::fs::write(root.join("hidden").join("marker"), b"x").expect("write marker");
    symlink("../hidden", root.join("visible").join("link")).expect("symlink into hidden");
}

#[test]
fn the_oracle_refuses_a_symlink_that_redirects_into_an_excluded_subtree() {
    // The whole reason the oracle arm exists. `RESOLVE_BENEATH` would allow
    // this: the target never leaves the anchor, so the kernel has no objection,
    // and the only path a BENEATH caller could test is the nominal
    // `visible/link` - which is exactly what the symlink defeats.
    //
    // upstream: rsync-3.5.0/syscall.c:2914-2919, where ds_descend() consults
    // abspath_outside_confinement() on the path it has tracked per component.
    let (_guard, root) = canonical_tempdir();
    tree_with_redirect_into_hidden(&root);

    let err = DirSandbox::open_dest_anchor_confined(
        &root,
        std::path::Path::new("visible/link"),
        super::ConfinePolicy::confined(ExcludeHidden),
    )
    .expect_err("a symlink resolving into an excluded subtree must be refused");

    assert_eq!(
        err.raw_os_error(),
        Some(libc::ELOOP),
        "upstream reports the exclude-aware refusal as ELOOP; got {err:?}"
    );
}

#[test]
fn the_same_redirect_resolves_when_nothing_is_excluded() {
    // The control. Without it the test above would also pass if the walk
    // refused this symlink for some unrelated reason - a relative target, a
    // `..` in the target, or symlinks generally - and would then be pinning
    // the wrong mechanism entirely.
    let (_guard, root) = canonical_tempdir();
    tree_with_redirect_into_hidden(&root);

    let sandbox = DirSandbox::open_dest_anchor_confined(
        &root,
        std::path::Path::new("visible/link"),
        super::ConfinePolicy::confined(ExcludeNothing),
    )
    .expect("with nothing excluded the identical walk must succeed");

    sandbox
        .lstat_at(std::ffi::OsStr::new("marker"))
        .expect("the walk must land in hidden/, reached through visible/link");
}

#[test]
fn dot_dot_is_a_movement_within_the_tree_not_a_refused_component() {
    // `..` is relocated, not dissolved: it pops to the pinned parent dirfd.
    // The BENEATH arm refuses it outright, which is correct there because the
    // kernel resolves the path; here the walk holds the stack and must move.
    //
    // upstream: rsync-3.5.0/syscall.c:2896-2901
    let (_guard, root) = canonical_tempdir();
    std::fs::create_dir(root.join("a")).expect("mkdir a");
    std::fs::create_dir(root.join("b")).expect("mkdir b");
    std::fs::write(root.join("b").join("marker"), b"x").expect("write marker");

    let sandbox = DirSandbox::open_dest_anchor_confined(
        &root,
        std::path::Path::new("a/../b"),
        super::ConfinePolicy::confined(ExcludeNothing),
    )
    .expect("a/../b stays inside the anchor and must resolve to b");

    sandbox
        .lstat_at(std::ffi::OsStr::new("marker"))
        .expect("the walk must land in b/");
}

#[test]
fn dot_dot_above_the_anchor_is_refused() {
    // The other half of the same rule, and the one that makes `..`-as-movement
    // safe: the stack is empty at the anchor, so there is no parent to pop to.
    //
    // upstream: rsync-3.5.0/syscall.c:2897-2899 reports ELOOP rather than
    // handing back the anchor's own parent.
    let (_guard, root) = canonical_tempdir();
    std::fs::create_dir(root.join("a")).expect("mkdir a");

    let err = DirSandbox::open_dest_anchor_confined(
        &root,
        std::path::Path::new("a/../.."),
        super::ConfinePolicy::confined(ExcludeNothing),
    )
    .expect_err("a `..` that rises above the anchor must be refused");

    assert_eq!(
        err.raw_os_error(),
        Some(libc::ELOOP),
        "rising above the anchor is reported as ELOOP; got {err:?}"
    );
}

#[test]
fn an_absolute_symlink_target_is_refused_even_when_it_points_back_inside() {
    // Upstream refuses an absolute target unconditionally
    // (rsync-3.5.0/syscall.c:2953). Pointing it back *inside* the anchor is
    // what makes this test non-vacuous: a walk that only refused escapes would
    // accept it, so this pins the rule rather than its usual consequence.
    let (_guard, root) = canonical_tempdir();
    std::fs::create_dir(root.join("real")).expect("mkdir real");
    symlink(root.join("real"), root.join("abs")).expect("absolute symlink");

    let err = DirSandbox::open_dest_anchor_confined(
        &root,
        std::path::Path::new("abs"),
        super::ConfinePolicy::confined(ExcludeNothing),
    )
    .expect_err("an absolute symlink target must be refused even when in-tree");

    assert_eq!(
        err.raw_os_error(),
        Some(libc::ELOOP),
        "an absolute target is reported as ELOOP; got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Task 602: the residual cases from the resolver test plan
// (`docs/design/path-confinement-resolver-api.md` section 8). The cases the
// walk already pins live above; these are the four it did not.
// ---------------------------------------------------------------------------

/// An oracle that counts how many times it is consulted.
///
/// Existence is the point: "the operator-trusted walk pays nothing" is a claim
/// about calls *not made*, which no assertion on the walk's return value can
/// distinguish from an oracle that ran and answered `false`.
#[derive(Default, Clone)]
struct CountingOracle {
    calls: std::rc::Rc<std::cell::Cell<usize>>,
}

impl CountingOracle {
    /// A clone shares the counter, so the caller can still read it after the
    /// oracle itself has been moved into the policy.
    fn calls(&self) -> usize {
        self.calls.get()
    }
}

impl super::ConfinementOracle for CountingOracle {
    fn outside_confinement(&self, _abspath: &std::path::Path) -> bool {
        self.calls.set(self.calls.get() + 1);
        false
    }
}

/// Spec case 4: a symlink target containing `..` is *walked*, not collapsed.
///
/// The discriminator is an intermediate component that does not exist.
/// Lexical collapse turns `missing/../real` into `real` and succeeds; a walk
/// opens `missing` first and fails. Upstream splits the spliced target with
/// `strtok_r` and calls `ds_descend()` per token, so the open happens.
///
/// The second half is the control: with the intermediate present the same
/// shape resolves, so the refusal above is attributable to the missing
/// component rather than to `..` in a target being rejected outright.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/syscall.c:2966-2976` `ds_walk_path()` tokenises on `/`
/// - `rsync-3.5.0/syscall.c:2937-2961` the spliced target re-enters the walk
#[test]
fn a_dot_dot_inside_a_symlink_target_is_walked_not_string_collapsed() {
    let (_guard, root) = canonical_tempdir();
    std::fs::create_dir(root.join("real")).expect("mkdir real");
    std::fs::create_dir(root.join("present")).expect("mkdir present");
    symlink("missing/../real", root.join("collapsing")).expect("symlink via missing");
    symlink("present/../real", root.join("walking")).expect("symlink via present");

    let err = DirSandbox::open_dest_anchor_confined(
        &root,
        std::path::Path::new("collapsing"),
        super::ConfinePolicy::confined(ExcludeNothing),
    )
    .expect_err("a walked target must open `missing` and fail; collapsing would succeed");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "the absent intermediate must surface as ENOENT; got {err:?}"
    );

    DirSandbox::open_dest_anchor_confined(
        &root,
        std::path::Path::new("walking"),
        super::ConfinePolicy::confined(ExcludeNothing),
    )
    .expect("the identical shape with a real intermediate must resolve");
}

/// Raise this process's soft `RLIMIT_NOFILE` toward `wanted`, returning the
/// soft limit actually in force afterwards.
///
/// `DS_MAXDEPTH` is 1024, and the common Linux default soft limit is *also*
/// 1024 - so a depth-ceiling test dies of `EMFILE` long before it reaches the
/// ceiling and measures fd exhaustion instead of the thing it is named for.
/// macOS defaults to 1048576, which is why this was invisible there and
/// surfaced only when the suite was first run on Linux.
///
/// `setrlimit(2)` refuses a soft limit above the hard limit, so the request is
/// clamped to it rather than failing. nextest runs each test in its own
/// process, so the change is local to this one.
#[cfg(unix)]
fn raise_nofile_soft_limit(wanted: libc::rlim_t) -> libc::rlim_t {
    // Everything stays in `libc::rlim_t` rather than converting to `u64`.
    // It is `u64` on Linux and macOS - which is why a `u64` cast reads as
    // redundant here and clippy rejects it - but it is `i64` on FreeBSD, so
    // neither an `as` cast nor `u64::from` is portable. Not converting at all
    // is.
    //
    // SAFETY: both calls hand the kernel a pointer to a fully initialised,
    // stack-owned `libc::rlimit` plus the matching resource constant, which is
    // the documented `getrlimit(2)` / `setrlimit(2)` ABI. Neither retains the
    // pointer past return, and the value is read only after a success.
    #[allow(unsafe_code)]
    unsafe {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
            return 0;
        }
        let target = wanted.min(limit.rlim_max);
        if limit.rlim_cur < target {
            let raised = libc::rlimit {
                rlim_cur: target,
                rlim_max: limit.rlim_max,
            };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &raised) == 0 {
                return target;
            }
        }
        limit.rlim_cur
    }
}

/// Spec case 6: the depth ceiling refuses rather than truncating.
///
/// Truncation is the dangerous failure: a walk that silently stopped early
/// would hand back a dirfd for an *ancestor* of the requested path, and every
/// later `*at` call would then operate on the wrong directory while looking
/// successful. Refusing with `ENOMEM` is the only safe answer.
///
/// The tree is built with `mkdirat`/`openat` on a moving dirfd rather than a
/// long absolute path: `PATH_MAX` is 1024 on macOS, so a 1024-deep absolute
/// path cannot be created at all through the path-based API. The walk itself
/// only ever sees single components, so it is unaffected.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/syscall.c:2801` `DS_MAXDEPTH`
/// - `rsync-3.5.0/syscall.c:2865-2868` `ds_push()` returns `ENOMEM` at the cap
#[test]
fn the_depth_ceiling_refuses_rather_than_truncating() {
    use std::os::fd::{AsFd, OwnedFd};

    let (_guard, root) = canonical_tempdir();
    let depth = super::DS_MAXDEPTH;

    // The fixture holds one dirfd per level and the walk opens its own set, so
    // reaching the ceiling needs roughly 2 * DS_MAXDEPTH descriptors. Raise
    // the soft limit first - see `raise_nofile_soft_limit`.
    let needed = (depth as libc::rlim_t + 1) * 2 + 64;
    let available = raise_nofile_soft_limit(needed);
    assert!(
        available >= needed,
        "this test needs {needed} file descriptors to reach the depth ceiling, \
         but the hard RLIMIT_NOFILE caps the soft limit at {available}; \
         below that the walk dies of EMFILE before it ever reports ENOMEM"
    );

    // Build `depth` nested single-character directories off a moving dirfd.
    // The parents are retained so teardown can walk back up without recursing.
    let name = std::ffi::OsStr::new("d");
    let mut chain: Vec<OwnedFd> = vec![OwnedFd::from(
        std::fs::File::open(&root).expect("open tempdir root as dirfd"),
    )];
    for _ in 0..depth {
        let cur = chain.last().expect("chain is never empty").as_fd();
        super::at_syscalls::mkdirat(cur, name, 0o700).expect("mkdirat");
        let next = super::at_syscalls::openat(
            cur,
            name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            0,
        )
        .expect("openat child");
        chain.push(OwnedFd::from(next));
    }

    let too_deep: std::path::PathBuf = std::iter::repeat_n("d", depth).collect();
    let err = DirSandbox::open_dest_anchor_confined(
        &root,
        &too_deep,
        super::ConfinePolicy::confined(ExcludeNothing),
    )
    .expect_err("a path at the ceiling must be refused, never silently truncated");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOMEM),
        "the ceiling is reported as ENOMEM; got {err:?}"
    );

    // The control: one below the cap resolves, so the refusal above is the
    // ceiling and not some unrelated limit reached along the way.
    let deep_enough: std::path::PathBuf = std::iter::repeat_n("d", depth - 2).collect();
    DirSandbox::open_dest_anchor_confined(
        &root,
        &deep_enough,
        super::ConfinePolicy::confined(ExcludeNothing),
    )
    .expect("a path just under the ceiling must resolve");

    // Tear the chain down deepest-first through the retained dirfds.
    // `TempDir`'s own cleanup calls `remove_dir_all`, which recurses once per
    // level and overflows the stack at this depth - the tree has to be gone
    // before the guard drops.
    chain.pop();
    while let Some(parent) = chain.pop() {
        super::at_syscalls::unlinkat(parent.as_fd(), name, super::at_syscalls::UnlinkFlags::Dir)
            .expect("rmdir one level");
    }
}

/// Spec case 8: an operator-trusted walk consults the oracle zero times.
///
/// Upstream leaves `ds.abspath` unseeded for a caller with nothing to exclude,
/// and notes such callers "pay nothing". oc mirrors that by seeding the
/// tracker only for an absolute anchor and gating the exclude check on a
/// non-empty tracker, so a relative anchor must produce no calls at all.
///
/// A counting oracle is the only instrument that can see this: an oracle that
/// ran and returned `false` yields exactly the same walk result. This is the
/// "a mutation that kills nothing is informative" shape stated as a test -
/// the property is a call *not made*, so no assertion on the return value can
/// distinguish the two, and only counting can.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/syscall.c:2989-2991` non-daemon callers "pay nothing"
#[test]
fn an_operator_trusted_walk_never_consults_the_oracle() {
    let (_guard, root) = canonical_tempdir();
    std::fs::create_dir_all(root.join("a/b")).expect("mkdir a/b");

    let counting = CountingOracle::default();
    let counting_probe = counting.clone();
    let cwd = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&root).expect("chdir into the anchor");
    let walked = DirSandbox::open_dest_anchor_confined(
        std::path::Path::new("."),
        std::path::Path::new("a/b"),
        super::ConfinePolicy::confined(counting),
    );
    std::env::set_current_dir(cwd).expect("restore cwd");
    walked.expect("a relative anchor must still resolve its tail");

    assert_eq!(
        counting_probe.calls(),
        0,
        "an unseeded tracker must skip the exclude check entirely"
    );

    // The control: with an absolute anchor the tracker is seeded and the same
    // two components DO consult the oracle. Without this the assertion above
    // would also hold for an oracle that is never wired up at all.
    let seeded = CountingOracle::default();
    let seeded_probe = seeded.clone();
    DirSandbox::open_dest_anchor_confined(
        &root,
        std::path::Path::new("a/b"),
        super::ConfinePolicy::confined(seeded),
    )
    .expect("absolute anchor must resolve");
    assert_eq!(
        seeded_probe.calls(),
        2,
        "a seeded tracker consults the oracle once per descended component"
    );
}

/// Spec case 9: the handle outlives the sandbox.
///
/// Task 596 settled that the resolver must hand back a *retained anchor
/// handle*, because consumers hold it across operations that outlive the walk.
/// If `DirSandbox`'s drop closed the descriptor, the surviving `Arc` would name
/// a closed fd - and, worse, a number the kernel is free to reissue, so a later
/// `*at` call could silently address an unrelated file rather than failing.
#[test]
fn the_anchor_handle_outlives_the_sandbox_that_produced_it() {
    use std::os::fd::AsFd;

    let (_guard, root) = canonical_tempdir();
    std::fs::create_dir(root.join("leaf")).expect("mkdir leaf");
    std::fs::write(root.join("leaf/marker"), b"x").expect("write marker");

    let sandbox = DirSandbox::open_dest_anchor_confined(
        &root,
        std::path::Path::new("leaf"),
        super::ConfinePolicy::confined(ExcludeNothing),
    )
    .expect("walk to leaf");
    let handle = sandbox.root_arc();
    drop(sandbox);

    super::at_syscalls::fstatat_nofollow(handle.as_fd(), std::ffi::OsStr::new("marker"))
        .expect("the retained handle must still address the walked directory");
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

/// The lowest descriptor number the kernel would hand out right now.
///
/// `open(2)` always returns the lowest free number, so opening and closing
/// one probe file names the number the *next* allocation will take. Setting
/// `RLIMIT_NOFILE` to that number plus one therefore admits exactly one
/// further descriptor.
fn next_free_fd() -> u64 {
    let probe = tempfile::tempfile().expect("probe descriptor");
    let raw = probe.as_raw_fd();
    assert!(raw >= 0, "probe descriptor must be valid");
    raw as u64
}

/// The *confined* peer-tail walk must emit the descriptor-exhaustion hint
/// too, and must stay silent when the same open fails for an ordinary
/// reason.
///
/// `ConfinedWalk::descend` is the walk that can genuinely exhaust the
/// descriptor table: it holds one dirfd per live path component, so a deep
/// peer tail costs a descriptor per level where a single path-based
/// `open()` costs one in total. Upstream warns from exactly that function -
/// the `EMFILE`/`ENFILE` arm sits inside `ds_descend()`, not at the anchor
/// open - so a silent `descend` was the divergence, while the pre-existing
/// emit in `openat_dir` covers only the unconfined sibling walk.
///
/// Three cells, each failing for a different reason:
///
/// 1. A walk that resolves proves the fixture is a *working* confined walk
///    rather than one failing for an unrelated reason, and warms any
///    one-shot capability probe before the ceiling drops.
/// 2. A walk refused with `ENOENT` is the negative control. It leaves
///    `descend` through the *same* `Err(err)` arm the hint now hangs off,
///    so widening [`should_warn_fd_exhaustion`](super::should_warn_fd_exhaustion)
///    to fire on any error - the mutation a positives-only suite cannot
///    see - turns this leg red.
/// 3. The measured run pins both halves of the emit: the hint appears
///    exactly once, and the caller still observes `EMFILE` rather than an
///    errno re-derived after the warning was printed.
///
/// Both the `RLIMIT_NOFILE` change and the stderr redirect are
/// process-global. That is safe only because nextest runs each test in its
/// own process. Every descriptor the measured run needs is allocated
/// *before* the limit drops, and both globals are restored before any
/// assertion runs so a failing assert can still allocate for its output.
///
/// upstream: syscall.c:2924-2936, inside `ds_descend()` (`syscall.c:2891`).
#[test]
fn confined_walk_warns_on_fd_exhaustion_and_stays_silent_on_enoent() {
    use rustix::io::{Errno, dup};
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
    use rustix::stdio::{dup2_stderr, stderr};
    use std::io::{Read, Seek, SeekFrom};
    use std::path::Path;

    let (_keep, anchor) = canonical_tempdir();
    let peer_tail = Path::new("archive/2026/hosts");
    std::fs::create_dir_all(anchor.join(peer_tail)).expect("build peer tail");

    // Controls, both under the ambient limit, both required to be silent.
    let mut control_capture = tempfile::tempfile().expect("control capture file");
    let saved_stderr = dup(stderr()).expect("save stderr");
    dup2_stderr(&control_capture).expect("redirect stderr");
    let resolved = DirSandbox::open_dest_anchor_confined(
        &anchor,
        peer_tail,
        super::ConfinePolicy::confined(ExcludeNothing),
    );
    let missing = DirSandbox::open_dest_anchor_confined(
        &anchor,
        Path::new("archive/absent"),
        super::ConfinePolicy::confined(ExcludeNothing),
    );
    dup2_stderr(&saved_stderr).expect("restore stderr");

    let mut control_stderr = String::new();
    control_capture
        .seek(SeekFrom::Start(0))
        .expect("rewind control capture");
    control_capture
        .read_to_string(&mut control_stderr)
        .expect("read control stderr");

    resolved.expect("the confined walk must resolve an existing tail under the ambient limit");
    let missing = missing.expect_err("a peer tail component that does not exist must fail");
    assert_eq!(
        missing.raw_os_error(),
        Some(Errno::NOENT.raw_os_error()),
        "the negative control must leave `descend` with ENOENT, not some other errno"
    );
    assert_eq!(
        control_stderr, "",
        "neither a resolving walk nor an ordinary ENOENT may print the \
         descriptor-exhaustion hint; got: {control_stderr:?}"
    );

    // Every descriptor the measured run needs is allocated here, before the
    // ceiling drops.
    let mut captured = tempfile::tempfile().expect("stderr capture file");
    let saved_stderr = dup(stderr()).expect("save stderr");
    let original = getrlimit(Resource::Nofile);

    // One descriptor of headroom: `open_trusted_dir` is a single `open()`
    // and takes it, so the first `descend` component is the allocation the
    // kernel refuses.
    let ceiling = next_free_fd() + 1;

    dup2_stderr(&captured).expect("redirect stderr");
    let lowered = setrlimit(
        Resource::Nofile,
        Rlimit {
            current: Some(ceiling),
            maximum: original.maximum,
        },
    );
    let observed = getrlimit(Resource::Nofile).current;

    // Gate the measurement on the drop actually landing. `setrlimit` refuses
    // a request above `rlim_max`, and running the walk regardless would turn
    // a failed drop into a silent pass.
    let outcome = (lowered.is_ok() && observed == Some(ceiling)).then(|| {
        DirSandbox::open_dest_anchor_confined(
            &anchor,
            peer_tail,
            super::ConfinePolicy::confined(ExcludeNothing),
        )
    });

    setrlimit(Resource::Nofile, original).expect("restore RLIMIT_NOFILE");
    dup2_stderr(&saved_stderr).expect("restore stderr");

    let mut emitted = String::new();
    captured.seek(SeekFrom::Start(0)).expect("rewind capture");
    captured
        .read_to_string(&mut emitted)
        .expect("read captured stderr");

    assert_eq!(
        observed,
        Some(ceiling),
        "the harness could not lower RLIMIT_NOFILE to {ceiling} \
         (setrlimit: {lowered:?}, original: {original:?}); nothing below this \
         point would have been exercised"
    );
    let outcome = outcome.expect("the measurement is gated on the drop above");

    let err = outcome.expect_err(
        "the confined walk completed with one descriptor of headroom; no \
         descriptor pressure was applied, so this cell asserted nothing",
    );
    assert_eq!(
        err.raw_os_error(),
        Some(Errno::MFILE.raw_os_error()),
        "the caller must still see EMFILE, not an error re-derived after the hint"
    );
    assert_eq!(
        emitted.matches(super::FD_EXHAUSTION_HINT).count(),
        1,
        "the confined walk must print the hint exactly once under \
         RLIMIT_NOFILE={ceiling}; got: {emitted:?}"
    );
}

/// The peer-tail anchor walk cannot exhaust descriptors; the `enter()`
/// descent that shares its open helper can.
///
/// Both walks go through [`openat_dir`](super::openat_dir), so the
/// descriptor-exhaustion emit inside it appears to hang off
/// [`open_dest_anchor`](DirSandbox::open_dest_anchor) - a walk that rebinds a
/// single `OwnedFd` per component and drops the parent immediately, and
/// therefore costs one descriptor no matter how deep the tail is. Reading only
/// that call site, the emit looks unreachable and invites deletion. It is not:
/// [`enter`](DirSandbox::enter) retains a frame per level, so it is the caller
/// that reaches the ceiling, and the emit is load-bearing for it.
///
/// Two cells at the *same* depth under the *same* ceiling, so depth and limit
/// are held fixed and only the descriptor discipline varies:
///
/// 1. The anchor walk resolves a 64-component tail with three descriptors of
///    headroom, and prints nothing. This is both the proof of constant cost
///    and the negative control for the emit: it is the same `openat_dir` code
///    path, succeeding, and it must stay silent.
/// 2. The identical descent through `enter()` fails with `EMFILE` before it
///    reaches 64 and prints the hint exactly once.
///
/// Cell 1 failing means the anchor walk started retaining descriptors - a
/// behaviour change worth catching on its own, since the peer controls the
/// tail's depth. Cell 2 failing means the emit stopped firing for the walk
/// that needs it.
///
/// Both the `RLIMIT_NOFILE` change and the stderr redirect are process-global,
/// which is safe only because nextest runs each test in its own process. Every
/// descriptor either cell needs is allocated before the ceiling drops - the
/// `openat2` capability probe included, which is a `OnceLock` and is warmed
/// deliberately - and both globals are restored before any assertion so a
/// failing assert can still allocate for its output.
///
/// upstream: syscall.c:2797-2800 - "The walk holds one fd per component, so
/// depth is bounded by RLIMIT_NOFILE anyway"; syscall.c:2924-2936 - the hint,
/// which upstream places on the accumulating walk alone.
#[test]
fn the_anchor_walk_cannot_exhaust_but_the_entered_walk_can() {
    use rustix::io::{Errno, dup};
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
    use rustix::stdio::{dup2_stderr, stderr};
    use std::io::{Read, Seek, SeekFrom};

    // Deep enough that a per-component descriptor cost cannot fit under the
    // ceiling, shallow enough to stay well inside macOS's 1024-byte PATH_MAX.
    const DEPTH: usize = 64;

    let (_keep, anchor) = canonical_tempdir();
    let deep: std::path::PathBuf = std::iter::repeat_n("a", DEPTH).collect();
    std::fs::create_dir_all(anchor.join(&deep)).expect("build the deep peer tail");

    // Warm the one-shot `openat2` capability probe under the ambient limit.
    // It is a `OnceLock` that costs a descriptor on its first call, and it
    // would otherwise fire inside the measurement and cache a verdict reached
    // under descriptor pressure.
    DirSandbox::open_dest_anchor(&anchor, std::path::Path::new("a"))
        .expect("the warm-up resolve must succeed under the ambient limit");

    let mut captured = tempfile::tempfile().expect("stderr capture file");
    let saved_stderr = dup(stderr()).expect("save stderr");
    let original = getrlimit(Resource::Nofile);

    // Three descriptors of headroom. The anchor walk's peak is two (the
    // parent is still borrowed while its child is opened), so it fits at any
    // depth; a walk that retains a frame per level runs out almost at once.
    let ceiling = next_free_fd() + 3;

    dup2_stderr(&captured).expect("redirect stderr");
    let lowered = setrlimit(
        Resource::Nofile,
        Rlimit {
            current: Some(ceiling),
            maximum: original.maximum,
        },
    );
    let observed = getrlimit(Resource::Nofile).current;

    // Gate both cells on the drop actually landing: `setrlimit` refuses a
    // request above `rlim_max`, and running anyway would turn a failed drop
    // into a silent pass.
    let dropped = lowered.is_ok() && observed == Some(ceiling);

    let anchor_walk = dropped.then(|| DirSandbox::open_dest_anchor(&anchor, &deep));
    let entered = dropped.then(|| {
        DirSandbox::open_root(&anchor).and_then(|mut sandbox| {
            for _ in 0..DEPTH {
                sandbox.enter(std::ffi::OsStr::new("a"))?;
            }
            Ok(sandbox.depth())
        })
    });

    setrlimit(Resource::Nofile, original).expect("restore RLIMIT_NOFILE");
    dup2_stderr(&saved_stderr).expect("restore stderr");

    let mut emitted = String::new();
    captured.seek(SeekFrom::Start(0)).expect("rewind capture");
    captured
        .read_to_string(&mut emitted)
        .expect("read captured stderr");

    assert_eq!(
        observed,
        Some(ceiling),
        "the harness could not lower RLIMIT_NOFILE to {ceiling} \
         (setrlimit: {lowered:?}, original: {original:?}); no descriptor \
         pressure was applied and nothing below this point was exercised"
    );

    anchor_walk
        .expect("the cells are gated on the drop above")
        .unwrap_or_else(|err| {
            panic!(
                "the anchor walk must resolve a {DEPTH}-component tail under \
                 RLIMIT_NOFILE={ceiling}; it costs one descriptor regardless of \
                 depth. Failing with {err:?} means it now retains a descriptor \
                 per component, which the peer controls the depth of"
            )
        });

    let err = match entered.expect("the cells are gated on the drop above") {
        Err(err) => err,
        Ok(depth) => panic!(
            "`enter()` holds a frame per level, so {DEPTH} levels cannot fit \
             under RLIMIT_NOFILE={ceiling}; reaching depth {depth} means no \
             descriptor pressure was applied and the emit was never reached"
        ),
    };
    assert_eq!(
        err.raw_os_error(),
        Some(Errno::MFILE.raw_os_error()),
        "the accumulating descent must surface EMFILE to its caller, not an \
         errno re-derived after the hint was printed; got {err:?}"
    );

    assert_eq!(
        emitted.matches(super::FD_EXHAUSTION_HINT).count(),
        1,
        "exactly one hint is expected: none from the anchor walk, which \
         succeeded, and one from the descent that exhausted; got: {emitted:?}"
    );
}
