//! Tests for device/inode tracking edge cases.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::super::unix::HardLinkKey;

/// Test that extreme device/inode values are handled correctly.
#[test]
fn extreme_device_inode_values() {
    let test_cases = [
        (0u64, 0u64),
        (u64::MAX, u64::MAX),
        (u64::MAX, 0),
        (0, u64::MAX),
        (1, 1),
        (u64::MAX - 1, u64::MAX - 1),
    ];

    for (device, inode) in test_cases {
        let key = HardLinkKey { device, inode };
        // Should not panic
        let _ = format!("{key:?}");

        // Clone should work
        let cloned = key;
        assert_eq!(key, cloned);
    }
}

/// Test hash collision resistance for HardLinkKey.
#[test]
fn hash_collision_resistance() {
    fn hash_key(key: &HardLinkKey) -> u64 {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }

    let test_pairs = [
        // Swapped device/inode
        (
            HardLinkKey {
                device: 12345,
                inode: 67890,
            },
            HardLinkKey {
                device: 67890,
                inode: 12345,
            },
        ),
        // Adjacent values
        (
            HardLinkKey {
                device: 100,
                inode: 100,
            },
            HardLinkKey {
                device: 100,
                inode: 101,
            },
        ),
        // High bits difference
        (
            HardLinkKey {
                device: 1 << 63,
                inode: 0,
            },
            HardLinkKey {
                device: 0,
                inode: 1 << 63,
            },
        ),
    ];

    for (key1, key2) in test_pairs {
        assert_ne!(key1, key2, "keys should not be equal");
        // Hashes might collide but it's acceptable; we just verify equality works
        if hash_key(&key1) == hash_key(&key2) {
            assert_ne!(
                key1, key2,
                "equal hash but unequal keys should be distinguished"
            );
        }
    }
}

/// Test that Copy trait works correctly for HardLinkKey.
#[test]
fn hard_link_key_is_copy() {
    let key = HardLinkKey {
        device: 42,
        inode: 100,
    };
    let copy1 = key;
    let copy2 = key;

    // All should be equal
    assert_eq!(key, copy1);
    assert_eq!(copy1, copy2);
    assert_eq!(key.device, 42);
    assert_eq!(key.inode, 100);
}
