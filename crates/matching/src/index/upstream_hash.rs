//! Replays upstream's `hash_search()` probe accounting for the `-vv`
//! `total:` line.
//!
//! oc finds matches through its own bucket layout, tag table and bithash, so
//! its probes never visit upstream's `hash_table` buckets. The two counters
//! upstream prints are defined by that table, though: `hash_hits` counts the
//! offsets whose bucket is non-empty and `false_alarms` the chain candidates
//! whose weak sum matches but whose strong sum does not. This model rebuilds
//! that table and walks its chains at every offset upstream probes, so the
//! counters are upstream's own. The scan only feeds it while the counters are
//! printed (`DEBUG_GTE(DELTASUM, 1)`).

use checksums::RollingDigest;
use signature::DigestBuf;

use super::{DeltaSignatureIndex, MAX_CHAIN_LEN, ProbeCounters};

/// upstream: match.c:45 `TRADITIONAL_TABLESIZE`.
const TRADITIONAL_TABLESIZE: u32 = 1 << 16;

/// Sentinel ending a chain, upstream's `-1`.
const CHAIN_END: i32 = -1;

/// Upstream's per-file `hash_table` with its chains and counters.
#[derive(Debug)]
pub struct UpstreamHashTable {
    tablesize: u32,
    heads: Vec<i32>,
    chains: Vec<i32>,
    /// upstream: rsync.h `SUMFLG_SAME_OFFSET` - kept only for `--inplace`.
    same_offset: Vec<bool>,
    updating_basis_file: bool,
    block_length: u64,
    counters: ProbeCounters,
}

impl UpstreamHashTable {
    /// Builds the table for `index`, as `build_hash_table()` does.
    ///
    /// `updating_basis_file` is upstream's `--inplace` state, under which the
    /// chain walk unlinks blocks the write cursor has passed.
    ///
    /// upstream: match.c:84-110 `build_hash_table()`.
    #[must_use]
    pub fn new(index: &DeltaSignatureIndex, updating_basis_file: bool) -> Self {
        let count = index.blocks.len();
        let tablesize = upstream_table_size(count);
        let mut table = Self {
            tablesize,
            heads: vec![CHAIN_END; tablesize as usize],
            chains: vec![CHAIN_END; count],
            same_offset: vec![false; count],
            updating_basis_file,
            block_length: index.block_length as u64,
            counters: ProbeCounters::default(),
        };
        for (i, block) in index.blocks.iter().enumerate() {
            let bucket = table.bucket(block.rolling().value());
            table.chains[i] = table.heads[bucket];
            table.heads[bucket] = i as i32;
        }
        table
    }

    /// The counters accumulated so far.
    #[must_use]
    pub const fn counters(&self) -> ProbeCounters {
        self.counters
    }

    /// Records that block `block` matched at its own offset, which keeps it
    /// on its chain under `--inplace`.
    ///
    /// upstream: match.c:316 `s->sums[i].flags |= SUMFLG_SAME_OFFSET`.
    pub fn mark_same_offset(&mut self, block: usize, offset: u64) {
        if self.updating_basis_file && block as u64 * self.block_length == offset {
            self.same_offset[block] = true;
        }
    }

    fn bucket(&self, sum: u32) -> usize {
        upstream_bucket(sum, self.tablesize)
    }

    /// Replays the probe at `offset` for a `window_len`-byte window whose
    /// weak sum is `digest`. `strong` computes the window's strong sum; it is
    /// called at most once, and only when a candidate needs it.
    ///
    /// upstream: match.c:214-271 `hash_search()` - `hash_hits++` for a
    /// non-empty bucket, then the chain walk that counts `false_alarms` and
    /// stops at the first strong match or past [`MAX_CHAIN_LEN`].
    pub fn probe(
        &mut self,
        index: &DeltaSignatureIndex,
        digest: RollingDigest,
        window_len: usize,
        offset: u64,
        mut strong: impl FnMut() -> DigestBuf,
    ) {
        let sum = digest.value();
        let bucket = self.bucket(sum);
        let mut i = self.heads[bucket];
        if i == CHAIN_END {
            return;
        }
        self.counters.hash_hits = self.counters.hash_hits.saturating_add(1);

        let mut window_strong: Option<DigestBuf> = None;
        let mut prev: Option<usize> = None;
        let mut chain_len = 0u32;
        while i != CHAIN_END {
            let block_index = i as usize;
            let next = self.chains[block_index];
            if self.is_bypassed(block_index, offset) {
                // upstream: match.c:233-237 - `*prev = s->sums[i].chain`.
                match prev {
                    Some(p) => self.chains[p] = next,
                    None => self.heads[bucket] = next,
                }
                i = next;
                continue;
            }
            prev = Some(block_index);
            let block = index.block(block_index);
            if block.rolling().value() != sum {
                i = next;
                continue;
            }
            chain_len += 1;
            if chain_len > MAX_CHAIN_LEN {
                return;
            }
            if block.len() != window_len {
                i = next;
                continue;
            }
            let window_strong = window_strong.get_or_insert_with(&mut strong);
            if window_strong.as_slice() != block.strong() {
                self.counters.false_alarms = self.counters.false_alarms.saturating_add(1);
                i = next;
                continue;
            }
            return;
        }
    }

    /// [`Self::probe`] for a window held as two slices.
    pub fn probe_window(
        &mut self,
        index: &DeltaSignatureIndex,
        digest: RollingDigest,
        offset: u64,
        first: &[u8],
        second: &[u8],
    ) {
        self.probe(index, digest, first.len() + second.len(), offset, || {
            index
                .algorithm
                .compute_truncated_slices(first, second, index.strong_length)
        });
    }

    /// upstream: match.c:233 - under `--inplace` a block behind the write
    /// cursor is dropped unless it already matched at its own offset.
    fn is_bypassed(&self, block: usize, offset: u64) -> bool {
        self.updating_basis_file
            && (block as u64) * self.block_length < offset
            && !self.same_offset[block]
    }

    /// Replays the probes upstream makes past the last full window, when the
    /// final basis block is short: the window shrinks one byte per offset
    /// until it is the final block's length.
    ///
    /// `tail` holds the source bytes from `first_offset` to the end of the
    /// file, and `first_offset` is the first offset oc's scan did not probe.
    ///
    /// upstream: match.c:197 `end = len + 1 - s->sums[s->count-1].len` and
    /// match.c:362-364, where the window loses a byte (`--k`) once nothing
    /// is left to roll in.
    pub fn probe_shrinking_tail(
        &mut self,
        index: &DeltaSignatureIndex,
        tail: &[u8],
        first_offset: u64,
    ) {
        let Some(last) = index.blocks.last() else {
            return;
        };
        if tail.is_empty() {
            return;
        }
        let len = first_offset + tail.len() as u64;
        let end = (len + 1).saturating_sub(last.len() as u64);
        let mut offset = first_offset;
        while offset < end {
            let start = (offset - first_offset) as usize;
            let window = &tail[start..tail.len().min(start + index.block_length)];
            self.probe_window(
                index,
                RollingDigest::from_bytes(window),
                offset,
                window,
                &[],
            );
            offset += 1;
        }
    }
}

/// upstream: match.c:86-90 - sized for an 80% load on big files, never
/// below `TRADITIONAL_TABLESIZE`.
fn upstream_table_size(count: usize) -> u32 {
    u32::try_from(count / 8)
        .unwrap_or(u32::MAX)
        .saturating_mul(10)
        .saturating_add(11)
        .max(TRADITIONAL_TABLESIZE)
}

/// upstream: match.c:76-78 `SUM2HASH` for the traditional table,
/// `BIG_SUM2HASH` for a larger one.
fn upstream_bucket(sum: u32, tablesize: u32) -> usize {
    if tablesize == TRADITIONAL_TABLESIZE {
        (((sum & 0xFFFF) + (sum >> 16)) & 0xFFFF) as usize
    } else {
        (sum % tablesize) as usize
    }
}

#[cfg(test)]
#[path = "upstream_hash_tests.rs"]
mod tests;
