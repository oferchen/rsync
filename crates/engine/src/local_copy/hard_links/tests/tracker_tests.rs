//! Basic construction and cohort-leader tests for [`HardLinkTracker`].

use std::path::Path;

use crate::local_copy::hard_links::HardLinkTracker;
use crate::local_copy::test_support;

#[test]
fn new_creates_tracker() {
    let tracker = HardLinkTracker::new();
    let _ = tracker;
}

#[test]
fn default_creates_tracker() {
    let tracker = HardLinkTracker::default();
    let _ = tracker;
}

#[test]
fn existing_target_returns_none_for_new_tracker() {
    let temp = test_support::create_tempdir();
    let file = temp.path().join("test.txt");
    std::fs::write(&file, "content").unwrap();
    let metadata = std::fs::metadata(&file).unwrap();

    let tracker = HardLinkTracker::new();
    assert!(tracker.existing_target(&metadata).is_none());
}

#[test]
fn record_does_not_panic() {
    let temp = test_support::create_tempdir();
    let file = temp.path().join("test.txt");
    std::fs::write(&file, "content").unwrap();
    let metadata = std::fs::metadata(&file).unwrap();

    let mut tracker = HardLinkTracker::new();
    tracker.record(&metadata, Path::new("/dest/test.txt"));
}

#[test]
fn register_acl_cohort_leader_first_call_is_leader() {
    let mut tracker = HardLinkTracker::new();
    assert!(
        tracker.register_acl_cohort_leader(Path::new("/ref/file.bin")),
        "first call for a reference is the cohort leader"
    );
}

#[test]
fn register_acl_cohort_leader_subsequent_calls_are_followers() {
    let mut tracker = HardLinkTracker::new();
    let reference = Path::new("/ref/file.bin");
    assert!(tracker.register_acl_cohort_leader(reference));
    for _ in 0..10 {
        assert!(
            !tracker.register_acl_cohort_leader(reference),
            "subsequent calls for the same reference are followers"
        );
    }
}

#[test]
fn register_acl_cohort_leader_distinct_references_each_leader_once() {
    let mut tracker = HardLinkTracker::new();
    assert!(tracker.register_acl_cohort_leader(Path::new("/ref/a.bin")));
    assert!(tracker.register_acl_cohort_leader(Path::new("/ref/b.bin")));
    assert!(!tracker.register_acl_cohort_leader(Path::new("/ref/a.bin")));
    assert!(!tracker.register_acl_cohort_leader(Path::new("/ref/b.bin")));
}

/// Simulates the `--copy-dest` Link branch behaviour: five destinations
/// linked to the same reference inode would, without the cohort gate,
/// invoke the DACL writer five times. The gate ensures count == 1.
///
/// upstream: hlink.c::hard_link_check returns 1 for followers so the
/// generator never calls `set_file_attrs()` -> `set_acl()` on an alias.
#[test]
fn cohort_gate_yields_one_acl_call_per_cohort() {
    let mut tracker = HardLinkTracker::new();
    let reference = Path::new("/ref/leader.bin");
    let mut dacl_writes = 0_usize;
    for _follower in 0..5 {
        if tracker.register_acl_cohort_leader(reference) {
            dacl_writes += 1;
        }
    }
    assert_eq!(
        dacl_writes, 1,
        "five-link cohort must produce exactly one DACL write"
    );
}

#[cfg(unix)]
mod key_tests {
    use super::super::super::unix::HardLinkKey;

    #[test]
    fn hard_link_key_eq() {
        let key1 = HardLinkKey {
            device: 1,
            inode: 100,
        };
        let key2 = HardLinkKey {
            device: 1,
            inode: 100,
        };
        let key3 = HardLinkKey {
            device: 2,
            inode: 100,
        };
        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }

    #[test]
    fn hard_link_key_hash() {
        use std::collections::HashSet;
        let key1 = HardLinkKey {
            device: 1,
            inode: 100,
        };
        let key2 = HardLinkKey {
            device: 1,
            inode: 100,
        };
        let mut set = HashSet::new();
        set.insert(key1);
        assert!(set.contains(&key2));
    }

    #[test]
    fn hard_link_key_debug() {
        let key = HardLinkKey {
            device: 1,
            inode: 100,
        };
        let debug = format!("{key:?}");
        assert!(debug.contains("HardLinkKey"));
    }

    #[test]
    fn hard_link_key_clone() {
        let key = HardLinkKey {
            device: 1,
            inode: 100,
        };
        let cloned = key;
        assert_eq!(key, cloned);
    }
}
