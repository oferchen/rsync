//! Tests for hardlink creation dispatch through `fast_io::hard_link`.
//!
//! `fast_io::hard_link` issues a direct `linkat(2)` / `link(2)` syscall on
//! every platform - upstream's model (`hlink.c:hard_link_one()` ->
//! `syscall.c:do_link_at()`). These tests verify the engine's link paths
//! produce correct hardlinks through that direct-syscall entry point; the
//! structure-level assertion that no io_uring ring is built per link lives
//! in `fast_io::io_uring_ops`.

use crate::local_copy::hard_links::{HardlinkApplyResult, HardlinkApplyTracker};
use crate::local_copy::test_support;

#[test]
fn hard_link_direct_syscall_creates_link() {
    let temp = test_support::create_tempdir();
    let src = temp.path().join("linkat_src.txt");
    let dst = temp.path().join("linkat_dst.txt");
    std::fs::write(&src, b"linkat dispatch test").unwrap();

    fast_io::hard_link(&src, &dst).expect("hard link must succeed");

    assert!(src.exists());
    assert!(dst.exists());
    assert_eq!(
        std::fs::read_to_string(&dst).unwrap(),
        "linkat dispatch test"
    );
}

#[cfg(unix)]
#[test]
fn hard_link_direct_syscall_shares_inode() {
    use std::os::unix::fs::MetadataExt;

    let temp = test_support::create_tempdir();
    let src = temp.path().join("linkat_inode_src.txt");
    let dst = temp.path().join("linkat_inode_dst.txt");
    std::fs::write(&src, b"inode check").unwrap();

    fast_io::hard_link(&src, &dst).expect("hard link must succeed");

    let src_ino = std::fs::metadata(&src).unwrap().ino();
    let dst_ino = std::fs::metadata(&dst).unwrap().ino();
    assert_eq!(src_ino, dst_ino, "hard link must share same inode");
}

#[test]
fn apply_follower_links_via_direct_syscall() {
    let temp = test_support::create_tempdir();
    let leader = temp.path().join("leader_dispatch.txt");
    let follower = temp.path().join("follower_dispatch.txt");
    std::fs::write(&leader, b"dispatch content").unwrap();

    let mut tracker = HardlinkApplyTracker::new();
    tracker.record_leader(42, leader.clone());

    let result = tracker.apply_follower(42, &follower).unwrap();
    assert_eq!(result, HardlinkApplyResult::Linked);
    assert_eq!(
        std::fs::read_to_string(&follower).unwrap(),
        "dispatch content"
    );
}

#[test]
fn resolve_deferred_links_via_direct_syscall() {
    let temp = test_support::create_tempdir();
    let leader = temp.path().join("deferred_leader.txt");
    let follower1 = temp.path().join("deferred_f1.txt");
    let follower2 = temp.path().join("deferred_f2.txt");

    std::fs::write(&leader, b"deferred content").unwrap();

    let mut tracker = HardlinkApplyTracker::new();
    tracker.record_leader(77, leader.clone());
    tracker
        .deferred
        .entry(77)
        .or_default()
        .push(follower1.clone());
    tracker
        .deferred
        .entry(77)
        .or_default()
        .push(follower2.clone());

    let (linked, errors) = tracker.resolve_deferred();
    assert_eq!(linked, 2);
    assert!(errors.is_empty());
    assert_eq!(
        std::fs::read_to_string(&follower1).unwrap(),
        "deferred content"
    );
    assert_eq!(
        std::fs::read_to_string(&follower2).unwrap(),
        "deferred content"
    );
}
