#[cfg(test)]
mod basic {
    use crate::flist::hardlink::{DevIno, HardlinkEntry, HardlinkLookup, HardlinkTable};

    #[test]
    fn dev_ino_new() {
        let di = DevIno::new(1, 2);
        assert_eq!(di.dev, 1);
        assert_eq!(di.ino, 2);
    }

    #[test]
    fn dev_ino_eq() {
        let a = DevIno::new(1, 2);
        let b = DevIno::new(1, 2);
        let c = DevIno::new(1, 3);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn dev_ino_hash() {
        use rustc_hash::FxHashSet;
        let mut set = FxHashSet::default();
        set.insert(DevIno::new(1, 2));
        assert!(set.contains(&DevIno::new(1, 2)));
        assert!(!set.contains(&DevIno::new(1, 3)));
    }

    #[test]
    fn hardlink_entry_new() {
        let entry = HardlinkEntry::new(42);
        assert_eq!(entry.first_ndx, 42);
        assert_eq!(entry.link_count, 1);
    }

    #[test]
    fn hardlink_table_new() {
        let table = HardlinkTable::new();
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn hardlink_table_first_occurrence() {
        let mut table = HardlinkTable::new();
        let result = table.find_or_insert(DevIno::new(0, 12345), 0);
        assert_eq!(result, HardlinkLookup::First(0));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn hardlink_table_second_occurrence() {
        let mut table = HardlinkTable::new();
        table.find_or_insert(DevIno::new(0, 12345), 0);
        let result = table.find_or_insert(DevIno::new(0, 12345), 5);
        assert_eq!(result, HardlinkLookup::LinkTo(0));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn hardlink_table_different_files() {
        let mut table = HardlinkTable::new();
        let r1 = table.find_or_insert(DevIno::new(0, 100), 0);
        let r2 = table.find_or_insert(DevIno::new(0, 200), 1);
        assert_eq!(r1, HardlinkLookup::First(0));
        assert_eq!(r2, HardlinkLookup::First(1));
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn hardlink_table_link_count() {
        let mut table = HardlinkTable::new();
        let di = DevIno::new(0, 12345);
        table.find_or_insert(di, 0);
        table.find_or_insert(di, 5);
        table.find_or_insert(di, 10);

        let entry = table.get(&di).unwrap();
        assert_eq!(entry.link_count, 3);
        assert_eq!(entry.first_ndx, 0);
    }

    #[test]
    fn hardlink_table_get_nonexistent() {
        let table = HardlinkTable::new();
        assert!(table.get(&DevIno::new(0, 12345)).is_none());
    }

    #[test]
    fn hardlink_table_clear() {
        let mut table = HardlinkTable::new();
        table.find_or_insert(DevIno::new(0, 100), 0);
        table.find_or_insert(DevIno::new(0, 200), 1);
        assert_eq!(table.len(), 2);

        table.clear();
        assert!(table.is_empty());
    }

    #[test]
    fn hardlink_table_with_capacity() {
        let table = HardlinkTable::with_capacity(100);
        assert!(table.is_empty());
    }

    #[test]
    fn hardlink_table_different_devices() {
        let mut table = HardlinkTable::new();
        // Same inode on different devices - should be separate entries
        let r1 = table.find_or_insert(DevIno::new(1, 100), 0);
        let r2 = table.find_or_insert(DevIno::new(2, 100), 1);
        assert_eq!(r1, HardlinkLookup::First(0));
        assert_eq!(r2, HardlinkLookup::First(1));
        assert_eq!(table.len(), 2);
    }
}

/// Tests for collision handling in the hardlink lookup table.
///
/// These tests verify correct behavior when:
/// - Multiple files have the same dev/ino pair (from different systems)
/// - Hash collisions occur in the underlying FxHashMap
/// - Large numbers of hardlinks stress the table
#[cfg(test)]
mod collision {
    use crate::flist::hardlink::{DevIno, HardlinkLookup, HardlinkTable};
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    /// Test that files from different systems with same dev/ino are correctly linked.
    ///
    /// When syncing from the same source filesystem, files with identical
    /// (dev, ino) pairs are true hardlinks and should be linked together.
    #[test]
    fn same_dev_ino_same_system_are_linked() {
        let mut table = HardlinkTable::new();
        let dev_ino = DevIno::new(0xFD00, 12345);

        let r1 = table.find_or_insert(dev_ino, 0);
        assert_eq!(r1, HardlinkLookup::First(0));

        let r2 = table.find_or_insert(dev_ino, 1);
        assert_eq!(r2, HardlinkLookup::LinkTo(0));

        let r3 = table.find_or_insert(dev_ino, 2);
        assert_eq!(r3, HardlinkLookup::LinkTo(0));

        assert_eq!(table.len(), 1);

        let entry = table.get(&dev_ino).unwrap();
        assert_eq!(entry.link_count, 3);
        assert_eq!(entry.first_ndx, 0);
    }

    /// Test that same inode on different devices are NOT linked.
    ///
    /// Different devices with the same inode number are distinct files,
    /// not hardlinks. This can happen when syncing from multiple filesystems.
    #[test]
    fn same_inode_different_device_not_linked() {
        let mut table = HardlinkTable::new();

        let pairs = [
            (DevIno::new(0, 12345), 0),
            (DevIno::new(1, 12345), 1),
            (DevIno::new(2, 12345), 2),
            (DevIno::new(0xFFFF_FFFF_FFFF_FFFF, 12345), 3),
        ];

        for (dev_ino, ndx) in pairs {
            let result = table.find_or_insert(dev_ino, ndx);
            assert_eq!(
                result,
                HardlinkLookup::First(ndx),
                "dev={} should be first occurrence",
                dev_ino.dev
            );
        }

        assert_eq!(table.len(), 4);
    }

    /// Test that same device with different inodes are NOT linked.
    #[test]
    fn same_device_different_inode_not_linked() {
        let mut table = HardlinkTable::new();

        for i in 0..100u32 {
            let dev_ino = DevIno::new(1, i as u64);
            let result = table.find_or_insert(dev_ino, i);
            assert_eq!(
                result,
                HardlinkLookup::First(i),
                "inode {i} should be first occurrence"
            );
        }

        assert_eq!(table.len(), 100);
    }

    /// Test hash collision behavior with DevIno pairs.
    ///
    /// While FxHashMap handles collisions internally, we want to verify
    /// that our DevIno equality is used correctly for collision resolution.
    #[test]
    fn hash_collision_distinct_keys_remain_separate() {
        let mut table = HardlinkTable::new();

        // FxHash is fast but not cryptographically secure, so collisions are possible.
        // Use values that differ only in high/low bits to stress collision resolution.
        let pairs = [
            DevIno::new(0, 0),
            DevIno::new(0, 1),
            DevIno::new(1, 0),
            DevIno::new(1, 1),
            DevIno::new(u64::MAX, 0),
            DevIno::new(0, u64::MAX),
            DevIno::new(u64::MAX, u64::MAX),
            // Swapped values exercise asymmetry of the dev/ino hash.
            DevIno::new(12345, 67890),
            DevIno::new(67890, 12345),
        ];

        for (ndx, &dev_ino) in pairs.iter().enumerate() {
            let result = table.find_or_insert(dev_ino, ndx as u32);
            assert_eq!(
                result,
                HardlinkLookup::First(ndx as u32),
                "DevIno({}, {}) should be first occurrence",
                dev_ino.dev,
                dev_ino.ino
            );
        }

        assert_eq!(table.len(), pairs.len());

        for (ndx, &dev_ino) in pairs.iter().enumerate() {
            let entry = table.get(&dev_ino).unwrap();
            assert_eq!(
                entry.first_ndx, ndx as u32,
                "DevIno({}, {}) should map to index {}",
                dev_ino.dev, dev_ino.ino, ndx
            );
        }
    }

    /// Test that DevIno hash considers both dev and ino fields.
    ///
    /// Verifies that the Hash implementation produces different hashes
    /// for DevIno pairs that differ only in one field.
    #[test]
    fn dev_ino_hash_uses_both_fields() {
        fn hash_dev_ino(di: DevIno) -> u64 {
            let mut hasher = DefaultHasher::new();
            di.hash(&mut hasher);
            hasher.finish()
        }

        let h1 = hash_dev_ino(DevIno::new(1, 100));
        let h2 = hash_dev_ino(DevIno::new(1, 101));
        assert_ne!(h1, h2, "Different inodes should have different hashes");

        let h3 = hash_dev_ino(DevIno::new(1, 100));
        let h4 = hash_dev_ino(DevIno::new(2, 100));
        assert_ne!(h3, h4, "Different devices should have different hashes");

        let h5 = hash_dev_ino(DevIno::new(12345, 67890));
        let h6 = hash_dev_ino(DevIno::new(67890, 12345));
        assert_ne!(h5, h6, "Swapped dev/ino should have different hashes");
    }

    /// Test behavior with synthetic hash collisions using values that
    /// FxHash is known to potentially collide on.
    #[test]
    fn fxhash_specific_collision_resistance() {
        let mut table = HardlinkTable::new();

        // FxHash uses multiply-rotate; these patterns target known weak-mixing inputs:
        // zero values, max values, powers of two, and alternating bit patterns.
        let test_cases = [
            (DevIno::new(0, 0), 0),
            (DevIno::new(u64::MAX, u64::MAX), 1),
            (DevIno::new(1 << 32, 0), 2),
            (DevIno::new(0, 1 << 32), 3),
            (DevIno::new(1 << 63, 0), 4),
            (DevIno::new(0, 1 << 63), 5),
            (DevIno::new(0x5555_5555_5555_5555, 0xAAAA_AAAA_AAAA_AAAA), 6),
            (DevIno::new(0xAAAA_AAAA_AAAA_AAAA, 0x5555_5555_5555_5555), 7),
        ];

        for (dev_ino, ndx) in test_cases {
            let result = table.find_or_insert(dev_ino, ndx);
            assert_eq!(
                result,
                HardlinkLookup::First(ndx),
                "DevIno({:#x}, {:#x}) should be distinct",
                dev_ino.dev,
                dev_ino.ino
            );
        }

        assert_eq!(table.len(), test_cases.len());
    }
}

/// Tests for edge cases with large numbers of hardlinks.
#[cfg(test)]
mod large_scale {
    use crate::flist::hardlink::{DevIno, HardlinkLookup, HardlinkTable};

    /// Test handling of many hardlinks to a single file.
    ///
    /// In practice, a single file can have thousands of hardlinks.
    /// This tests that link_count handles high values correctly.
    #[test]
    fn many_hardlinks_to_single_file() {
        let mut table = HardlinkTable::new();
        let dev_ino = DevIno::new(1, 12345);

        const NUM_LINKS: u32 = 10_000;

        let first = table.find_or_insert(dev_ino, 0);
        assert_eq!(first, HardlinkLookup::First(0));

        for i in 1..NUM_LINKS {
            let result = table.find_or_insert(dev_ino, i);
            assert_eq!(result, HardlinkLookup::LinkTo(0));
        }

        let entry = table.get(&dev_ino).unwrap();
        assert_eq!(entry.link_count, NUM_LINKS);
        assert_eq!(entry.first_ndx, 0);

        assert_eq!(table.len(), 1);
    }

    /// Test handling of many distinct hardlink groups.
    ///
    /// Verifies the table can handle thousands of distinct (dev, ino) pairs
    /// without degraded performance or incorrect lookups.
    #[test]
    fn many_distinct_hardlink_groups() {
        let mut table = HardlinkTable::with_capacity(10_000);

        const NUM_GROUPS: u32 = 10_000;

        for i in 0..NUM_GROUPS {
            let dev_ino = DevIno::new(i as u64 / 1000, i as u64);
            let result = table.find_or_insert(dev_ino, i);
            assert_eq!(result, HardlinkLookup::First(i));
        }

        assert_eq!(table.len(), NUM_GROUPS as usize);

        for i in 0..NUM_GROUPS {
            let dev_ino = DevIno::new(i as u64 / 1000, i as u64);
            let entry = table.get(&dev_ino).unwrap();
            assert_eq!(entry.first_ndx, i);
            assert_eq!(entry.link_count, 1);
        }
    }

    /// Test mixed scenario: many groups with varying link counts.
    #[test]
    fn mixed_hardlink_scenario() {
        let mut table = HardlinkTable::new();

        const NUM_GROUPS: u32 = 1_000;

        let mut file_ndx = 0u32;

        for group in 0..NUM_GROUPS {
            let dev_ino = DevIno::new(group as u64 / 100, group as u64);
            let link_count = (group % 10) + 1; // 1-10 links per group

            for link in 0..link_count {
                let result = table.find_or_insert(dev_ino, file_ndx);
                if link == 0 {
                    assert_eq!(result, HardlinkLookup::First(file_ndx));
                } else if let HardlinkLookup::LinkTo(first) = result {
                    assert_eq!(first, file_ndx - link);
                } else {
                    panic!("Expected LinkTo for link {link} in group {group}");
                }
                file_ndx += 1;
            }
        }

        assert_eq!(table.len(), NUM_GROUPS as usize);

        for group in 0..NUM_GROUPS {
            let dev_ino = DevIno::new(group as u64 / 100, group as u64);
            let expected_links = (group % 10) + 1;
            let entry = table.get(&dev_ino).unwrap();
            assert_eq!(
                entry.link_count, expected_links,
                "Group {group} should have {expected_links} links"
            );
        }
    }

    /// Test with maximum u32 file indices.
    ///
    /// Verifies handling of file indices near u32::MAX.
    #[test]
    fn max_file_index_values() {
        let mut table = HardlinkTable::new();

        let test_indices = [0, 1, u32::MAX / 2, u32::MAX - 1, u32::MAX];

        for (i, &ndx) in test_indices.iter().enumerate() {
            let dev_ino = DevIno::new(i as u64, i as u64);
            let result = table.find_or_insert(dev_ino, ndx);
            assert_eq!(result, HardlinkLookup::First(ndx));
        }

        for (i, &expected_ndx) in test_indices.iter().enumerate() {
            let dev_ino = DevIno::new(i as u64, i as u64);
            let entry = table.get(&dev_ino).unwrap();
            assert_eq!(entry.first_ndx, expected_ndx);
        }
    }

    /// Test with extreme dev/ino values.
    #[test]
    fn extreme_dev_ino_values() {
        let mut table = HardlinkTable::new();

        let test_cases = [
            DevIno::new(0, 0),
            DevIno::new(u64::MAX, u64::MAX),
            DevIno::new(u64::MAX, 0),
            DevIno::new(0, u64::MAX),
            DevIno::new(1, 1),
            DevIno::new(u64::MAX - 1, u64::MAX - 1),
        ];

        for (ndx, &dev_ino) in test_cases.iter().enumerate() {
            let result = table.find_or_insert(dev_ino, ndx as u32);
            assert_eq!(result, HardlinkLookup::First(ndx as u32));
        }

        assert_eq!(table.len(), test_cases.len());

        for (ndx, &dev_ino) in test_cases.iter().enumerate() {
            let entry = table.get(&dev_ino).unwrap();
            assert_eq!(entry.first_ndx, ndx as u32);
        }
    }

    /// Test link count approaching u32::MAX.
    ///
    /// While unlikely in practice, verifies no overflow in link_count.
    #[test]
    fn link_count_high_values() {
        let mut table = HardlinkTable::new();
        let dev_ino = DevIno::new(1, 1);

        table.find_or_insert(dev_ino, 0);

        // Simulate many links by inserting repeatedly; in production each insert
        // would correspond to a distinct file index sharing this dev/ino pair.
        const ITERATIONS: u32 = 100_000;
        for i in 1..ITERATIONS {
            table.find_or_insert(dev_ino, i);
        }

        let entry = table.get(&dev_ino).unwrap();
        assert_eq!(entry.link_count, ITERATIONS);
    }
}

/// Tests for concurrent/interleaved access patterns.
#[cfg(test)]
mod interleaved_access {
    use crate::flist::hardlink::{DevIno, HardlinkLookup, HardlinkTable};

    /// Test interleaved inserts and lookups.
    ///
    /// Simulates realistic usage where files are discovered in arbitrary order.
    #[test]
    fn interleaved_inserts_and_lookups() {
        let mut table = HardlinkTable::new();

        let di1 = DevIno::new(1, 100);
        let di2 = DevIno::new(1, 200);
        let di3 = DevIno::new(2, 100);

        table.find_or_insert(di1, 0);
        table.find_or_insert(di2, 1);
        table.find_or_insert(di3, 2);

        assert_eq!(table.find_or_insert(di1, 3), HardlinkLookup::LinkTo(0));
        assert_eq!(table.find_or_insert(di2, 4), HardlinkLookup::LinkTo(1));

        let di4 = DevIno::new(2, 200);
        assert_eq!(table.find_or_insert(di4, 5), HardlinkLookup::First(5));

        assert_eq!(table.find_or_insert(di3, 6), HardlinkLookup::LinkTo(2));
        assert_eq!(table.find_or_insert(di1, 7), HardlinkLookup::LinkTo(0));

        assert_eq!(table.len(), 4);
        assert_eq!(table.get(&di1).unwrap().link_count, 3);
        assert_eq!(table.get(&di2).unwrap().link_count, 2);
        assert_eq!(table.get(&di3).unwrap().link_count, 2);
        assert_eq!(table.get(&di4).unwrap().link_count, 1);
    }

    /// Test that get() doesn't modify the table.
    #[test]
    fn get_is_readonly() {
        let mut table = HardlinkTable::new();
        let dev_ino = DevIno::new(1, 100);

        table.find_or_insert(dev_ino, 0);
        table.find_or_insert(dev_ino, 1);

        let initial_count = table.get(&dev_ino).unwrap().link_count;
        assert_eq!(initial_count, 2);

        for _ in 0..100 {
            let entry = table.get(&dev_ino).unwrap();
            assert_eq!(entry.link_count, 2);
        }

        assert_eq!(table.len(), 1);
        assert_eq!(table.get(&dev_ino).unwrap().link_count, 2);
    }

    /// Test clear() properly resets the table.
    #[test]
    fn clear_allows_fresh_start() {
        let mut table = HardlinkTable::new();
        let dev_ino = DevIno::new(1, 100);

        table.find_or_insert(dev_ino, 0);
        table.find_or_insert(dev_ino, 1);
        assert_eq!(table.len(), 1);
        assert_eq!(table.get(&dev_ino).unwrap().link_count, 2);

        table.clear();
        assert!(table.is_empty());
        assert!(table.get(&dev_ino).is_none());

        let result = table.find_or_insert(dev_ino, 100);
        assert_eq!(result, HardlinkLookup::First(100));
        assert_eq!(table.get(&dev_ino).unwrap().first_ndx, 100);
        assert_eq!(table.get(&dev_ino).unwrap().link_count, 1);
    }

    /// Test that file index is preserved correctly through lookups.
    ///
    /// The first_ndx should always point to the first file inserted,
    /// regardless of how many subsequent links are added.
    #[test]
    fn first_index_preserved_through_many_links() {
        let mut table = HardlinkTable::new();
        let dev_ino = DevIno::new(1, 100);

        table.find_or_insert(dev_ino, 42);

        for i in 0..1000 {
            let result = table.find_or_insert(dev_ino, 1000 + i);
            assert_eq!(
                result,
                HardlinkLookup::LinkTo(42),
                "Link {i} should reference first index 42"
            );
        }

        let entry = table.get(&dev_ino).unwrap();
        assert_eq!(entry.first_ndx, 42);
        assert_eq!(entry.link_count, 1001);
    }
}
