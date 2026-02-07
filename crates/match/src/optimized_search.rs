//! Optimized hash search algorithm matching upstream rsync's `match.c` approach.
//!
//! This module implements a two-level matching strategy for finding matching blocks
//! during delta transfer:
//! 1. Fast rolling checksum lookup with tag table for quick rejection
//! 2. Slow strong checksum verification for confirmed matches
//!
//! # Performance optimizations
//!
//! - **Tag table**: O(1) rejection of non-matching rolling checksums using low 16 bits
//! - **Hash table with chaining**: Fast lookup of blocks by rolling checksum
//! - **Sorted optimization**: Pre-sorted blocks enable sequential scan for large block sets
//!
//! # Example
//!
//! ```rust
//! use matching::optimized_search::{BlockEntry, BlockHashTable};
//!
//! let blocks = vec![
//!     BlockEntry {
//!         index: 0,
//!         checksum: 0x12345678,
//!         strong_checksum: vec![0xab, 0xcd, 0xef, 0x01],
//!         block_len: 4096,
//!     },
//!     BlockEntry {
//!         index: 1,
//!         checksum: 0x9abcdef0,
//!         strong_checksum: vec![0x12, 0x34, 0x56, 0x78],
//!         block_len: 4096,
//!     },
//! ];
//!
//! let table = BlockHashTable::new(blocks);
//! let strong = vec![0xab, 0xcd, 0xef, 0x01];
//! let matched = table.find_match(0x12345678, &strong);
//! assert!(matched.is_some());
//! ```

use rustc_hash::FxHashMap;

/// Size of the tag table for quick hash rejection (2^16 entries).
const TAG_TABLE_SIZE: usize = 1 << 16;

/// Hash table entry for signature block lookup.
#[derive(Debug, Clone)]
pub struct BlockEntry {
    /// Block index in the original file.
    pub index: u32,
    /// Weak rolling checksum (sum1 + sum2).
    pub checksum: u32,
    /// Strong checksum (MD4/MD5 digest).
    pub strong_checksum: Vec<u8>,
    /// Block length in bytes.
    pub block_len: u32,
}

/// Tag table for O(1) rejection of non-matching rolling checksums.
/// Maps the low 16 bits of a rolling checksum to whether any block has that tag.
#[derive(Debug)]
pub struct TagTable {
    tags: Vec<bool>,
}

impl TagTable {
    /// Build a tag table from a set of block entries.
    pub fn new(blocks: &[BlockEntry]) -> Self {
        let mut tags = vec![false; TAG_TABLE_SIZE];
        for block in blocks {
            let tag = (block.checksum & 0xFFFF) as usize;
            tags[tag] = true;
        }
        Self { tags }
    }

    /// Check if a rolling checksum might match any block (false = definitely no match).
    #[inline]
    pub fn might_match(&self, checksum: u32) -> bool {
        let tag = (checksum & 0xFFFF) as usize;
        self.tags[tag]
    }
}

/// Hash table for signature block lookup with chaining.
#[derive(Debug)]
pub struct BlockHashTable {
    /// Maps rolling checksum → list of block entries with that checksum.
    table: FxHashMap<u32, Vec<usize>>,
    /// All block entries.
    blocks: Vec<BlockEntry>,
    /// Tag table for quick rejection.
    tag_table: TagTable,
    /// Whether blocks are sorted by checksum (enables sequential optimization).
    sorted: bool,
}

impl BlockHashTable {
    /// Build a hash table from block entries.
    pub fn new(blocks: Vec<BlockEntry>) -> Self {
        let tag_table = TagTable::new(&blocks);
        let mut table = FxHashMap::default();

        // Build hash table with chaining
        for (idx, block) in blocks.iter().enumerate() {
            table.entry(block.checksum).or_insert_with(Vec::new).push(idx);
        }

        Self {
            table,
            blocks,
            tag_table,
            sorted: false,
        }
    }

    /// Build a hash table with pre-sorted blocks (enables sequential scan optimization).
    pub fn new_sorted(mut blocks: Vec<BlockEntry>) -> Self {
        // Sort blocks by rolling checksum
        blocks.sort_by_key(|b| b.checksum);

        let tag_table = TagTable::new(&blocks);
        let mut table = FxHashMap::default();

        // Build hash table with chaining
        for (idx, block) in blocks.iter().enumerate() {
            table.entry(block.checksum).or_insert_with(Vec::new).push(idx);
        }

        Self {
            table,
            blocks,
            tag_table,
            sorted: true,
        }
    }

    /// Find blocks matching a given rolling checksum.
    /// Returns indices into the blocks array.
    /// Uses tag table for O(1) rejection of non-matches.
    pub fn find_weak_matches(&self, checksum: u32) -> &[usize] {
        // First check tag table for quick rejection
        if !self.tag_table.might_match(checksum) {
            return &[];
        }

        // Look up in hash table
        self.table.get(&checksum).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Verify a weak match against a strong checksum.
    /// Returns the block entry if both checksums match.
    pub fn verify_match(&self, block_idx: usize, strong: &[u8]) -> Option<&BlockEntry> {
        let block = &self.blocks[block_idx];
        if block.strong_checksum == strong {
            Some(block)
        } else {
            None
        }
    }

    /// Find a matching block using two-level lookup:
    /// 1. Tag table quick rejection
    /// 2. Rolling checksum match
    /// 3. Strong checksum verification
    pub fn find_match(&self, rolling_checksum: u32, strong_checksum: &[u8]) -> Option<&BlockEntry> {
        // Get weak matches (includes tag table check)
        let weak_matches = self.find_weak_matches(rolling_checksum);

        // Verify each weak match against strong checksum
        for &block_idx in weak_matches {
            if let Some(block) = self.verify_match(block_idx, strong_checksum) {
                return Some(block);
            }
        }

        None
    }

    /// Number of blocks in the table.
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Whether blocks are sorted (sequential scan optimization enabled).
    pub fn is_sorted(&self) -> bool {
        self.sorted
    }
}

/// Statistics for match search operations.
#[derive(Debug, Clone, Default)]
pub struct MatchStats {
    /// Number of tag hits (passed tag table check).
    pub tag_hits: u64,
    /// Number of false tag hits (tag match but checksum mismatch).
    pub false_tag_hits: u64,
    /// Number of weak matches (rolling checksum matched).
    pub weak_matches: u64,
    /// Number of strong matches (both checksums matched).
    pub strong_matches: u64,
    /// Number of total lookups.
    pub total_lookups: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tag_table_empty() {
        let blocks = vec![];
        let tag_table = TagTable::new(&blocks);
        // All tags should be false for empty input
        assert!(!tag_table.might_match(0x12345678));
        assert!(!tag_table.might_match(0x00000000));
        assert!(!tag_table.might_match(0xFFFFFFFF));
    }

    #[test]
    fn test_tag_table_might_match() {
        let blocks = vec![
            BlockEntry {
                index: 0,
                checksum: 0x12345678,
                strong_checksum: vec![0xab, 0xcd],
                block_len: 4096,
            },
            BlockEntry {
                index: 1,
                checksum: 0x9abc1234,
                strong_checksum: vec![0xef, 0x01],
                block_len: 4096,
            },
        ];
        let tag_table = TagTable::new(&blocks);

        // Should match low 16 bits: 0x5678 and 0x1234
        assert!(tag_table.might_match(0x12345678));
        assert!(tag_table.might_match(0x9abc1234));
        // Any checksum with same low 16 bits should match
        assert!(tag_table.might_match(0xFFFF5678));
        assert!(tag_table.might_match(0x00001234));
    }

    #[test]
    fn test_tag_table_no_match() {
        let blocks = vec![BlockEntry {
            index: 0,
            checksum: 0x12345678,
            strong_checksum: vec![0xab, 0xcd],
            block_len: 4096,
        }];
        let tag_table = TagTable::new(&blocks);

        // Different low 16 bits should not match
        assert!(!tag_table.might_match(0x12341234));
        assert!(!tag_table.might_match(0xFFFFFFFF));
    }

    #[test]
    fn test_block_hash_table_new() {
        let blocks = vec![
            BlockEntry {
                index: 0,
                checksum: 0x12345678,
                strong_checksum: vec![0xab, 0xcd],
                block_len: 4096,
            },
            BlockEntry {
                index: 1,
                checksum: 0x9abcdef0,
                strong_checksum: vec![0xef, 0x01],
                block_len: 4096,
            },
        ];
        let table = BlockHashTable::new(blocks);

        assert_eq!(table.len(), 2);
        assert!(!table.is_empty());
        assert!(!table.is_sorted());
    }

    #[test]
    fn test_block_hash_table_sorted() {
        let blocks = vec![
            BlockEntry {
                index: 0,
                checksum: 0x9abcdef0,
                strong_checksum: vec![0xef, 0x01],
                block_len: 4096,
            },
            BlockEntry {
                index: 1,
                checksum: 0x12345678,
                strong_checksum: vec![0xab, 0xcd],
                block_len: 4096,
            },
        ];
        let table = BlockHashTable::new_sorted(blocks);

        assert_eq!(table.len(), 2);
        assert!(!table.is_empty());
        assert!(table.is_sorted());

        // Verify blocks are actually sorted
        for i in 1..table.blocks.len() {
            assert!(table.blocks[i - 1].checksum <= table.blocks[i].checksum);
        }
    }

    #[test]
    fn test_find_weak_matches() {
        let blocks = vec![
            BlockEntry {
                index: 0,
                checksum: 0x12345678,
                strong_checksum: vec![0xab, 0xcd],
                block_len: 4096,
            },
            BlockEntry {
                index: 1,
                checksum: 0x9abcdef0,
                strong_checksum: vec![0xef, 0x01],
                block_len: 4096,
            },
        ];
        let table = BlockHashTable::new(blocks);

        let matches = table.find_weak_matches(0x12345678);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], 0);

        let matches = table.find_weak_matches(0x9abcdef0);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], 1);
    }

    #[test]
    fn test_find_weak_matches_no_match() {
        let blocks = vec![BlockEntry {
            index: 0,
            checksum: 0x12345678,
            strong_checksum: vec![0xab, 0xcd],
            block_len: 4096,
        }];
        let table = BlockHashTable::new(blocks);

        let matches = table.find_weak_matches(0xFFFFFFFF);
        assert_eq!(matches.len(), 0);
    }

    #[test]
    fn test_verify_match_correct() {
        let blocks = vec![BlockEntry {
            index: 0,
            checksum: 0x12345678,
            strong_checksum: vec![0xab, 0xcd, 0xef, 0x01],
            block_len: 4096,
        }];
        let table = BlockHashTable::new(blocks);

        let strong = vec![0xab, 0xcd, 0xef, 0x01];
        let result = table.verify_match(0, &strong);
        assert!(result.is_some());
        assert_eq!(result.unwrap().index, 0);
    }

    #[test]
    fn test_verify_match_wrong_strong() {
        let blocks = vec![BlockEntry {
            index: 0,
            checksum: 0x12345678,
            strong_checksum: vec![0xab, 0xcd, 0xef, 0x01],
            block_len: 4096,
        }];
        let table = BlockHashTable::new(blocks);

        let wrong_strong = vec![0xff, 0xff, 0xff, 0xff];
        let result = table.verify_match(0, &wrong_strong);
        assert!(result.is_none());
    }

    #[test]
    fn test_find_match_full() {
        let blocks = vec![
            BlockEntry {
                index: 0,
                checksum: 0x12345678,
                strong_checksum: vec![0xab, 0xcd, 0xef, 0x01],
                block_len: 4096,
            },
            BlockEntry {
                index: 1,
                checksum: 0x9abcdef0,
                strong_checksum: vec![0x12, 0x34, 0x56, 0x78],
                block_len: 4096,
            },
        ];
        let table = BlockHashTable::new(blocks);

        let strong = vec![0xab, 0xcd, 0xef, 0x01];
        let matched = table.find_match(0x12345678, &strong);
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().index, 0);

        let strong2 = vec![0x12, 0x34, 0x56, 0x78];
        let matched2 = table.find_match(0x9abcdef0, &strong2);
        assert!(matched2.is_some());
        assert_eq!(matched2.unwrap().index, 1);
    }

    #[test]
    fn test_find_match_tag_reject() {
        let blocks = vec![BlockEntry {
            index: 0,
            checksum: 0x12345678,
            strong_checksum: vec![0xab, 0xcd, 0xef, 0x01],
            block_len: 4096,
        }];
        let table = BlockHashTable::new(blocks);

        // Checksum with different low 16 bits should be rejected by tag table
        let strong = vec![0xab, 0xcd, 0xef, 0x01];
        let matched = table.find_match(0x12341234, &strong);
        assert!(matched.is_none());
    }

    #[test]
    fn test_find_match_weak_but_not_strong() {
        let blocks = vec![BlockEntry {
            index: 0,
            checksum: 0x12345678,
            strong_checksum: vec![0xab, 0xcd, 0xef, 0x01],
            block_len: 4096,
        }];
        let table = BlockHashTable::new(blocks);

        // Correct rolling checksum but wrong strong checksum
        let wrong_strong = vec![0xff, 0xff, 0xff, 0xff];
        let matched = table.find_match(0x12345678, &wrong_strong);
        assert!(matched.is_none());
    }

    #[test]
    fn test_collision_handling() {
        // Multiple blocks with the same rolling checksum
        let blocks = vec![
            BlockEntry {
                index: 0,
                checksum: 0x12345678,
                strong_checksum: vec![0xaa, 0xaa],
                block_len: 4096,
            },
            BlockEntry {
                index: 1,
                checksum: 0x12345678, // Same rolling checksum
                strong_checksum: vec![0xbb, 0xbb],
                block_len: 4096,
            },
            BlockEntry {
                index: 2,
                checksum: 0x12345678, // Same rolling checksum
                strong_checksum: vec![0xcc, 0xcc],
                block_len: 4096,
            },
        ];
        let table = BlockHashTable::new(blocks);

        // Should find all weak matches
        let weak_matches = table.find_weak_matches(0x12345678);
        assert_eq!(weak_matches.len(), 3);

        // Should find the correct block by strong checksum
        let matched = table.find_match(0x12345678, &[0xbb, 0xbb]);
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().index, 1);
    }

    #[test]
    fn test_match_stats_default() {
        let stats = MatchStats::default();
        assert_eq!(stats.tag_hits, 0);
        assert_eq!(stats.false_tag_hits, 0);
        assert_eq!(stats.weak_matches, 0);
        assert_eq!(stats.strong_matches, 0);
        assert_eq!(stats.total_lookups, 0);
    }

    #[test]
    fn test_large_block_set() {
        // Create 10000 blocks with unique checksums
        let mut blocks = Vec::new();
        for i in 0..10000 {
            blocks.push(BlockEntry {
                index: i as u32,
                checksum: i * 1000,
                strong_checksum: vec![(i >> 8) as u8, (i & 0xFF) as u8],
                block_len: 4096,
            });
        }
        let table = BlockHashTable::new(blocks);

        assert_eq!(table.len(), 10000);

        // Verify we can find specific blocks
        let matched = table.find_match(5000 * 1000, &[(5000 >> 8) as u8, (5000 & 0xFF) as u8]);
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().index, 5000);

        // Verify non-existent block returns None
        let not_matched = table.find_match(0xFFFFFFFF, &[0xFF, 0xFF]);
        assert!(not_matched.is_none());

        // Test sorted variant with large block set
        let mut blocks2 = Vec::new();
        for i in 0..10000 {
            blocks2.push(BlockEntry {
                index: i as u32,
                checksum: (10000 - i) * 1000, // Reverse order
                strong_checksum: vec![(i >> 8) as u8, (i & 0xFF) as u8],
                block_len: 4096,
            });
        }
        let table_sorted = BlockHashTable::new_sorted(blocks2);

        assert_eq!(table_sorted.len(), 10000);
        assert!(table_sorted.is_sorted());

        // Verify we can still find blocks after sorting
        let matched = table_sorted.find_match(5000 * 1000, &[(5000 >> 8) as u8, (5000 & 0xFF) as u8]);
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().index, 5000);
    }
}
