//! Integration tests for hardlink detection and resolution.
//!
//! These tests verify the complete hardlink algorithm including:
//! - Single and multiple hardlink groups
//! - Cross-device handling
//! - Large groups with many hardlinks
//! - Edge cases with extreme values

use engine::hardlink::{HardlinkAction, HardlinkKey, HardlinkTracker};

#[test]
fn single_hardlink_group_basic() {
    let mut tracker = HardlinkTracker::new();
    let key = HardlinkKey::new(0xFD00, 12345);

    // Register three files with same dev/ino
    assert!(tracker.register(key, 0));
    assert!(!tracker.register(key, 5));
    assert!(!tracker.register(key, 10));

    // First should be source
    assert_eq!(tracker.resolve(0), HardlinkAction::Transfer);
    assert!(tracker.is_hardlink_source(0));
    assert_eq!(tracker.get_hardlink_target(0), None);

    // Others should link to first
    assert_eq!(tracker.resolve(5), HardlinkAction::LinkTo(0));
    assert!(!tracker.is_hardlink_source(5));
    assert_eq!(tracker.get_hardlink_target(5), Some(0));

    assert_eq!(tracker.resolve(10), HardlinkAction::LinkTo(0));
    assert!(!tracker.is_hardlink_source(10));
    assert_eq!(tracker.get_hardlink_target(10), Some(0));

    // Verify counts
    assert_eq!(tracker.file_count(), 3);
    assert_eq!(tracker.group_count(), 1);

    // Verify group structure
    let groups: Vec<_> = tracker.groups().collect();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].source_index, 0);
    assert_eq!(groups[0].link_indices, vec![5, 10]);
    assert_eq!(groups[0].total_count(), 3);
}

#[test]
fn multiple_hardlink_groups() {
    let mut tracker = HardlinkTracker::new();
    let key1 = HardlinkKey::new(1, 100);
    let key2 = HardlinkKey::new(1, 200);
    let key3 = HardlinkKey::new(1, 300);

    // Group 1: files 0, 1
    tracker.register(key1, 0);
    tracker.register(key1, 1);

    // Group 2: files 2, 3, 4
    tracker.register(key2, 2);
    tracker.register(key2, 3);
    tracker.register(key2, 4);

    // Group 3: file 5 (no links, shouldn't appear in groups iterator)
    tracker.register(key3, 5);

    // Verify actions
    assert_eq!(tracker.resolve(0), HardlinkAction::Transfer);
    assert_eq!(tracker.resolve(1), HardlinkAction::LinkTo(0));
    assert_eq!(tracker.resolve(2), HardlinkAction::Transfer);
    assert_eq!(tracker.resolve(3), HardlinkAction::LinkTo(2));
    assert_eq!(tracker.resolve(4), HardlinkAction::LinkTo(2));
    assert_eq!(tracker.resolve(5), HardlinkAction::Transfer);

    // Verify sources
    assert!(tracker.is_hardlink_source(0));
    assert!(!tracker.is_hardlink_source(1));
    assert!(tracker.is_hardlink_source(2));
    assert!(!tracker.is_hardlink_source(3));
    assert!(!tracker.is_hardlink_source(4));
    assert!(!tracker.is_hardlink_source(5)); // Single file, not a source

    // Verify counts
    assert_eq!(tracker.file_count(), 6);
    assert_eq!(tracker.group_count(), 2); // Only groups with links

    // Verify groups
    let mut groups: Vec<_> = tracker.groups().collect();
    groups.sort_by_key(|g| g.source_index);
    assert_eq!(groups.len(), 2);

    assert_eq!(groups[0].source_index, 0);
    assert_eq!(groups[0].link_indices, vec![1]);

    assert_eq!(groups[1].source_index, 2);
    assert_eq!(groups[1].link_indices, vec![3, 4]);
}

#[test]
fn cross_device_hardlinks_not_linked() {
    let mut tracker = HardlinkTracker::new();

    // Same inode, different devices
    let key1 = HardlinkKey::new(0, 12345);
    let key2 = HardlinkKey::new(1, 12345);
    let key3 = HardlinkKey::new(2, 12345);

    tracker.register(key1, 0);
    tracker.register(key2, 1);
    tracker.register(key3, 2);

    // All should be sources (no links)
    assert_eq!(tracker.resolve(0), HardlinkAction::Transfer);
    assert_eq!(tracker.resolve(1), HardlinkAction::Transfer);
    assert_eq!(tracker.resolve(2), HardlinkAction::Transfer);

    // None should be hardlink sources (no links in their groups)
    assert!(!tracker.is_hardlink_source(0));
    assert!(!tracker.is_hardlink_source(1));
    assert!(!tracker.is_hardlink_source(2));

    assert_eq!(tracker.file_count(), 3);
    assert_eq!(tracker.group_count(), 0); // No groups with links
}

#[test]
fn files_with_nlink_1_no_hardlinks() {
    let mut tracker = HardlinkTracker::new();

    // Each file has unique dev/ino (nlink=1)
    for i in 0..10 {
        let key = HardlinkKey::new(1, i as u64);
        tracker.register(key, i);
    }

    // All should be transferred normally
    for i in 0..10 {
        assert_eq!(tracker.resolve(i), HardlinkAction::Transfer);
        assert!(!tracker.is_hardlink_source(i));
    }

    assert_eq!(tracker.file_count(), 10);
    assert_eq!(tracker.group_count(), 0);
}

#[test]
fn large_hardlink_group() {
    let mut tracker = HardlinkTracker::new();
    let key = HardlinkKey::new(0xFD00, 999999);

    const NUM_LINKS: i32 = 10_000;

    // Register many files with same dev/ino
    for i in 0..NUM_LINKS {
        let is_first = tracker.register(key, i);
        assert_eq!(is_first, i == 0);
    }

    // First should be source
    assert_eq!(tracker.resolve(0), HardlinkAction::Transfer);
    assert!(tracker.is_hardlink_source(0));

    // All others should link to first
    for i in 1..NUM_LINKS {
        assert_eq!(tracker.resolve(i), HardlinkAction::LinkTo(0));
        assert!(!tracker.is_hardlink_source(i));
        assert_eq!(tracker.get_hardlink_target(i), Some(0));
    }

    assert_eq!(tracker.file_count(), NUM_LINKS as usize);
    assert_eq!(tracker.group_count(), 1);

    // Verify group structure
    let groups: Vec<_> = tracker.groups().collect();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].source_index, 0);
    assert_eq!(groups[0].link_indices.len(), (NUM_LINKS - 1) as usize);
    assert_eq!(groups[0].total_count(), NUM_LINKS as usize);
}

#[test]
fn many_distinct_groups() {
    let mut tracker = HardlinkTracker::with_capacity(1000);

    const NUM_GROUPS: i32 = 1000;

    // Create many groups, each with 2 files
    for group in 0..NUM_GROUPS {
        let key = HardlinkKey::new((group / 100) as u64, group as u64);
        tracker.register(key, group * 2);
        tracker.register(key, group * 2 + 1);
    }

    // Verify all groups
    for group in 0..NUM_GROUPS {
        let source = group * 2;
        let link = group * 2 + 1;

        assert_eq!(tracker.resolve(source), HardlinkAction::Transfer);
        assert_eq!(tracker.resolve(link), HardlinkAction::LinkTo(source));
    }

    assert_eq!(tracker.file_count(), (NUM_GROUPS * 2) as usize);
    assert_eq!(tracker.group_count(), NUM_GROUPS as usize);
}

#[test]
fn extreme_device_inode_values() {
    let mut tracker = HardlinkTracker::new();

    let extreme_cases = [
        (0, 0),
        (u64::MAX, u64::MAX),
        (0, u64::MAX),
        (u64::MAX, 0),
        (1, 1),
        (u64::MAX - 1, u64::MAX - 1),
    ];

    for (i, &(dev, ino)) in extreme_cases.iter().enumerate() {
        let key = HardlinkKey::new(dev, ino);
        tracker.register(key, i as i32);
        tracker.register(key, (i + 100) as i32);
    }

    // Verify each group works correctly
    for (i, _) in extreme_cases.iter().enumerate() {
        let source = i as i32;
        let link = (i + 100) as i32;

        assert_eq!(tracker.resolve(source), HardlinkAction::Transfer);
        assert_eq!(tracker.resolve(link), HardlinkAction::LinkTo(source));
    }

    assert_eq!(tracker.group_count(), extreme_cases.len());
}

#[test]
fn negative_file_indices() {
    let mut tracker = HardlinkTracker::new();
    let key = HardlinkKey::new(1, 100);

    // Negative indices (used in incremental file lists)
    tracker.register(key, -10);
    tracker.register(key, -5);
    tracker.register(key, 0);
    tracker.register(key, 5);

    assert_eq!(tracker.resolve(-10), HardlinkAction::Transfer);
    assert_eq!(tracker.resolve(-5), HardlinkAction::LinkTo(-10));
    assert_eq!(tracker.resolve(0), HardlinkAction::LinkTo(-10));
    assert_eq!(tracker.resolve(5), HardlinkAction::LinkTo(-10));
}

#[test]
fn interleaved_registration() {
    let mut tracker = HardlinkTracker::new();
    let key1 = HardlinkKey::new(1, 100);
    let key2 = HardlinkKey::new(1, 200);

    // Interleave registrations from different groups
    tracker.register(key1, 0);
    tracker.register(key2, 1);
    tracker.register(key1, 2);
    tracker.register(key2, 3);
    tracker.register(key1, 4);

    // Group 1 (key1): 0 -> 2, 4
    assert_eq!(tracker.resolve(0), HardlinkAction::Transfer);
    assert_eq!(tracker.resolve(2), HardlinkAction::LinkTo(0));
    assert_eq!(tracker.resolve(4), HardlinkAction::LinkTo(0));

    // Group 2 (key2): 1 -> 3
    assert_eq!(tracker.resolve(1), HardlinkAction::Transfer);
    assert_eq!(tracker.resolve(3), HardlinkAction::LinkTo(1));
}

#[test]
fn tracker_clear() {
    let mut tracker = HardlinkTracker::new();
    let key = HardlinkKey::new(1, 100);

    tracker.register(key, 0);
    tracker.register(key, 1);
    assert_eq!(tracker.file_count(), 2);
    assert_eq!(tracker.group_count(), 1);

    tracker.clear();
    assert_eq!(tracker.file_count(), 0);
    assert_eq!(tracker.group_count(), 0);

    // Can reuse after clear
    tracker.register(key, 100);
    tracker.register(key, 101);
    assert_eq!(tracker.resolve(100), HardlinkAction::Transfer);
    assert_eq!(tracker.resolve(101), HardlinkAction::LinkTo(100));
}

#[test]
fn mixed_scenario_realistic() {
    let mut tracker = HardlinkTracker::new();

    // Simulate a realistic directory scan:
    // - Some files have no hardlinks (nlink=1)
    // - Some files are in small hardlink groups (2-3 files)
    // - One large hardlink group (system files)

    // Single files
    tracker.register(HardlinkKey::new(1, 1000), 0);
    tracker.register(HardlinkKey::new(1, 1001), 1);

    // Small group 1: 2 files
    let group1 = HardlinkKey::new(1, 2000);
    tracker.register(group1, 2);
    tracker.register(group1, 3);

    // Small group 2: 3 files
    let group2 = HardlinkKey::new(1, 3000);
    tracker.register(group2, 4);
    tracker.register(group2, 5);
    tracker.register(group2, 6);

    // More single files
    tracker.register(HardlinkKey::new(1, 1002), 7);

    // Large group: 100 files (e.g., system executables)
    let large_group = HardlinkKey::new(1, 5000);
    for i in 0..100 {
        tracker.register(large_group, 100 + i);
    }

    // Verify counts
    assert_eq!(tracker.file_count(), 108); // 3 singles + 2 in group1 + 3 in group2 + 100 in large group
    assert_eq!(tracker.group_count(), 3); // Only groups with links

    // Verify single files
    assert_eq!(tracker.resolve(0), HardlinkAction::Transfer);
    assert_eq!(tracker.resolve(1), HardlinkAction::Transfer);
    assert_eq!(tracker.resolve(7), HardlinkAction::Transfer);

    // Verify small groups
    assert_eq!(tracker.resolve(2), HardlinkAction::Transfer);
    assert_eq!(tracker.resolve(3), HardlinkAction::LinkTo(2));

    assert_eq!(tracker.resolve(4), HardlinkAction::Transfer);
    assert_eq!(tracker.resolve(5), HardlinkAction::LinkTo(4));
    assert_eq!(tracker.resolve(6), HardlinkAction::LinkTo(4));

    // Verify large group
    assert_eq!(tracker.resolve(100), HardlinkAction::Transfer);
    for i in 1..100 {
        assert_eq!(tracker.resolve(100 + i), HardlinkAction::LinkTo(100));
    }
}

#[test]
fn hardlink_key_equality_and_hashing() {
    use std::collections::HashSet;

    let k1 = HardlinkKey::new(100, 12345);
    let k2 = HardlinkKey::new(100, 12345);
    let k3 = HardlinkKey::new(100, 12346);
    let k4 = HardlinkKey::new(101, 12345);

    // Equality
    assert_eq!(k1, k2);
    assert_ne!(k1, k3);
    assert_ne!(k1, k4);
    assert_ne!(k3, k4);

    // Hashing
    let mut set = HashSet::new();
    set.insert(k1);
    assert!(set.contains(&k2)); // Same key
    assert!(!set.contains(&k3)); // Different inode
    assert!(!set.contains(&k4)); // Different device
}

#[test]
fn resolver_static_methods() {
    use engine::hardlink::HardlinkResolver;

    let mut tracker = HardlinkTracker::new();
    let key = HardlinkKey::new(1, 100);

    tracker.register(key, 0);
    tracker.register(key, 1);

    // Test static resolver
    assert_eq!(HardlinkResolver::resolve(&tracker, 0), HardlinkAction::Transfer);
    assert_eq!(HardlinkResolver::resolve(&tracker, 1), HardlinkAction::LinkTo(0));
}

#[test]
fn unregistered_file_returns_skip() {
    let tracker = HardlinkTracker::new();

    // File never registered should return Skip
    assert_eq!(tracker.resolve(999), HardlinkAction::Skip);
    assert_eq!(tracker.get_hardlink_target(999), None);
    assert!(!tracker.is_hardlink_source(999));
}

#[test]
fn groups_iterator_only_returns_groups_with_links() {
    let mut tracker = HardlinkTracker::new();

    // Create some single-file "groups" (no links)
    for i in 0..5 {
        tracker.register(HardlinkKey::new(1, i as u64), i);
    }

    // Create one actual group with links
    let key = HardlinkKey::new(1, 100);
    tracker.register(key, 10);
    tracker.register(key, 11);

    // Iterator should only yield the one group with links
    let groups: Vec<_> = tracker.groups().collect();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].source_index, 10);
}

#[test]
fn protocol_28_29_simulation() {
    // Simulate protocol 28-29 behavior where dev/ino are transmitted
    let mut tracker = HardlinkTracker::new();

    // In protocol 28-29, we track same_dev flag
    let dev = 0xFD00u64;
    let ino1 = 12345u64;
    let ino2 = 67890u64;

    // Group 1: Same device
    tracker.register(HardlinkKey::new(dev, ino1), 0);
    tracker.register(HardlinkKey::new(dev, ino1), 1);

    // Group 2: Different device (should not link)
    tracker.register(HardlinkKey::new(dev + 1, ino1), 2);

    // Group 3: Same device, different inode
    tracker.register(HardlinkKey::new(dev, ino2), 3);
    tracker.register(HardlinkKey::new(dev, ino2), 4);

    // Verify protocol behavior
    assert_eq!(tracker.resolve(0), HardlinkAction::Transfer);
    assert_eq!(tracker.resolve(1), HardlinkAction::LinkTo(0));
    assert_eq!(tracker.resolve(2), HardlinkAction::Transfer); // Different dev
    assert_eq!(tracker.resolve(3), HardlinkAction::Transfer);
    assert_eq!(tracker.resolve(4), HardlinkAction::LinkTo(3));
}

#[test]
fn protocol_30plus_simulation() {
    // Simulate protocol 30+ behavior where indices are used
    let mut tracker = HardlinkTracker::new();

    // In protocol 30+, we use file indices directly
    let key = HardlinkKey::new(1, 100);

    // First file becomes index 0 (HLINK_FIRST)
    assert!(tracker.register(key, 0));

    // Subsequent files reference index 0
    assert!(!tracker.register(key, 5));
    assert!(!tracker.register(key, 10));

    // Verify index-based resolution
    let groups: Vec<_> = tracker.groups().collect();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].source_index, 0);
    assert_eq!(groups[0].link_indices, vec![5, 10]);

    // Can use these indices for wire protocol encoding
    for &link_idx in &groups[0].link_indices {
        assert_eq!(tracker.get_hardlink_target(link_idx), Some(groups[0].source_index));
    }
}
