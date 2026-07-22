//! Two-level block match search mirroring upstream rsync's `match.c`.
//!
//! Each lookup first rejects via the tag table (low 16 bits of the rolling
//! checksum), then probes the hash table keyed by full rolling checksum, then
//! verifies with the strong checksum. Lookups always resolve through the hash
//! table; the sorted constructor records that blocks were pre-sorted by
//! checksum but does not add a distinct sequential-scan lookup path.

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
    /// Builds a tag table from a set of block entries.
    pub fn new(blocks: &[BlockEntry]) -> Self {
        let mut tags = vec![false; TAG_TABLE_SIZE];
        for block in blocks {
            let tag = (block.checksum & 0xFFFF) as usize;
            tags[tag] = true;
        }
        Self { tags }
    }

    /// Returns `true` if a rolling checksum may match an indexed block.
    ///
    /// A `false` return is definitive; `true` may be a false positive that
    /// the strong checksum will then reject.
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
    /// Whether blocks were pre-sorted by checksum in the constructor.
    sorted: bool,
}

impl BlockHashTable {
    /// Builds a hash table from block entries.
    pub fn new(blocks: Vec<BlockEntry>) -> Self {
        let tag_table = TagTable::new(&blocks);
        let mut table = FxHashMap::default();

        for (idx, block) in blocks.iter().enumerate() {
            table
                .entry(block.checksum)
                .or_insert_with(Vec::new)
                .push(idx);
        }

        Self {
            table,
            blocks,
            tag_table,
            sorted: false,
        }
    }

    /// Builds a hash table from blocks pre-sorted by checksum.
    ///
    /// Sorting is recorded via [`Self::is_sorted`]; lookups still resolve
    /// through the hash table exactly as [`Self::new`] does.
    pub fn new_sorted(mut blocks: Vec<BlockEntry>) -> Self {
        blocks.sort_by_key(|b| b.checksum);

        let tag_table = TagTable::new(&blocks);
        let mut table = FxHashMap::default();

        for (idx, block) in blocks.iter().enumerate() {
            table
                .entry(block.checksum)
                .or_insert_with(Vec::new)
                .push(idx);
        }

        Self {
            table,
            blocks,
            tag_table,
            sorted: true,
        }
    }

    /// Returns indices of blocks whose rolling checksum equals `checksum`.
    ///
    /// The tag table rejects non-matches in O(1) before the hash probe.
    pub fn find_weak_matches(&self, checksum: u32) -> &[usize] {
        if !self.tag_table.might_match(checksum) {
            return &[];
        }

        self.table
            .get(&checksum)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Returns the block entry if its strong checksum equals `strong`.
    pub fn verify_match(&self, block_idx: usize, strong: &[u8]) -> Option<&BlockEntry> {
        let block = &self.blocks[block_idx];
        if block.strong_checksum == strong {
            Some(block)
        } else {
            None
        }
    }

    /// Finds a matching block via tag-table rejection, rolling-checksum
    /// lookup, then strong-checksum verification.
    pub fn find_match(&self, rolling_checksum: u32, strong_checksum: &[u8]) -> Option<&BlockEntry> {
        let weak_matches = self.find_weak_matches(rolling_checksum);

        for &block_idx in weak_matches {
            if let Some(block) = self.verify_match(block_idx, strong_checksum) {
                return Some(block);
            }
        }

        None
    }

    /// Returns the number of blocks in the table.
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Returns `true` if the table contains no blocks.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Returns `true` if blocks were pre-sorted by checksum in the constructor.
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

        assert!(tag_table.might_match(0x12345678));
        assert!(tag_table.might_match(0x9abc1234));
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

        let wrong_strong = vec![0xff, 0xff, 0xff, 0xff];
        let matched = table.find_match(0x12345678, &wrong_strong);
        assert!(matched.is_none());
    }

    #[test]
    fn test_collision_handling() {
        let blocks = vec![
            BlockEntry {
                index: 0,
                checksum: 0x12345678,
                strong_checksum: vec![0xaa, 0xaa],
                block_len: 4096,
            },
            BlockEntry {
                index: 1,
                checksum: 0x12345678,
                strong_checksum: vec![0xbb, 0xbb],
                block_len: 4096,
            },
            BlockEntry {
                index: 2,
                checksum: 0x12345678,
                strong_checksum: vec![0xcc, 0xcc],
                block_len: 4096,
            },
        ];
        let table = BlockHashTable::new(blocks);

        let weak_matches = table.find_weak_matches(0x12345678);
        assert_eq!(weak_matches.len(), 3);

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
        let mut blocks = Vec::new();
        for i in 0..10000 {
            blocks.push(BlockEntry {
                index: i,
                checksum: i * 1000,
                strong_checksum: vec![(i >> 8) as u8, (i & 0xFF) as u8],
                block_len: 4096,
            });
        }
        let table = BlockHashTable::new(blocks);

        assert_eq!(table.len(), 10000);

        let matched = table.find_match(5000 * 1000, &[(5000 >> 8) as u8, (5000 & 0xFF) as u8]);
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().index, 5000);

        let not_matched = table.find_match(0xFFFFFFFF, &[0xFF, 0xFF]);
        assert!(not_matched.is_none());

        let mut blocks2 = Vec::new();
        for i in 0..10000 {
            blocks2.push(BlockEntry {
                index: i,
                // Insert in reverse order to exercise the sorted constructor.
                checksum: (10000 - i) * 1000,
                strong_checksum: vec![(i >> 8) as u8, (i & 0xFF) as u8],
                block_len: 4096,
            });
        }
        let table_sorted = BlockHashTable::new_sorted(blocks2);

        assert_eq!(table_sorted.len(), 10000);
        assert!(table_sorted.is_sorted());

        let matched =
            table_sorted.find_match(5000 * 1000, &[(5000 >> 8) as u8, (5000 & 0xFF) as u8]);
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().index, 5000);
    }
}
