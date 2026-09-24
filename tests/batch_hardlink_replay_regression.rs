//! `--read-batch -aH` must reconstruct a hardlink cluster even when the member
//! carrying the delta payload is not the one the replaying receiver treats as
//! the cluster leader.
//!
//! Regression guard for the batch hardlink data-loss fix (#7928, "ship hardlink
//! payload under the sorted-first cluster NDX"). A local `--write-batch` builds
//! the flist and delta stream in traversal order, then remaps each delta NDX to
//! the sorted position the replaying receiver assigns after
//! `flist_sort_and_clean()`. The engine's `reorder_hardlink_group_holders`
//! promotes the *name-sorted-last* fresh cohort member to the data-holder, so it
//! is the member captured first and the one that records the delta payload. The
//! replaying receiver, however, flags the *name-sorted-first* member
//! `FLAG_HLINK_FIRST` and expects the payload under its NDX (upstream
//! `hlink.c:113-194 match_gnums()`). Before #7928 the payload shipped under the
//! holder's own sorted NDX, so the real leader received no data, was never
//! written, and the other member was left unlinked - the cluster replayed as a
//! single or missing file at exit 0 (silent data loss).
//!
//! This guard is oracle-free on purpose: it drives oc's own `--write-batch` and
//! `--read-batch`, with no upstream rsync involved, so it always runs its
//! assertions instead of skipping when no oracle is installed. The fix lives on
//! the write path in `crates/engine`, which the `batch` crate cannot reach (it
//! has no `engine` dependency), so the regression can only be pinned by a full
//! write -> read round-trip - hence a binary-level integration test rather than
//! a `crates/batch` unit test.

mod integration;

#[cfg(unix)]
use integration::helpers::{RsyncCommand, TestDir};
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

/// A two-name hardlink cluster whose data-holder is not its sorted-first member
/// must survive a local `--write-batch` -> `--read-batch -aH` round-trip with
/// both names present, both carrying the content, and both sharing one inode.
#[test]
#[cfg(unix)]
fn read_batch_preserves_hardlink_cluster_when_holder_is_not_sorted_first() {
    let test_dir = TestDir::new().expect("create test dir");
    let src = test_dir.mkdir("src").expect("create src");

    // The cohort names are chosen so the sorted-first member ("aaa_leader.txt")
    // is NOT the data-holder. `reorder_hardlink_group_holders` promotes the
    // name-sorted-LAST fresh member ("zzz_holder.txt") to carry the payload, so
    // its traversal position and the leader's sorted position differ - the exact
    // case #7928 fixes. A single-name cohort, or one whose first and last names
    // coincide, would pass even against the pre-#7928 code and prove nothing.
    fs::write(src.join("aaa_leader.txt"), b"linked payload\n").expect("write leader");
    fs::hard_link(src.join("aaa_leader.txt"), src.join("zzz_holder.txt")).expect("link holder");
    // Control: an unlinked file must stay independent (nlink == 1) so a fix that
    // over-links every file cannot pass either.
    fs::write(src.join("mmm_solo.txt"), b"solo payload\n").expect("write solo");

    let direct = test_dir.mkdir("direct").expect("create direct");
    let replayed = test_dir.mkdir("replayed").expect("create replayed");
    let batch_path = test_dir.path().join("BATCH");

    // Record. -a does not imply -H, so -H is passed explicitly. This also copies
    // into direct/, the reference tree the replay must reproduce. direct/ is
    // empty here, so every cohort member is "fresh" and the reorder engages.
    RsyncCommand::new()
        .args([
            "-a",
            "-H",
            &format!("--write-batch={}", batch_path.display()),
            &format!("{}/", src.display()),
            &format!("{}/", direct.display()),
        ])
        .assert_success();
    assert!(
        batch_path.exists(),
        "--write-batch must create '{}'",
        batch_path.display()
    );

    // Replay into a fresh, empty tree - every cohort member is missing, which is
    // what makes the receiver flag the sorted-first member FLAG_HLINK_FIRST.
    let output = RsyncCommand::new()
        .args([
            "-a",
            "-H",
            &format!("--read-batch={}", batch_path.display()),
            &format!("{}/", replayed.display()),
        ])
        .assert_success();
    let stderr = String::from_utf8_lossy(&output.stderr);

    let leader = replayed.join("aaa_leader.txt");
    let holder = replayed.join("zzz_holder.txt");
    let solo = replayed.join("mmm_solo.txt");

    // Both cohort names must exist: the pre-#7928 bug dropped one of them.
    assert!(
        leader.exists(),
        "replay must produce the sorted-first member aaa_leader.txt (stderr: {stderr})"
    );
    assert!(
        holder.exists(),
        "replay must produce the holder member zzz_holder.txt (stderr: {stderr})"
    );
    assert!(
        solo.exists(),
        "replay must produce the unlinked control file mmm_solo.txt (stderr: {stderr})"
    );

    // Both cohort names must carry the payload, not a truncated or empty
    // stand-in: the leader losing its data is precisely the reported symptom.
    assert_eq!(
        fs::read(&leader).expect("read leader"),
        b"linked payload\n",
        "the sorted-first member must carry the cluster payload"
    );
    assert_eq!(
        fs::read(&holder).expect("read holder"),
        b"linked payload\n",
        "the holder member must carry the cluster payload"
    );

    // The two names must resolve to ONE inode with nlink 2 - the hardlink
    // identity #7928 restores, and the whole point of -H.
    let leader_meta = fs::metadata(&leader).expect("stat leader");
    let holder_meta = fs::metadata(&holder).expect("stat holder");
    assert_eq!(
        leader_meta.ino(),
        holder_meta.ino(),
        "aaa_leader.txt and zzz_holder.txt must share one inode after replay"
    );
    assert_eq!(
        leader_meta.nlink(),
        2,
        "the cohort must report nlink=2; got {}",
        leader_meta.nlink()
    );
    assert_eq!(
        holder_meta.nlink(),
        2,
        "the cohort must report nlink=2; got {}",
        holder_meta.nlink()
    );

    // The control file must stay independent of the cohort.
    let solo_meta = fs::metadata(&solo).expect("stat solo");
    assert_eq!(
        solo_meta.nlink(),
        1,
        "the unlinked file must stay independent; got nlink={}",
        solo_meta.nlink()
    );
    assert_ne!(
        solo_meta.ino(),
        leader_meta.ino(),
        "the unlinked file must not be linked into the cohort"
    );

    // The replayed tree must match the reference the recording copied directly.
    for name in ["aaa_leader.txt", "zzz_holder.txt", "mmm_solo.txt"] {
        assert_eq!(
            fs::read(direct.join(name)).expect("read reference"),
            fs::read(replayed.join(name)).expect("read replayed"),
            "replayed '{name}' differs from the directly-copied reference tree"
        );
    }
}

/// Replaying a hardlink batch into a PRE-EXISTING destination whose cluster
/// members already exist as separate, stale files must re-link every member to
/// the freshly transferred payload - not leave a stale pre-existing member as
/// the source.
///
/// This is the oracle-free form of the upstream `batch-only-remove-source-
/// regression` cell. With the cluster payload shipped under the sorted-last
/// data-holder, the batch-replay receiver must key the group's source on the
/// member the stream transferred (recorded at commit), not on disk presence -
/// otherwise the stale sorted-first member wins and the fresh payload is
/// discarded (both members end up hard-linked but carrying the stale bytes).
#[test]
#[cfg(unix)]
fn read_batch_relinks_stale_preexisting_cluster_to_fresh_payload() {
    let test_dir = TestDir::new().expect("create test dir");
    let src = test_dir.mkdir("src").expect("create src");

    let fresh: &[u8] = b"fresh source payload\n";
    // "a-linked" sorts before "z-linked", so the sorted-last member carries the
    // batch payload; the stale sorted-first member must be re-linked to it.
    fs::write(src.join("a-linked.txt"), fresh).expect("write a-linked");
    fs::hard_link(src.join("a-linked.txt"), src.join("z-linked.txt")).expect("link z-linked");

    let direct = test_dir.mkdir("direct").expect("create direct");
    let replay = test_dir.mkdir("replay").expect("create replay");
    let batch = test_dir.path().join("BATCH");

    // The replay destination already holds both cluster names as SEPARATE stale
    // files (distinct inodes), the case the UTS cell exercises.
    let stale: &[u8] = b"stale destination bytes\n";
    fs::write(replay.join("a-linked.txt"), stale).expect("seed stale a");
    fs::write(replay.join("z-linked.txt"), stale).expect("seed stale z");

    RsyncCommand::new()
        .args([
            "-a",
            "-H",
            &format!("--write-batch={}", batch.display()),
            &format!("{}/", src.display()),
            &format!("{}/", direct.display()),
        ])
        .assert_success();

    let output = RsyncCommand::new()
        .args([
            "-a",
            "-H",
            &format!("--read-batch={}", batch.display()),
            &format!("{}/", replay.display()),
        ])
        .assert_success();
    let stderr = String::from_utf8_lossy(&output.stderr);

    for name in ["a-linked.txt", "z-linked.txt"] {
        assert_eq!(
            fs::read(replay.join(name)).expect("read replayed cluster member"),
            fresh,
            "{name} must carry the freshly transferred payload, not the stale \
             pre-existing bytes (stderr: {stderr})"
        );
    }
    let a_ino = fs::metadata(replay.join("a-linked.txt"))
        .expect("stat a-linked")
        .ino();
    let z_ino = fs::metadata(replay.join("z-linked.txt"))
        .expect("stat z-linked")
        .ino();
    assert_eq!(
        a_ino, z_ino,
        "the cluster must share one inode after replay into a stale destination"
    );
}
