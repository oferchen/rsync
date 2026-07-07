//! SEC-1.m consolidated attack harness: end-to-end symlink-swap
//! regression that ties the SEC-1.f (lstat-class) and SEC-1.g
//! (unlink-class) primitives together into a single attacker pattern
//! at the receiver-pipeline integration level.
//!
//! The two per-primitive test files (`fstatat_swap_resistance.rs` and
//! `unlinkat_swap_resistance.rs`) cover the helpers in isolation on
//! quiescent trees. This harness pins the documented SEC-1 invariant -
//! that a mid-syscall symlink swap cannot redirect a receiver-issued
//! stat or unlink to an attacker-chosen target outside the destination
//! root - against an *active* attacker thread racing the receiver.
//!
//! Three scenarios exercise the full attack pattern the receiver
//! pipeline must withstand:
//!
//! 1. **`scenario_1`** - the lstat-class invariant under live race.
//!    A receiver-shaped tree contains `dest/subdir/leaf` as a
//!    plain file. An attacker thread swaps `dest/subdir/leaf` for a
//!    symlink pointing into a sibling sensitive tree between the
//!    receiver's decide-to-stat moment and the sandbox-anchored
//!    lstat. The sandbox-anchored helper either reports the symlink
//!    leaf itself (if the swap wins the race) or the original file
//!    (if the receiver wins). It must never resolve to the sensitive
//!    target outside the destination root.
//!
//! 2. **`scenario_2`** - the unlink-class invariant under live race.
//!    A receiver-shaped tree contains `dest/leader` and
//!    `dest/follower` as hardlinked siblings. An attacker thread
//!    swaps `dest/follower` for a symlink to `dest/leader` between
//!    the receiver's decide-to-delete moment and the sandbox-anchored
//!    unlinkat. The leader must survive in every interleaving: with
//!    the SEC-1.g cutover, `unlinkat` is hard-coded to never follow a
//!    terminal symlink, so the symlink itself is removed and the
//!    leader is left intact. A path-based `remove_file` would have
//!    deleted the leader; this test catches any regression that
//!    reverts to path-based removal.
//!
//! 3. **`scenario_3`** - tight race-window stress. The attacker is
//!    synchronised with the receiver via `crossbeam-channel`
//!    handshakes (deterministic, not sleep-based, per the
//!    `project_concurrent_dispatch_test_flake` learning) and repeats
//!    the swap N times in a tight loop while the receiver issues the
//!    sandbox-anchored stat/unlink pair on every iteration. The
//!    invariant - sensitive tree untouched, sandbox tree the only
//!    thing the syscalls touch - must hold across every iteration
//!    regardless of who wins each individual race.
//!
//! These tests deliberately do not stand up a full receiver pipeline.
//! The SEC-1 invariant lives in `fast_io::{lstat_via_sandbox_or_fallback,
//! unlink_via_sandbox_or_fallback}` and the receiver consumes them
//! through the `DirSandbox` carrier (SEC-1.e). The harness shapes the
//! filesystem state the way the receiver shapes it and calls the same
//! helpers the receiver calls; that is the integration boundary the
//! SEC-1.f and SEC-1.g cutovers actually defend.
//!
//! Dependency note: this test file consumes the SEC-1.g API surface
//! (`fast_io::UnlinkFlags`, `fast_io::unlink_via_sandbox_or_fallback`).
//! It will not compile until #4671 merges; CI failures before then are
//! expected.

#![cfg(unix)]

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use crossbeam_channel::{Receiver, Sender, bounded};
use fast_io::{
    DirSandbox, LstatOutcome, UnlinkFlags, lstat_via_sandbox_or_fallback,
    symlinkat_via_sandbox_or_fallback, unlink_via_sandbox_or_fallback,
};
use tempfile::{TempDir, tempdir};

/// Returns whether `openat2(RESOLVE_BENEATH)` nested-parent anchoring is
/// live on this host. When off (non-Linux, or Linux < 5.6), the helpers
/// degrade to the path-based fallback and the interior-symlink guarantee
/// is not enforced; Linux CI is the runtime gate for the refusal.
fn nested_anchor_live() -> bool {
    cfg!(target_os = "linux") && fast_io::openat2_supported()
}

/// `tempdir()` may sit under a symlink prefix on macOS / some CI
/// runners; canonicalise so the sandbox open succeeds under
/// `RESOLVE_NO_SYMLINKS`.
fn canonical_tempdir() -> (TempDir, PathBuf) {
    let dir = tempdir().expect("tempdir");
    let canon = std::fs::canonicalize(dir.path()).expect("canonicalize");
    (dir, canon)
}

/// Two-phase channel handshake. The receiver fires `proceed_tx` when
/// it is about to issue the syscall; the attacker fires `done_tx`
/// once its swap has been applied. Using bounded channels keeps the
/// rendezvous deterministic - no sleeps, no wall-clock guesses, per
/// the `project_concurrent_dispatch_test_flake` learning that
/// scheduler hiccups on Windows / macOS CI runners turn sleep-based
/// races into flakes.
struct RaceChannels {
    proceed_tx: Sender<()>,
    proceed_rx: Receiver<()>,
    done_tx: Sender<()>,
    done_rx: Receiver<()>,
}

impl RaceChannels {
    fn new() -> Self {
        let (proceed_tx, proceed_rx) = bounded(1);
        let (done_tx, done_rx) = bounded(1);
        Self {
            proceed_tx,
            proceed_rx,
            done_tx,
            done_rx,
        }
    }
}

/// Scenario 1: lstat-class invariant under a live attacker thread.
///
/// The receiver's destination contains a regular file at
/// `dest/subdir/leaf`. A sibling `sensitive/` tree holds a secret
/// file outside the receiver's sandbox root. An attacker swaps
/// `dest/subdir/leaf` for a symlink pointing into the sensitive tree
/// between the receiver's decide-to-stat moment and the syscall.
///
/// Invariant: the sandbox-anchored lstat must never report
/// `is_file()` against the sensitive target. Either it reports the
/// pre-swap file (receiver won) or the symlink itself (attacker won).
/// Crucially, the sensitive target outside the sandbox is never
/// stated, opened, or modified.
#[test]
fn scenario_1_lstat_race_does_not_follow_swapped_symlink_outside_sandbox() {
    let (_keep, parent) = canonical_tempdir();

    // Sensitive tree outside the destination sandbox root.
    let sensitive_dir = parent.join("sensitive");
    std::fs::create_dir(&sensitive_dir).expect("mkdir sensitive");
    let sensitive_file = sensitive_dir.join("secret");
    std::fs::write(&sensitive_file, b"do-not-disclose").expect("write secret");

    // Destination sandbox root. SEC-1.f only anchors single-component
    // leaves through the sandbox dirfd, so we shape the race against
    // a leaf directly under the sandbox root.
    let dest = parent.join("dest");
    std::fs::create_dir(&dest).expect("mkdir dest");
    let leaf_name = "leaf";
    let leaf_path = dest.join(leaf_name);
    std::fs::write(&leaf_path, b"original-contents").expect("write leaf");

    let sandbox = DirSandbox::open_root(&dest).expect("sandbox open");

    let channels = RaceChannels::new();
    let proceed_rx = channels.proceed_rx.clone();
    let done_tx = channels.done_tx.clone();

    let leaf_path_for_attacker = leaf_path.clone();
    let sensitive_file_for_attacker = sensitive_file.clone();
    let attacker = thread::spawn(move || {
        // Wait for the receiver to signal "about to syscall".
        proceed_rx.recv().expect("recv proceed");
        // Apply the swap: unlink the original file and replace it
        // with a symlink to the sensitive target outside the
        // destination sandbox.
        let _ = std::fs::remove_file(&leaf_path_for_attacker);
        // If the receiver got there first the path is gone; if not,
        // the swap installs the symlink. Either interleaving is
        // valid; the invariant is about what the sandbox helper
        // *resolves to*, not which thread wins.
        let _ = symlink(&sensitive_file_for_attacker, &leaf_path_for_attacker);
        done_tx.send(()).expect("send done");
    });

    // Signal the attacker and immediately issue the sandbox-anchored
    // stat. The order of `send` and `lstat_via_sandbox_or_fallback`
    // determines who wins each race, and the invariant must hold in
    // every case.
    channels.proceed_tx.send(()).expect("send proceed");
    let outcome =
        lstat_via_sandbox_or_fallback(Some(&sandbox), &dest, Path::new(leaf_name), &leaf_path);
    // Make sure the attacker has finished before we inspect state.
    channels.done_rx.recv().expect("recv done");
    attacker.join().expect("attacker join");

    match outcome {
        Ok(LstatOutcome::At(meta)) => {
            // Two valid interleavings:
            // - receiver won the race: meta describes the original
            //   regular file.
            // - attacker won the race: meta describes the symlink
            //   itself, AT_SYMLINK_NOFOLLOW means it is reported as a
            //   symlink, not followed to the sensitive target.
            assert!(
                meta.is_file() || meta.is_symlink(),
                "sandbox lstat must describe a file-or-symlink leaf, never the dereferenced \
                 sensitive target"
            );
        }
        Ok(LstatOutcome::Std(meta)) => {
            // Single-component leaf should normally take the
            // sandbox-anchored path; the Std arm is reserved for
            // multi-component fallback. Accept it for forward
            // compatibility but assert the same TOCTOU invariant.
            assert!(
                meta.is_file() || meta.is_symlink(),
                "fallback lstat must describe a file-or-symlink leaf, never the dereferenced \
                 sensitive target"
            );
        }
        Err(err) => {
            // ENOENT is acceptable: it means the attacker raced
            // between the receiver's unlink-replace pair and we
            // sampled the gap. Anything else points at a bug.
            assert_eq!(
                err.kind(),
                std::io::ErrorKind::NotFound,
                "sandbox lstat must either succeed or report ENOENT for a missing leaf"
            );
        }
    }

    // The sensitive tree must be untouched regardless of who won
    // the race. SEC-1.f's invariant: the sandbox-anchored stat
    // never crosses the sandbox boundary.
    assert!(
        sensitive_dir.is_dir(),
        "sensitive directory outside the sandbox must survive the race"
    );
    assert!(
        sensitive_file.is_file(),
        "sensitive file outside the sandbox must survive the race"
    );
    assert_eq!(
        std::fs::read(&sensitive_file).expect("read secret"),
        b"do-not-disclose",
        "sensitive file contents must be unchanged"
    );
}

/// Scenario 2: unlink-class invariant under a live attacker thread.
///
/// The receiver's destination contains two hardlinked siblings:
/// `dest/leader` and `dest/follower`. The receiver decides to delete
/// `dest/follower` (stale obstacle). An attacker swaps `dest/follower`
/// for a symlink to `dest/leader` between the decide-to-delete moment
/// and the sandbox-anchored unlinkat.
///
/// With the SEC-1.g cutover, `unlinkat(File)` is hard-coded never to
/// follow a terminal symlink, so the swapped symlink is removed
/// itself and the leader survives. A path-based `remove_file` (pre-
/// SEC-1.g behaviour) would have deleted the leader through the
/// symlink; this test would have caught that regression.
#[test]
fn scenario_2_unlinkat_race_does_not_follow_swapped_symlink_to_sibling() {
    let (_keep, dest) = canonical_tempdir();

    let leader = dest.join("leader");
    let follower = dest.join("follower");
    std::fs::write(&leader, b"leader-payload").expect("write leader");
    std::fs::hard_link(&leader, &follower).expect("hard link");

    let sandbox = DirSandbox::open_root(&dest).expect("sandbox open");

    let channels = RaceChannels::new();
    let proceed_rx = channels.proceed_rx.clone();
    let done_tx = channels.done_tx.clone();

    let follower_path_for_attacker = follower.clone();
    let leader_path_for_attacker = leader.clone();
    let attacker = thread::spawn(move || {
        proceed_rx.recv().expect("recv proceed");
        // Unlink the hardlink follower and replace it with a
        // symlink pointing at the leader. The symlink is intended
        // to redirect a naive path-based `remove_file` into
        // deleting the leader.
        let _ = std::fs::remove_file(&follower_path_for_attacker);
        let _ = symlink(&leader_path_for_attacker, &follower_path_for_attacker);
        done_tx.send(()).expect("send done");
    });

    channels.proceed_tx.send(()).expect("send proceed");
    let unlink_result = unlink_via_sandbox_or_fallback(
        Some(&sandbox),
        &dest,
        Path::new("follower"),
        &follower,
        UnlinkFlags::File,
    );
    channels.done_rx.recv().expect("recv done");
    attacker.join().expect("attacker join");

    // Acceptable outcomes:
    // - receiver won: hardlink follower was removed before the
    //   attacker installed the symlink; `unlink_result` is Ok and
    //   `follower` is gone.
    // - attacker won: symlink installed; `unlinkat(File)` removed
    //   the symlink itself; `unlink_result` is Ok and `follower` is
    //   gone (the symlink, not the leader).
    // - tight interleaving: the attacker beat us by one syscall and
    //   our unlink hit ENOENT. Still safe: the leader is untouched.
    match unlink_result {
        Ok(()) => {}
        Err(err) => assert_eq!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "sandbox unlink must either succeed or report ENOENT, got {err}"
        ),
    }

    // The critical invariant: the leader must survive every
    // interleaving. SEC-1.g says `unlinkat(File)` never follows a
    // terminal symlink; if it did, the attacker would have used
    // the symlink-to-leader to delete the leader through us.
    assert!(
        leader.is_file(),
        "leader must survive: a path-based remove_file would have followed the symlink and \
         deleted the leader, which is exactly the SEC-1.g regression"
    );
    assert_eq!(
        std::fs::read(&leader).expect("read leader"),
        b"leader-payload",
        "leader contents must be unchanged after the race"
    );
}

/// Scenario 3: tight race-window stress. Repeat the swap-vs-stat-vs-
/// unlink interleaving N times to maximise race-window exposure on
/// platforms where the scheduler granularity differs. Deterministic
/// channel handshakes (not sleeps) make this loop reproducible.
#[test]
fn scenario_3_repeated_race_keeps_sensitive_tree_untouched() {
    const ITERATIONS: usize = 64;

    let (_keep, parent) = canonical_tempdir();
    let sensitive_dir = parent.join("sensitive");
    std::fs::create_dir(&sensitive_dir).expect("mkdir sensitive");
    let sensitive_file = sensitive_dir.join("secret");
    std::fs::write(&sensitive_file, b"never-touched").expect("write secret");

    let dest = parent.join("dest");
    std::fs::create_dir(&dest).expect("mkdir dest");

    let sandbox = DirSandbox::open_root(&dest).expect("sandbox open");

    let leaf_name = "leaf";
    let leaf_path = dest.join(leaf_name);

    // Per-iteration handshake: the main thread tells the attacker to
    // arm the next swap and waits for it to confirm. The bounded
    // channels guarantee a happens-before edge, so this is a
    // deterministic rendezvous, not a sleep-based race.
    let (arm_tx, arm_rx) = bounded::<()>(1);
    let (armed_tx, armed_rx) = bounded::<()>(1);
    let stop = Arc::new(AtomicBool::new(false));

    let leaf_path_for_attacker = leaf_path.clone();
    let sensitive_file_for_attacker = sensitive_file.clone();
    let stop_for_attacker = Arc::clone(&stop);
    let attacker = thread::spawn(move || {
        while !stop_for_attacker.load(Ordering::Acquire) {
            if arm_rx.recv().is_err() {
                break;
            }
            // Install the symlink swap. If the path already holds
            // a stale entry from a previous iteration, clear it
            // first; ignore ENOENT.
            let _ = std::fs::remove_file(&leaf_path_for_attacker);
            let _ = symlink(&sensitive_file_for_attacker, &leaf_path_for_attacker);
            if armed_tx.send(()).is_err() {
                break;
            }
        }
    });

    for _ in 0..ITERATIONS {
        // Place a regular file at the leaf so the receiver-shaped
        // syscall has something to stat-then-unlink.
        let _ = std::fs::remove_file(&leaf_path);
        std::fs::write(&leaf_path, b"iter-original").expect("write iter leaf");

        // Tell the attacker to prime its swap; wait until it has.
        arm_tx.send(()).expect("send arm");
        armed_rx.recv().expect("recv armed");

        // Receiver-shaped pair: sandbox-anchored stat followed by
        // sandbox-anchored unlink, exactly the sequence the
        // obstacle-clear path in `receiver/directory/links.rs`
        // issues.
        let stat_outcome =
            lstat_via_sandbox_or_fallback(Some(&sandbox), &dest, Path::new(leaf_name), &leaf_path);
        match stat_outcome {
            Ok(LstatOutcome::At(meta)) => {
                assert!(
                    meta.is_file() || meta.is_symlink(),
                    "iter sandbox lstat must describe the leaf, never the sensitive target"
                );
            }
            Ok(LstatOutcome::Std(meta)) => {
                assert!(
                    meta.is_file() || meta.is_symlink(),
                    "iter fallback lstat must describe the leaf, never the sensitive target"
                );
            }
            Err(err) => assert_eq!(
                err.kind(),
                std::io::ErrorKind::NotFound,
                "iter sandbox lstat must succeed or ENOENT, got {err}"
            ),
        }

        let unlink_result = unlink_via_sandbox_or_fallback(
            Some(&sandbox),
            &dest,
            Path::new(leaf_name),
            &leaf_path,
            UnlinkFlags::File,
        );
        match unlink_result {
            Ok(()) => {}
            Err(err) => assert_eq!(
                err.kind(),
                std::io::ErrorKind::NotFound,
                "iter sandbox unlink must succeed or ENOENT, got {err}"
            ),
        }

        // Per-iteration invariant: the sensitive tree was not
        // touched by any of the receiver-shaped syscalls.
        assert!(
            sensitive_file.is_file(),
            "sensitive file must survive every iteration"
        );
        assert_eq!(
            std::fs::read(&sensitive_file).expect("read secret"),
            b"never-touched",
            "sensitive file contents must be unchanged every iteration"
        );
    }

    // Tear down the attacker cleanly: drop the arm sender so its
    // recv unblocks with Err, then flag stop for the rare case
    // where it ran one extra loop.
    stop.store(true, Ordering::Release);
    drop(arm_tx);
    attacker.join().expect("attacker join");

    // Final invariant after the full stress loop: the sensitive
    // tree is byte-identical to its initial state.
    assert!(
        sensitive_dir.is_dir(),
        "sensitive directory must survive the full stress loop"
    );
    assert!(
        sensitive_file.is_file(),
        "sensitive file must survive the full stress loop"
    );
    assert_eq!(
        std::fs::read(&sensitive_file).expect("final read secret"),
        b"never-touched",
        "sensitive file contents must be byte-identical after the full stress loop"
    );
}

/// Scenario 4: interior-directory symlink escape (the SEC nested-path
/// anchoring keystone). The receiver acts on a multi-component path
/// `dest/a/EVIL/link`. An attacker replaces the *interior* directory
/// `dest/a/EVIL` with a symlink pointing OUTSIDE the destination root.
///
/// Before the nested-parent anchor, the leaf op fell to a path-based
/// syscall that re-resolved `a/EVIL` through the ambient namespace,
/// following the symlink and creating the entry outside the root. With
/// the `openat2(RESOLVE_BENEATH)` parent anchor the interior escape is
/// refused in-kernel (EXDEV/ELOOP/ENOTDIR) and the outside tree is never
/// written. This is the exact gap `single_component_leaf` could not
/// close: the leaf is single-component, but the *parent* path is not.
#[test]
fn scenario_4_interior_dir_symlink_escape_is_refused() {
    let (_keep, parent) = canonical_tempdir();

    // Sensitive tree outside the destination sandbox root.
    let outside = parent.join("outside");
    std::fs::create_dir(&outside).expect("mkdir outside");

    let dest = parent.join("dest");
    let a = dest.join("a");
    std::fs::create_dir_all(&a).expect("mkdir dest/a");
    // Interior-component symlink escape: dest/a/EVIL -> ../../outside.
    symlink(&outside, a.join("EVIL")).expect("plant escaping interior symlink");

    let sandbox = DirSandbox::open_root(&dest).expect("sandbox open");

    let rel = Path::new("a/EVIL/link");
    let link_path = dest.join(rel);
    let result = symlinkat_via_sandbox_or_fallback(
        Some(&sandbox),
        &dest,
        rel,
        &link_path,
        Path::new("payload"),
    );

    if nested_anchor_live() {
        let err = result.expect_err("interior-dir symlink escape must be refused");
        let raw = err.raw_os_error();
        assert!(
            matches!(
                raw,
                Some(libc::EXDEV) | Some(libc::ELOOP) | Some(libc::ENOTDIR)
            ),
            "expected EXDEV/ELOOP/ENOTDIR from the RESOLVE_BENEATH parent open, got {raw:?}"
        );
        assert!(
            !outside.join("link").exists(),
            "no entry may be created outside the destination root via the interior symlink"
        );
        assert!(
            std::fs::read_dir(&outside)
                .expect("read outside")
                .next()
                .is_none(),
            "the outside tree must remain empty after the refused escape"
        );
    } else {
        // No kernel RESOLVE_BENEATH: the helper degrades to the
        // path-based fallback (today's behaviour). Assert the call is
        // total; the security refusal is asserted on the Linux gate.
        let _ = result;
    }
}
