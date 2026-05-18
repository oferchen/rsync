//! Tests for the io_uring LINKAT dispatch in hardlink creation.
//!
//! These tests verify that `fast_io::hard_link` works correctly regardless
//! of whether io_uring handles the link or `std::fs::hard_link` does.

use crate::local_copy::hard_links::{HardlinkApplyResult, HardlinkApplyTracker};
use crate::local_copy::test_support;

#[test]
fn hard_link_via_io_uring_or_fallback_creates_link() {
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
fn hard_link_via_io_uring_or_fallback_shares_inode() {
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
fn apply_follower_uses_io_uring_or_fallback() {
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
fn resolve_deferred_uses_io_uring_or_fallback() {
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

#[test]
fn try_hard_link_via_io_uring_returns_consistent_availability() {
    let dir = test_support::create_tempdir();
    let src = dir.path().join("probe_src.txt");
    let dst1 = dir.path().join("probe_dst1.txt");
    let dst2 = dir.path().join("probe_dst2.txt");
    std::fs::write(&src, b"data").unwrap();

    let first = fast_io::try_hard_link_via_io_uring(&src, &dst1).is_some();
    let second = fast_io::try_hard_link_via_io_uring(&src, &dst2).is_some();
    assert_eq!(first, second, "availability must be consistent");
}
