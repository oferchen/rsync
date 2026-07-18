//! Tests for the protocol-aware `HardlinkApplyTracker`.

use std::path::{Path, PathBuf};

use crate::local_copy::hard_links::{HardlinkApplyResult, HardlinkApplyTracker};
use crate::local_copy::test_support;

#[test]
fn new_tracker_is_empty() {
    let tracker = HardlinkApplyTracker::new();
    assert_eq!(tracker.leader_count(), 0);
    assert_eq!(tracker.deferred_count(), 0);
}

#[test]
fn default_creates_empty_tracker() {
    let tracker = HardlinkApplyTracker::default();
    assert_eq!(tracker.leader_count(), 0);
}

#[test]
fn record_leader_returns_empty_when_no_deferred() {
    let mut tracker = HardlinkApplyTracker::new();
    let deferred = tracker.record_leader(42, PathBuf::from("/dest/leader.txt"));
    assert!(deferred.is_empty());
    assert_eq!(tracker.leader_count(), 1);
}

#[test]
fn leader_path_returns_recorded_path() {
    let mut tracker = HardlinkApplyTracker::new();
    tracker.record_leader(7, PathBuf::from("/dest/file.txt"));
    assert_eq!(tracker.leader_path(7), Some(Path::new("/dest/file.txt")));
    assert!(tracker.leader_path(99).is_none());
}

#[test]
fn follower_linked_when_leader_exists() {
    let temp = test_support::create_tempdir();
    let leader_path = temp.path().join("leader.txt");
    let follower_path = temp.path().join("follower.txt");
    std::fs::write(&leader_path, "shared content").unwrap();

    let mut tracker = HardlinkApplyTracker::new();
    tracker.record_leader(10, leader_path.clone());

    let result = tracker.apply_follower(10, &follower_path).unwrap();
    assert_eq!(result, HardlinkApplyResult::Linked);
    assert_eq!(
        std::fs::read_to_string(&follower_path).unwrap(),
        "shared content"
    );
}

#[cfg(unix)]
#[test]
fn follower_shares_inode_with_leader() {
    use std::os::unix::fs::MetadataExt;

    let temp = test_support::create_tempdir();
    let leader_path = temp.path().join("leader.txt");
    let follower_path = temp.path().join("follower.txt");
    std::fs::write(&leader_path, "content").unwrap();

    let mut tracker = HardlinkApplyTracker::new();
    tracker.record_leader(5, leader_path.clone());
    tracker.apply_follower(5, &follower_path).unwrap();

    let leader_meta = std::fs::metadata(&leader_path).unwrap();
    let follower_meta = std::fs::metadata(&follower_path).unwrap();
    assert_eq!(leader_meta.ino(), follower_meta.ino());
    assert_eq!(leader_meta.dev(), follower_meta.dev());
    assert!(leader_meta.nlink() >= 2);
}

#[test]
fn follower_deferred_when_leader_missing() {
    let mut tracker = HardlinkApplyTracker::new();
    let result = tracker
        .apply_follower(42, Path::new("/dest/follower.txt"))
        .unwrap();
    assert_eq!(result, HardlinkApplyResult::Deferred);
    assert_eq!(tracker.deferred_count(), 1);
}

#[test]
fn record_leader_returns_deferred_followers() {
    let mut tracker = HardlinkApplyTracker::new();

    tracker
        .apply_follower(10, Path::new("/dest/f1.txt"))
        .unwrap();
    tracker
        .apply_follower(10, Path::new("/dest/f2.txt"))
        .unwrap();
    assert_eq!(tracker.deferred_count(), 2);

    // Record the leader - should return deferred followers
    let deferred = tracker.record_leader(10, PathBuf::from("/dest/leader.txt"));
    assert_eq!(deferred.len(), 2);
    assert_eq!(deferred[0], PathBuf::from("/dest/f1.txt"));
    assert_eq!(deferred[1], PathBuf::from("/dest/f2.txt"));
    assert_eq!(tracker.deferred_count(), 0);
}

#[cfg(unix)]
#[test]
fn deferred_followers_resolved_after_leader_committed() {
    use std::os::unix::fs::MetadataExt;

    let temp = test_support::create_tempdir();
    let leader_path = temp.path().join("leader.txt");
    let follower1 = temp.path().join("follower1.txt");
    let follower2 = temp.path().join("follower2.txt");

    let mut tracker = HardlinkApplyTracker::new();

    // Followers arrive before leader
    tracker.apply_follower(20, &follower1).unwrap();
    tracker.apply_follower(20, &follower2).unwrap();
    assert_eq!(tracker.deferred_count(), 2);

    // Leader committed to disk
    std::fs::write(&leader_path, "deferred content").unwrap();
    let deferred = tracker.record_leader(20, leader_path.clone());

    // Caller creates links for deferred followers
    for follower_dest in &deferred {
        std::fs::hard_link(&leader_path, follower_dest).unwrap();
    }

    // Verify all share the same inode
    let leader_ino = std::fs::metadata(&leader_path).unwrap().ino();
    assert_eq!(std::fs::metadata(&follower1).unwrap().ino(), leader_ino);
    assert_eq!(std::fs::metadata(&follower2).unwrap().ino(), leader_ino);
}

#[cfg(unix)]
#[test]
fn hardlinks_across_directories() {
    use std::os::unix::fs::MetadataExt;

    let temp = test_support::create_tempdir();
    let dir_a = temp.path().join("dir_a");
    let dir_b = temp.path().join("dir_b");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();

    let leader_path = dir_a.join("file.txt");
    let follower_path = dir_b.join("file.txt");
    std::fs::write(&leader_path, "cross-dir content").unwrap();

    let mut tracker = HardlinkApplyTracker::new();
    tracker.record_leader(30, leader_path.clone());

    let result = tracker.apply_follower(30, &follower_path).unwrap();
    assert_eq!(result, HardlinkApplyResult::Linked);

    let leader_meta = std::fs::metadata(&leader_path).unwrap();
    let follower_meta = std::fs::metadata(&follower_path).unwrap();
    assert_eq!(leader_meta.ino(), follower_meta.ino());
    assert_eq!(
        std::fs::read_to_string(&follower_path).unwrap(),
        "cross-dir content"
    );
}

#[test]
fn multiple_independent_groups() {
    let temp = test_support::create_tempdir();
    let leader1 = temp.path().join("group1_leader.txt");
    let leader2 = temp.path().join("group2_leader.txt");
    let follower1 = temp.path().join("group1_follower.txt");
    let follower2 = temp.path().join("group2_follower.txt");

    std::fs::write(&leader1, "group1").unwrap();
    std::fs::write(&leader2, "group2").unwrap();

    let mut tracker = HardlinkApplyTracker::new();
    tracker.record_leader(100, leader1.clone());
    tracker.record_leader(200, leader2.clone());

    tracker.apply_follower(100, &follower1).unwrap();
    tracker.apply_follower(200, &follower2).unwrap();

    assert_eq!(std::fs::read_to_string(&follower1).unwrap(), "group1");
    assert_eq!(std::fs::read_to_string(&follower2).unwrap(), "group2");
}

#[test]
fn follower_replaces_existing_file() {
    let temp = test_support::create_tempdir();
    let leader = temp.path().join("leader.txt");
    let follower = temp.path().join("follower.txt");

    std::fs::write(&leader, "correct content").unwrap();
    std::fs::write(&follower, "old content").unwrap();

    let mut tracker = HardlinkApplyTracker::new();
    tracker.record_leader(50, leader.clone());

    let result = tracker.apply_follower(50, &follower).unwrap();
    assert_eq!(result, HardlinkApplyResult::Linked);
    assert_eq!(
        std::fs::read_to_string(&follower).unwrap(),
        "correct content"
    );
}

#[test]
fn follower_creates_parent_directories() {
    let temp = test_support::create_tempdir();
    let leader = temp.path().join("leader.txt");
    let follower = temp.path().join("deep/nested/dir/follower.txt");

    std::fs::write(&leader, "content").unwrap();

    let mut tracker = HardlinkApplyTracker::new();
    tracker.record_leader(60, leader.clone());

    let result = tracker.apply_follower(60, &follower).unwrap();
    assert_eq!(result, HardlinkApplyResult::Linked);
    assert!(follower.exists());
    assert_eq!(std::fs::read_to_string(&follower).unwrap(), "content");
}

#[test]
fn resolve_deferred_creates_links() {
    let temp = test_support::create_tempdir();
    let leader = temp.path().join("leader.txt");
    let follower1 = temp.path().join("f1.txt");
    let follower2 = temp.path().join("f2.txt");

    let mut tracker = HardlinkApplyTracker::new();

    tracker.apply_follower(70, &follower1).unwrap();
    tracker.apply_follower(70, &follower2).unwrap();

    std::fs::write(&leader, "resolved content").unwrap();
    // Re-insert deferred into tracker for resolve_deferred to handle
    let deferred_list = tracker.record_leader(70, leader.clone());

    // Manually re-defer them for the resolve path
    let mut tracker2 = HardlinkApplyTracker::new();
    tracker2.record_leader(70, leader.clone());
    for path in &deferred_list {
        // Simulate re-deferral by directly adding
        tracker2.deferred.entry(70).or_default().push(path.clone());
    }

    let (linked, errors) = tracker2.resolve_deferred();
    assert_eq!(linked, 2);
    assert!(errors.is_empty());
    assert!(follower1.exists());
    assert!(follower2.exists());
}

#[test]
fn resolve_deferred_reports_missing_leader() {
    let mut tracker = HardlinkApplyTracker::new();
    // Manually insert a deferred follower for a non-existent leader
    tracker
        .deferred
        .entry(999)
        .or_default()
        .push(PathBuf::from("/nonexistent/follower.txt"));

    let (linked, errors) = tracker.resolve_deferred();
    assert_eq!(linked, 0);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, PathBuf::from("/nonexistent/follower.txt"));
}

#[test]
fn many_followers_in_one_group() {
    let temp = test_support::create_tempdir();
    let leader = temp.path().join("leader.txt");
    std::fs::write(&leader, "shared").unwrap();

    let mut tracker = HardlinkApplyTracker::new();
    tracker.record_leader(80, leader.clone());

    for i in 0..50 {
        let follower = temp.path().join(format!("follower_{i}.txt"));
        let result = tracker.apply_follower(80, &follower).unwrap();
        assert_eq!(result, HardlinkApplyResult::Linked);
        assert_eq!(std::fs::read_to_string(&follower).unwrap(), "shared");
    }
}

#[cfg(unix)]
#[test]
fn all_followers_share_single_inode() {
    use std::os::unix::fs::MetadataExt;

    let temp = test_support::create_tempdir();
    let leader = temp.path().join("leader.txt");
    std::fs::write(&leader, "inode-check").unwrap();

    let mut tracker = HardlinkApplyTracker::new();
    tracker.record_leader(90, leader.clone());

    let leader_ino = std::fs::metadata(&leader).unwrap().ino();

    for i in 0..10 {
        let follower = temp.path().join(format!("f{i}.txt"));
        tracker.apply_follower(90, &follower).unwrap();
        assert_eq!(std::fs::metadata(&follower).unwrap().ino(), leader_ino);
    }

    // nlink should be 11 (1 leader + 10 followers)
    assert_eq!(std::fs::metadata(&leader).unwrap().nlink(), 11);
}
