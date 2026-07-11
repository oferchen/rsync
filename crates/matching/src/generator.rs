//! Delta token generation pipeline.
//!
//! # DEBUG_DELTASUM Tracing Levels
//!
//! This module implements rsync-compatible DEBUG_DELTASUM tracing at 4 levels:
//!
//! - **Level 1**: Basic delta operations summary (match_report equivalent)
//! - **Level 2**: Workflow milestones (hash search start/end, statistics)
//! - **Level 3**: Detailed checksum information (potential matches, search params)
//! - **Level 4**: Per-iteration offset tracking (very verbose)

use std::io::{self, Cursor, Read};

use checksums::RollingChecksum;
use logging::debug_log;
use rayon::prelude::*;

#[cfg(feature = "tracing")]
use tracing::instrument;

use crate::index::{DeltaSignatureIndex, MatchedBlocks};
use crate::ring_buffer::RingBuffer;
use crate::script::{DeltaScript, DeltaToken};

/// Default buffer size used by [`DeltaGenerator::generate`].
const DEFAULT_BUFFER_LEN: usize = 128 * 1024;

/// Literal flush threshold matching upstream rsync's `CHUNK_SIZE` (32 KiB).
///
/// When pending literals accumulate beyond `block_length + CHUNK_SIZE` bytes,
/// they are flushed early to bound memory usage. Without this, a large
/// unmatched region would grow `pending_literals` to the entire file size.
///
/// upstream: rsync.h:158, match.c:339
const CHUNK_SIZE: usize = 32 * 1024;

/// Minimum bytes per parallel range in [`DeltaGenerator::generate_chunked`].
///
/// Ranges below this size are not worth a rayon task: the per-range boundary
/// loss (a straddling block degrades to literals) and task overhead would
/// outweigh the scan parallelism. The effective floor is the larger of this
/// and 64 basis blocks. See `docs/design/zsync-seq-match.md`.
const MIN_PARALLEL_CHUNK_BYTES: usize = 1024 * 1024;

/// Emits a coalesced `DeltaToken::Copy` covering an open seq-match run.
///
/// The seq-match optimization tracks `(start_basis_idx, run_len)` while the
/// chain loop confirms adjacent matches. When the run breaks - either at a
/// non-adjacent match, a literal break, or chain end - this helper flushes
/// the accumulated run as a single fat Copy token (`len = run_len *
/// block_len`) and resets the tracking state. The wire layer expands fat
/// Copy tokens into one wire op per basis block, preserving wire-format
/// byte-equality with the no-coalesce baseline.
fn flush_seq_match_run(
    tokens: &mut Vec<DeltaToken>,
    start: &mut Option<u64>,
    run_len: &mut usize,
    block_len: usize,
) {
    if let Some(start_idx) = start.take() {
        if *run_len > 0 {
            tokens.push(DeltaToken::Copy {
                index: start_idx,
                len: *run_len * block_len,
            });
        }
    }
    *run_len = 0;
}

/// Produces rsync-style delta tokens by comparing an input stream against a signature index.
#[derive(Clone, Debug)]
pub struct DeltaGenerator {
    buffer_len: usize,
    /// Test-only knob disabling the matched-block pruning bitmap so the
    /// property tests can compare prune-on against prune-off output. The
    /// production path always prunes; see `docs/design/zsync-prune.md`.
    #[cfg(any(test, feature = "bench-internal"))]
    prune_matched: bool,
}

impl DeltaGenerator {
    /// Creates a new generator with default buffering.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer_len: DEFAULT_BUFFER_LEN,
            #[cfg(any(test, feature = "bench-internal"))]
            prune_matched: true,
        }
    }

    /// Overrides the buffer length used when reading from the input stream.
    #[must_use]
    pub fn with_buffer_len(mut self, buffer_len: usize) -> Self {
        self.buffer_len = buffer_len.max(1);
        self
    }

    /// Test-only switch that disables the matched-block pruning bitmap.
    ///
    /// Used by the property tests in `matched_blocks_tests.rs` to compare
    /// prune-on output against the no-prune baseline. Not exposed in
    /// production builds.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_prune_matched(mut self, enabled: bool) -> Self {
        self.prune_matched = enabled;
        self
    }

    /// Bench-only switch mirroring [`Self::with_prune_matched`], used by
    /// the harness in `crates/matching/benches/prune_duplicate_heavy.rs`
    /// to compare prune-on against prune-off match throughput on
    /// duplicate-heavy basis data.
    ///
    /// Behind the internal `bench-internal` feature flag so the surface
    /// never reaches release builds. See `docs/design/zsync-prune.md`
    /// benchmark plan binding (#2071) for the methodology.
    #[cfg(all(not(test), feature = "bench-internal"))]
    #[must_use]
    pub fn with_prune_matched(mut self, enabled: bool) -> Self {
        self.prune_matched = enabled;
        self
    }

    /// Generates a [`DeltaScript`] describing how to reconstruct the input from basis blocks.
    ///
    /// This implements rsync's delta generation algorithm:
    ///
    /// 1. Slide a window of `block_length` bytes over the input
    /// 2. At each position, compute the rolling checksum
    /// 3. If the checksum matches a known block, verify with the strong checksum
    /// 4. On match: emit a `Copy` token referencing the basis block
    /// 5. On no match: accumulate the byte as a literal and advance by 1
    ///
    /// # Arguments
    ///
    /// * `reader` - Source data to generate delta for
    /// * `index` - Pre-built signature index from the basis file
    ///
    /// # Returns
    ///
    /// A [`DeltaScript`] containing `Copy` and `Literal` tokens that, when applied
    /// to the basis file, reconstruct the input.
    ///
    /// # Upstream Reference
    ///
    /// See `match.c:hash_search()` for the matching algorithm.
    pub fn generate<R: Read>(
        &self,
        reader: R,
        index: &DeltaSignatureIndex,
    ) -> io::Result<DeltaScript> {
        #[cfg(any(test, feature = "bench-internal"))]
        let prune_matched = self.prune_matched;
        #[cfg(not(any(test, feature = "bench-internal")))]
        let prune_matched = true;
        self.generate_with_prune(reader, index, prune_matched)
    }

    /// Core single-stream delta scan, parameterized on whether the shared
    /// `index` consumed-bitset pruning is engaged.
    ///
    /// Production [`Self::generate`] always prunes. [`Self::generate_chunked`]
    /// passes `prune_matched = false`: with pruning off this routine performs
    /// only read-only lookups on `index` (no [`DeltaSignatureIndex::mark_consumed`]
    /// or [`DeltaSignatureIndex::reset_consumed`]), so a shared `&index` is safe
    /// to scan from multiple rayon workers concurrently.
    fn generate_with_prune<R: Read>(
        &self,
        mut reader: R,
        index: &DeltaSignatureIndex,
        prune_matched: bool,
    ) -> io::Result<DeltaScript> {
        let block_len = index.block_length();
        let mut window = RingBuffer::with_capacity(block_len);
        let mut pending_literals = Vec::with_capacity(block_len);
        let mut rolling = RollingChecksum::new();
        let mut tokens = Vec::new();
        let mut total_bytes = 0u64;
        let mut literal_bytes = 0u64;

        let mut hash_hits = 0u64;
        let mut false_alarms = 0u64;
        let mut matches = 0u64;
        let mut offset = 0u64;

        // upstream: match.c - `want_i` tracks the expected next block index
        // for adjacent-match hinting. After a confirmed match at block `i`,
        // the next match is most often at `index.next_match(i)`: zsync's
        // librcksum/rsum.c:262 maintains the same lookahead slot. Seeding
        // with `Some(0)` covers the start-of-file case where block 0 is the
        // most likely first match. Probing the hint before the hash table
        // lookup skips the probe entirely when data is sequential.
        let mut want_i: Option<usize> = Some(0);

        // zsync-inspired matched-block pruning: each emitted Copy token sets a
        // bit so later probes skip the strong-checksum verify on already-
        // consumed basis blocks. Duplicate-content siblings live at distinct
        // basis indices; the bitmap leaves those siblings findable until each
        // is consumed independently. See docs/design/zsync-prune.md.
        let mut matched_blocks = MatchedBlocks::with_block_count(index.block_count());
        // ZSO-3: the shared consumed-bitset on `index` survives across
        // generator sessions when callers reuse the same index. Reset
        // it now so each `generate()` call starts with a fresh prune
        // state. Concurrent generators sharing the same index continue
        // to coordinate through `mark_consumed` after this reset.
        if prune_matched {
            index.reset_consumed();
        }

        let mut buffer = vec![0u8; self.buffer_len.max(block_len)];
        let mut buffer_pos = 0usize;
        let mut buffer_len = 0usize;

        debug_log!(
            Deltasum,
            2,
            "hash search b={} len={}",
            block_len,
            index.block_length()
        );

        debug_log!(
            Deltasum,
            3,
            "hash search s->blength={} buffer_len={}",
            block_len,
            self.buffer_len
        );

        loop {
            if buffer_pos == buffer_len {
                buffer_len = reader.read(&mut buffer)?;
                buffer_pos = 0;
                if buffer_len == 0 {
                    break;
                }
            }

            let byte = buffer[buffer_pos];
            buffer_pos += 1;

            let evicted = window.push_back(byte);

            if let Some(outgoing_byte) = evicted {
                // roll() is faster than roll_many() for single-byte updates.
                rolling
                    .roll(outgoing_byte, byte)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                pending_literals.push(outgoing_byte);
                offset += 1;

                // upstream: match.c:339-340 - flush early to bound memory growth
                if pending_literals.len() >= block_len + CHUNK_SIZE {
                    literal_bytes += pending_literals.len() as u64;
                    total_bytes += pending_literals.len() as u64;
                    let filled =
                        std::mem::replace(&mut pending_literals, Vec::with_capacity(block_len));
                    tokens.push(DeltaToken::Literal(filled));
                }
            } else {
                rolling.update_byte(byte);
            }

            if !window.is_full() {
                continue;
            }

            let digest = rolling.digest();

            debug_log!(
                Deltasum,
                4,
                "offset={} sum={:04x}{:04x}",
                offset,
                digest.sum1(),
                digest.sum2()
            );

            // upstream: match.c:144-190 - try want_i hint before hash probe.
            // The want_i hint deliberately bypasses the matched-block bitmap:
            // the hint targets the just-matched index plus one, which the
            // bitmap correctly leaves unset. Adding a bitmap probe to the
            // hint adds a branch with no information gain.
            let (first, second) = window.as_slices();
            let matched = {
                let prune_filter = prune_matched.then_some(&matched_blocks);
                if let Some(hint) = want_i {
                    if index.check_block_match_slices(hint, digest, first, second) {
                        Some(hint)
                    } else {
                        hash_hits += 1;
                        index.find_match_slices_filtered(digest, first, second, prune_filter)
                    }
                } else {
                    hash_hits += 1;
                    index.find_match_slices_filtered(digest, first, second, prune_filter)
                }
            };
            if let Some(mut match_idx) = matched {
                // upstream: match.c:265-310 - block match with bulk refill.
                // After each match, refill the window in bulk and compute the
                // rolling checksum via SIMD-accelerated update() instead of
                // block_len individual push_back()+update_byte() calls. Chained
                // adjacent matches stay in this inner loop.
                //
                // zsync seq-match (`docs/design/zsync-seq-match.md`): when the
                // chain finds blocks N, N+1, N+2 ... in the basis matched at
                // consecutive target offsets, coalesce them into a single fat
                // COPY token of `len = run * block_len`. The wire layer expands
                // this back into one wire token per block, preserving wire
                // byte-equality with the no-coalesce baseline.
                let mut run_start_idx: Option<u64> = None;
                let mut run_len: usize = 0;
                loop {
                    matches += 1;

                    debug_log!(
                        Deltasum,
                        3,
                        "potential match at {} i={} sum={:08x}",
                        offset,
                        match_idx,
                        rolling.digest().value()
                    );

                    if !pending_literals.is_empty() {
                        // Adjacency invariant: literals between runs always
                        // break the seq-match streak, so flush any pending run
                        // before emitting the literal.
                        flush_seq_match_run(
                            &mut tokens,
                            &mut run_start_idx,
                            &mut run_len,
                            block_len,
                        );
                        literal_bytes += pending_literals.len() as u64;
                        total_bytes += pending_literals.len() as u64;
                        let filled =
                            std::mem::replace(&mut pending_literals, Vec::with_capacity(block_len));
                        tokens.push(DeltaToken::Literal(filled));
                    }

                    let block = index.block(match_idx);
                    let block_basis_idx = block.index();

                    match run_start_idx {
                        Some(start_idx) if start_idx + run_len as u64 == block_basis_idx => {
                            run_len += 1;
                        }
                        _ => {
                            flush_seq_match_run(
                                &mut tokens,
                                &mut run_start_idx,
                                &mut run_len,
                                block_len,
                            );
                            run_start_idx = Some(block_basis_idx);
                            run_len = 1;
                        }
                    }
                    total_bytes += block.len() as u64;

                    // zsync prune trigger site, equivalent to write_blocks()
                    // in librcksum/rsum.c:109-119: mark the basis block as
                    // consumed AFTER the Copy token has been emitted, so a
                    // later probe at a different source offset will not pick
                    // this basis index again.
                    matched_blocks.mark_matched(match_idx);
                    // ZSO-3 hash-chain prune on the shared index. Mirrors
                    // the per-session `matched_blocks.mark_matched` above
                    // but uses interior mutability so the same effect
                    // applies when the index is shared read-only across
                    // concurrent generators
                    // (`crates/engine/src/concurrent_delta/`).
                    if prune_matched {
                        index.mark_consumed(match_idx as u32);
                    }

                    let last_matched = match_idx;
                    // upstream/zsync: `librcksum/rsum.c:262` advances the
                    // lookahead slot to the indexed successor. The basis can
                    // re-order or skip partial blocks, so consult the index
                    // rather than assuming `match_idx + 1`.
                    want_i = index.next_match(last_matched);

                    window.clear();
                    rolling.reset();
                    offset += block_len as u64;

                    // upstream: match.c:303-308 - recomputes checksum from scratch
                    // over the next window via get_checksum1() (SIMD-accelerated).
                    let mut filled = 0usize;
                    while filled < block_len {
                        if buffer_pos == buffer_len {
                            buffer_len = reader.read(&mut buffer)?;
                            buffer_pos = 0;
                            if buffer_len == 0 {
                                break;
                            }
                        }
                        let take = (buffer_len - buffer_pos).min(block_len - filled);
                        window.extend_from_slice(&buffer[buffer_pos..buffer_pos + take]);
                        filled += take;
                        buffer_pos += take;
                    }

                    if filled < block_len {
                        // Near EOF: let the byte-by-byte loop drain the remaining bytes.
                        break;
                    }

                    let (s1, s2) = window.as_slices();
                    rolling.update(s1);
                    if !s2.is_empty() {
                        rolling.update(s2);
                    }

                    // upstream: match.c:144-190 - try want_i hint at the next
                    // block boundary before falling back to a hash probe.
                    // The probe uses the index's recorded
                    // [`DeltaSignatureIndex::next_match`] link so a stored
                    // successor that diverges from `last_matched + 1` is
                    // still followed.
                    let adj_digest = rolling.digest();
                    let (f, s) = window.as_slices();
                    let adj_match = {
                        let adj_filter = prune_matched.then_some(&matched_blocks);
                        if let Some(next_idx) =
                            index.try_next_match_slices(last_matched, adj_digest, f, s)
                        {
                            Some(next_idx)
                        } else {
                            hash_hits += 1;
                            index.find_match_slices_filtered(adj_digest, f, s, adj_filter)
                        }
                    };
                    if let Some(next_idx) = adj_match {
                        match_idx = next_idx;
                    } else {
                        false_alarms += 1;
                        break;
                    }
                }
                flush_seq_match_run(&mut tokens, &mut run_start_idx, &mut run_len, block_len);
                continue;
            } else {
                false_alarms += 1;
            }
        }

        // Drain any bytes still in the window as literals (window held fewer
        // than block_len bytes at EOF, so no further match is possible).
        //
        // Upstream rsync matches the basis's short final block here via
        // `l = MIN(blength, len-offset)` (`match.c:222-224`), but on the wire
        // sender path that would (a) fail signature-index construction for
        // small single-partial-block files and (b) produce a token stream the
        // upstream compressed-delta receiver rejects (`token.c:665`). The
        // "trailing partial block emitted as literal" is a pre-existing wire
        // efficiency gap, not a correctness issue - reconstruction is
        // byte-exact either way - so the tail match stays confined to the
        // local-copy delta path (`engine::local_copy`).
        while let Some(byte) = window.pop_front() {
            pending_literals.push(byte);
        }

        if !pending_literals.is_empty() {
            literal_bytes += pending_literals.len() as u64;
            total_bytes += pending_literals.len() as u64;
            tokens.push(DeltaToken::Literal(pending_literals));
        }

        debug_log!(Deltasum, 2, "done hash search");
        debug_log!(
            Deltasum,
            2,
            "false_alarms={} hash_hits={} matches={}",
            false_alarms,
            hash_hits,
            matches
        );

        // upstream: match.c match_report() equivalent.
        debug_log!(
            Deltasum,
            1,
            "delta: {} tokens, {} total, {} literal, {} matched",
            tokens.len(),
            total_bytes,
            literal_bytes,
            total_bytes.saturating_sub(literal_bytes)
        );

        Ok(DeltaScript::new(tokens, total_bytes, literal_bytes))
    }

    /// Generates a [`DeltaScript`] by scanning `source` in parallel across up
    /// to `max_chunks` contiguous, non-overlapping ranges.
    ///
    /// The sender-side rolling scan is inherently sequential per byte, so the
    /// only way to engage multiple cores is to split the source into disjoint
    /// ranges and scan each against the shared signature `index`. Each range
    /// scans with pruning disabled, so no worker ever *writes* the index's
    /// `consumed` bitset. We clear that bitset once up front (the matcher
    /// consults it unconditionally, and a prior pruned [`Self::generate`] would
    /// otherwise leave every block marked consumed, defeating all matching);
    /// after that single reset it is immutable for the duration of the scan, so
    /// the shared `&index` is safe to read from every rayon worker with no
    /// locking. Every worker keeps its own window, rolling checksum, and token
    /// vector; there is no shared mutable state and therefore no mutex on the
    /// hot path.
    ///
    /// # Correctness
    ///
    /// Concatenating the per-range token streams in source order reconstructs
    /// `source` byte-for-byte: each range's tokens independently reconstruct
    /// that range's bytes, and `DeltaToken::Copy { index, .. }` carries the
    /// absolute basis block index, so it is position-independent across the
    /// concatenation boundary. A basis block straddling a range boundary is
    /// simply emitted as literal bytes by the adjoining ranges - a small
    /// compression cost (bounded by `block_length` per boundary), never a
    /// reconstruction error. For inputs too small to split usefully this
    /// delegates to the sequential [`Self::generate`], which keeps pruning on.
    ///
    /// # Wire transparency
    ///
    /// The scan runs with the consumed-bitset prune disabled, so the emitted
    /// token stream (and therefore the wire bytes and the matched/literal
    /// split) equals the pruned sequential [`Self::generate`] output only when
    /// **both** hold:
    ///
    /// 1. the basis is duplicate-free
    ///    ([`DeltaSignatureIndex::has_duplicate_blocks`] is `false`) - with
    ///    every content unique, disabling the prune cannot change which basis
    ///    block a source window resolves to; and
    /// 2. no matched basis block straddles a range boundary - a straddling
    ///    match cannot be completed by either adjoining range and degrades to
    ///    literals here while the sequential scan copies it.
    ///
    /// Condition 2 holds for in-place edits of a same-length basis (matches
    /// stay block-aligned and the boundary either coincides with a block edge
    /// or lands in an edited, non-matching block). It can fail for shifted
    /// content, so callers must treat the result as potentially divergent
    /// (opt-in only, never advertised as byte-identical) and only engage the
    /// parallel path behind a default-off flag with the duplicate-free gate.
    /// The adjacent-literal coalescing in the concatenation loop keeps the
    /// literal-token framing identical to the sequential scan whenever
    /// conditions 1 and 2 hold.
    ///
    /// # Arguments
    ///
    /// * `source` - The full source buffer to delta-encode.
    /// * `index` - Pre-built signature index from the basis file.
    /// * `max_chunks` - Upper bound on parallel ranges (clamped to `>= 1`).
    pub fn generate_chunked(
        &self,
        source: &[u8],
        index: &DeltaSignatureIndex,
        max_chunks: usize,
    ) -> io::Result<DeltaScript> {
        let block_len = index.block_length();
        let n = source.len();

        // Each range must be much larger than one block so the boundary
        // degradation (a straddling block falls back to literals) stays in the
        // noise. Require the larger of 1 MiB and 64 blocks per range.
        let min_chunk = MIN_PARALLEL_CHUNK_BYTES
            .max(block_len.saturating_mul(64))
            .max(1);
        let feasible = (n / min_chunk).max(1);
        let chunks = feasible.min(max_chunks.max(1));

        if chunks <= 1 {
            // Too small to split usefully - keep the pruned sequential path.
            return self.generate(Cursor::new(source), index);
        }

        // The matcher consults the shared `consumed` bitset on every probe,
        // even though these chunks pass no per-chunk prune filter. A prior
        // pruned generate() leaves blocks marked consumed; clear the bitset
        // once so every chunk can match. Pruning is off per chunk, so no worker
        // writes it back - it stays immutable across the concurrent scan.
        index.reset_consumed();

        // Partition into `chunks` contiguous, non-overlapping ranges.
        let base = n / chunks;
        let mut ranges = Vec::with_capacity(chunks);
        let mut start = 0usize;
        for i in 0..chunks {
            let end = if i + 1 == chunks { n } else { start + base };
            ranges.push((start, end));
            start = end;
        }

        // Scan every range concurrently against the shared read-only index.
        // rayon's ordered `collect` preserves source order, so no manual
        // sequencing or locking is needed to reassemble the stream.
        let scripts: Vec<io::Result<DeltaScript>> = ranges
            .par_iter()
            .map(|&(s, e)| self.generate_with_prune(Cursor::new(&source[s..e]), index, false))
            .collect();

        // Concatenate token streams in source order. Literal payloads are
        // moved (not copied) out of each per-range script.
        //
        // Coalesce a trailing `Literal` of one range with the leading
        // `Literal` of the next when a range boundary lands inside an
        // unmatched region: the previous range drains its tail bytes as a
        // literal and the next range emits its head bytes as a second literal,
        // but the sequential [`Self::generate`] scan - which sees the region
        // as one contiguous run - emits a single literal token there. Merging
        // the two halves restores that framing, so the wire byte stream
        // (`write_token_literal` frames each literal token independently)
        // stays identical to the sequential scan for a duplicate-free basis
        // whose matched blocks never straddle a range boundary. Merging only
        // ever concatenates adjacent literal bytes, so reconstruction and the
        // total/literal byte counts are unaffected on every input.
        let mut tokens: Vec<DeltaToken> = Vec::new();
        let mut total_bytes = 0u64;
        let mut literal_bytes = 0u64;
        for script in scripts {
            let script = script?;
            total_bytes += script.total_bytes();
            literal_bytes += script.literal_bytes();
            let mut range_tokens = script.into_tokens().into_iter();
            if let Some(first) = range_tokens.next() {
                match (tokens.last_mut(), first) {
                    (Some(DeltaToken::Literal(prev)), DeltaToken::Literal(next)) => {
                        prev.extend_from_slice(&next);
                    }
                    (_, first) => tokens.push(first),
                }
                tokens.extend(range_tokens);
            }
        }

        Ok(DeltaScript::new(tokens, total_bytes, literal_bytes))
    }
}

impl Default for DeltaGenerator {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience helper that generates a delta using the default [`DeltaGenerator`] configuration.
#[cfg_attr(
    feature = "tracing",
    instrument(skip(reader, index), name = "generate_delta")
)]
pub fn generate_delta<R: Read>(reader: R, index: &DeltaSignatureIndex) -> io::Result<DeltaScript> {
    DeltaGenerator::new().generate(reader, index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::apply_delta;
    use protocol::ProtocolVersion;
    use signature::{
        SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout,
        generate_file_signature,
    };
    use std::io::Cursor;
    use std::num::NonZeroU8;

    /// Deterministic pseudo-random byte stream (xorshift64) for parity tests.
    /// Avoids `rand` and keeps inputs reproducible across runs.
    fn pseudo_random(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state & 0xff) as u8
            })
            .collect()
    }

    /// Reconstructs the source from `basis` by applying `script`.
    fn reconstruct(basis: &[u8], index: &DeltaSignatureIndex, script: &DeltaScript) -> Vec<u8> {
        let mut basis_cursor = Cursor::new(basis.to_vec());
        let mut output = Vec::new();
        apply_delta(&mut basis_cursor, &mut output, index, script).expect("apply");
        output
    }

    fn build_index(data: &[u8]) -> DeltaSignatureIndex {
        let params = SignatureLayoutParams::new(
            data.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature =
            generate_file_signature(data, layout, SignatureAlgorithm::Md4).expect("signature");
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
    }

    /// Builds an index with a caller-chosen fixed block length so a
    /// byte-aligned source yields an exact, predictable block count.
    fn build_index_fixed(data: &[u8], block_len: u32) -> DeltaSignatureIndex {
        use std::num::NonZeroU32;
        let params = SignatureLayoutParams::new(
            data.len() as u64,
            Some(NonZeroU32::new(block_len).unwrap()),
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature =
            generate_file_signature(data, layout, SignatureAlgorithm::Md4).expect("signature");
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
    }

    #[test]
    fn generate_delta_produces_literals_when_no_matches() {
        let basis = vec![0u8; 2048];
        let index = build_index(&basis);
        let input = b"new data";

        let script = generate_delta(&input[..], &index).expect("script");
        assert_eq!(script.tokens().len(), 1);
        assert!(
            matches!(script.tokens()[0], DeltaToken::Literal(ref bytes) if bytes == b"new data")
        );
        assert_eq!(script.literal_bytes(), input.len() as u64);
    }

    #[test]
    fn generate_delta_finds_matching_blocks() {
        let basis: Vec<u8> = (0..10_000).map(|b| (b % 251) as u8).collect();
        let index = build_index(&basis);
        let block_len = index.block_length();
        let mut input = Vec::new();
        input.extend_from_slice(&basis[..block_len]);
        input.extend_from_slice(b"extra");

        let script = generate_delta(&input[..], &index).expect("script");
        assert!(matches!(script.tokens()[0], DeltaToken::Copy { .. }));
        assert!(matches!(script.tokens()[1], DeltaToken::Literal(ref bytes) if bytes == b"extra"));

        let mut basis_cursor = Cursor::new(basis);
        let mut output = Vec::new();
        apply_delta(&mut basis_cursor, &mut output, &index, &script).expect("apply");
        assert_eq!(output, input);
    }

    #[test]
    fn delta_generator_new_uses_default_buffer_len() {
        let generator = DeltaGenerator::new();
        assert_eq!(generator.buffer_len, DEFAULT_BUFFER_LEN);
    }

    #[test]
    fn delta_generator_default_matches_new() {
        let new = DeltaGenerator::new();
        let default = DeltaGenerator::default();
        assert_eq!(new.buffer_len, default.buffer_len);
    }

    #[test]
    fn delta_generator_with_buffer_len_sets_custom_length() {
        let generator = DeltaGenerator::new().with_buffer_len(4096);
        assert_eq!(generator.buffer_len, 4096);
    }

    #[test]
    fn delta_generator_with_buffer_len_zero_becomes_one() {
        let generator = DeltaGenerator::new().with_buffer_len(0);
        assert_eq!(generator.buffer_len, 1);
    }

    #[test]
    fn delta_generator_with_buffer_len_chain() {
        let generator = DeltaGenerator::new()
            .with_buffer_len(1024)
            .with_buffer_len(2048);
        assert_eq!(generator.buffer_len, 2048);
    }

    #[test]
    fn delta_generator_clone() {
        let generator = DeltaGenerator::new().with_buffer_len(512);
        let cloned = generator.clone();
        assert_eq!(generator.buffer_len, cloned.buffer_len);
    }

    #[test]
    fn delta_generator_debug() {
        let generator = DeltaGenerator::new();
        let debug = format!("{generator:?}");
        assert!(debug.contains("DeltaGenerator"));
        assert!(debug.contains("buffer_len"));
    }

    #[test]
    fn generate_delta_empty_input_produces_empty_script() {
        let basis = vec![0u8; 2048];
        let index = build_index(&basis);
        let input: &[u8] = &[];

        let script = generate_delta(input, &index).expect("script");
        assert!(script.tokens().is_empty());
        assert_eq!(script.total_bytes(), 0);
        assert_eq!(script.literal_bytes(), 0);
    }

    #[test]
    fn generate_delta_single_byte_produces_literal() {
        let basis = vec![0u8; 2048];
        let index = build_index(&basis);
        let input = [42u8];

        let script = generate_delta(&input[..], &index).expect("script");
        assert_eq!(script.tokens().len(), 1);
        assert!(matches!(script.tokens()[0], DeltaToken::Literal(ref bytes) if bytes == &[42]));
        assert_eq!(script.literal_bytes(), 1);
    }

    #[test]
    fn generate_delta_all_literal_counts_correctly() {
        let basis = vec![0u8; 2048];
        let index = build_index(&basis);
        let input = b"unique data that won't match any blocks";

        let script = generate_delta(&input[..], &index).expect("script");
        assert_eq!(script.literal_bytes(), input.len() as u64);
        assert_eq!(script.total_bytes(), input.len() as u64);
    }

    #[test]
    fn generate_delta_with_small_buffer_produces_same_result() {
        let basis: Vec<u8> = (0..10_000).map(|b| (b % 251) as u8).collect();
        let index = build_index(&basis);
        let input = b"test input data";

        let default_gen = DeltaGenerator::new();
        let small_gen = DeltaGenerator::new().with_buffer_len(64);

        let script1 = default_gen.generate(&input[..], &index).expect("script1");
        let script2 = small_gen.generate(&input[..], &index).expect("script2");

        assert_eq!(script1.literal_bytes(), script2.literal_bytes());
        assert_eq!(script1.total_bytes(), script2.total_bytes());
    }

    #[test]
    fn generate_delta_with_large_buffer_produces_same_result() {
        let basis: Vec<u8> = (0..10_000).map(|b| (b % 251) as u8).collect();
        let index = build_index(&basis);
        let input = b"test input data";

        let default_gen = DeltaGenerator::new();
        let large_gen = DeltaGenerator::new().with_buffer_len(1024 * 1024);

        let script1 = default_gen.generate(&input[..], &index).expect("script1");
        let script2 = large_gen.generate(&input[..], &index).expect("script2");

        assert_eq!(script1.literal_bytes(), script2.literal_bytes());
        assert_eq!(script1.total_bytes(), script2.total_bytes());
    }

    #[test]
    fn generate_delta_copy_only_has_zero_literal_bytes() {
        let basis: Vec<u8> = (0..10_000).map(|b| (b % 251) as u8).collect();
        let index = build_index(&basis);
        let block_len = index.block_length();
        let input = basis[..block_len].to_vec();

        let script = generate_delta(&input[..], &index).expect("script");
        assert_eq!(script.literal_bytes(), 0);
    }

    #[test]
    fn generate_delta_mixed_literal_and_copy() {
        let basis: Vec<u8> = (0..10_000).map(|b| (b % 251) as u8).collect();
        let index = build_index(&basis);
        let block_len = index.block_length();

        let mut input = vec![1u8, 2u8, 3u8];
        input.extend_from_slice(&basis[..block_len]);
        input.extend_from_slice(b"end");

        let script = generate_delta(&input[..], &index).expect("script");
        assert!(script.tokens().len() >= 2);
        assert_eq!(script.literal_bytes(), 6);
    }

    #[test]
    fn generate_delta_convenience_function_works() {
        let basis = vec![0u8; 2048];
        let index = build_index(&basis);
        let input = b"hello";

        let script = generate_delta(&input[..], &index).expect("script");
        assert!(script.total_bytes() > 0);
    }

    #[test]
    fn delta_script_round_trip_identical_data() {
        let basis: Vec<u8> = (0..10_000).map(|b| (b % 251) as u8).collect();
        let index = build_index(&basis);
        let input = basis.clone();

        let script = generate_delta(&input[..], &index).expect("script");
        assert!(script.literal_bytes() < script.total_bytes());

        let mut basis_cursor = Cursor::new(basis);
        let mut output = Vec::new();
        apply_delta(&mut basis_cursor, &mut output, &index, &script).expect("apply");
        assert_eq!(output, input);
    }

    #[test]
    fn generate_chunked_reconstructs_identical_source() {
        let data = pseudo_random(4 * 1024 * 1024, 0x1234_5678);
        let index = build_index(&data);
        let generator = DeltaGenerator::new();

        let sequential = generator
            .generate(Cursor::new(&data[..]), &index)
            .expect("sequential");
        let chunked = generator
            .generate_chunked(&data, &index, 8)
            .expect("chunked");

        // Both must reconstruct the source byte-for-byte even though the
        // chunked token stream differs at range boundaries.
        assert_eq!(reconstruct(&data, &index, &sequential), data);
        assert_eq!(reconstruct(&data, &index, &chunked), data);
        assert_eq!(chunked.total_bytes(), data.len() as u64);
        assert_eq!(sequential.total_bytes(), data.len() as u64);
    }

    #[test]
    fn generate_chunked_reconstructs_modified_source() {
        let basis = pseudo_random(4 * 1024 * 1024, 0x0000_abcd);
        let index = build_index(&basis);

        let mut source = basis.clone();
        // Scatter single-byte edits through chunk interiors and across a
        // range boundary so each chunk holds a mix of copies and literals.
        for off in [10_usize, 1_000_000, 2_500_000, source.len() - 50] {
            source[off] ^= 0xff;
        }
        // Bracket with pure-literal regions the basis cannot match.
        let mut full = b"PREFIX-LITERAL-REGION".to_vec();
        full.extend_from_slice(&source);
        full.extend_from_slice(b"SUFFIX-LITERAL-REGION");

        let generator = DeltaGenerator::new();
        let sequential = generator
            .generate(Cursor::new(&full[..]), &index)
            .expect("sequential");
        let chunked = generator
            .generate_chunked(&full, &index, 6)
            .expect("chunked");

        assert_eq!(reconstruct(&basis, &index, &sequential), full);
        assert_eq!(reconstruct(&basis, &index, &chunked), full);
        assert_eq!(chunked.total_bytes(), full.len() as u64);
    }

    #[test]
    fn generate_chunked_handles_duplicate_basis_blocks() {
        // 4 MiB of a repeated 64 KiB block: many basis indices share content,
        // exercising the prune-off concurrent path where chunks independently
        // match the same basis blocks without coordination.
        let block = pseudo_random(64 * 1024, 0x0000_0099);
        let mut basis = Vec::with_capacity(4 * 1024 * 1024);
        for _ in 0..64 {
            basis.extend_from_slice(&block);
        }
        let index = build_index(&basis);
        let source = basis.clone();

        let chunked = DeltaGenerator::new()
            .generate_chunked(&source, &index, 8)
            .expect("chunked");

        assert_eq!(reconstruct(&basis, &index, &chunked), source);
        assert_eq!(chunked.total_bytes(), source.len() as u64);
    }

    #[test]
    fn generate_chunked_small_input_matches_sequential() {
        let basis = pseudo_random(8192, 0x0000_0042);
        let index = build_index(&basis);
        let source = basis.clone();

        let generator = DeltaGenerator::new();
        let sequential = generator
            .generate(Cursor::new(&source[..]), &index)
            .expect("sequential");
        let chunked = generator
            .generate_chunked(&source, &index, 8)
            .expect("chunked");

        // Below the split threshold the chunked path delegates to the
        // sequential scan, so the scripts must be byte-identical.
        assert_eq!(sequential.tokens().len(), chunked.tokens().len());
        assert_eq!(sequential.total_bytes(), chunked.total_bytes());
        assert_eq!(sequential.literal_bytes(), chunked.literal_bytes());
        assert_eq!(reconstruct(&basis, &index, &chunked), source);
    }

    #[test]
    fn generate_chunked_max_chunks_one_is_sequential() {
        let data = pseudo_random(4 * 1024 * 1024, 0x0000_5a5a);
        let index = build_index(&data);
        let generator = DeltaGenerator::new();

        let sequential = generator
            .generate(Cursor::new(&data[..]), &index)
            .expect("sequential");
        // max_chunks == 1 must force the sequential path verbatim.
        let single = generator
            .generate_chunked(&data, &index, 1)
            .expect("single");

        assert_eq!(sequential.tokens().len(), single.tokens().len());
        assert_eq!(sequential.total_bytes(), single.total_bytes());
        assert_eq!(sequential.literal_bytes(), single.literal_bytes());
    }

    #[test]
    fn generate_chunked_matches_after_prior_pruned_generate() {
        // Regression: the matcher consults the shared `consumed` bitset on
        // every probe. A pruned generate() leaves blocks marked consumed; if
        // generate_chunked does not reset the bitset, every block looks taken
        // and the scan degrades to all-literal (correct output, but zero delta
        // compression and pathologically slow). Reconstruction parity alone
        // cannot catch this - assert effectiveness (most bytes are copied).
        let data = pseudo_random(4 * 1024 * 1024, 0x0bad_f00d);
        let index = build_index(&data);
        let generator = DeltaGenerator::new();

        // Prime the shared consumed-bitset with a pruned sequential generate.
        let seq = generator
            .generate(Cursor::new(&data[..]), &index)
            .expect("seq");
        assert!(
            seq.literal_bytes() < data.len() as u64 / 4,
            "sequential should mostly match identical data"
        );

        let chunked = generator
            .generate_chunked(&data, &index, 4)
            .expect("chunked");
        assert_eq!(reconstruct(&data, &index, &chunked), data);
        assert!(
            chunked.literal_bytes() < data.len() as u64 / 4,
            "chunked must still produce copies after a prior pruned generate; \
             got literal_bytes={} of {}",
            chunked.literal_bytes(),
            data.len()
        );
    }

    #[test]
    fn block_skip_matched_path_is_o_blocks_not_o_bytes() {
        // ZSO-2 regression: after a confirmed block match the generator
        // advances the scan by a whole block (`offset += block_len` on the
        // matched path, generator.rs ~:391) instead of sliding one byte at a
        // time (`offset += 1` on the miss path, ~:253). A byte-for-byte copy
        // of a duplicate-free, block-aligned basis is therefore resolved with
        // O(file_len / block_len) block probes, never O(file_len) per-byte
        // probes.
        const BLOCK_LEN: u32 = 1024;
        const N_BLOCKS: usize = 64;

        let data = pseudo_random(N_BLOCKS * BLOCK_LEN as usize, 0x51ce_b100);
        let index = build_index_fixed(&data, BLOCK_LEN);
        // A deterministic per-block count requires a duplicate-free basis so
        // every source window resolves to exactly one basis block.
        assert!(
            !index.has_duplicate_blocks(),
            "random basis must be duplicate-free for a deterministic skip count",
        );
        let block_len = index.block_length();
        assert_eq!(
            block_len, BLOCK_LEN as usize,
            "layout must honor the block length"
        );
        let n_blocks = data.len() / block_len;
        assert_eq!(n_blocks, N_BLOCKS);
        assert_eq!(index.block_count(), N_BLOCKS);

        // The index owns the shared seq-match probe counters. Reset them, then
        // scan an exact copy of the basis.
        let counters = index.seq_match_counters();
        counters.reset();

        let generator = DeltaGenerator::new();
        let script = generator
            .generate(Cursor::new(&data[..]), &index)
            .expect("script");

        // A 100%-match, duplicate-free, block-aligned source coalesces into a
        // single fat Copy token spanning every basis block, with zero literals.
        // A matched path that slid by one byte would fall out of block
        // alignment and spill literals instead.
        assert_eq!(
            script.literal_bytes(),
            0,
            "a full copy must emit no literals"
        );
        assert_eq!(script.total_bytes(), data.len() as u64);
        assert_eq!(
            script.tokens().len(),
            1,
            "seq-match must coalesce the full run into a single Copy token",
        );
        match &script.tokens()[0] {
            DeltaToken::Copy { index: idx, len } => {
                assert_eq!(*idx, 0, "the coalesced run must start at basis block 0");
                assert_eq!(*len, data.len(), "the run must cover every basis block");
            }
            other => panic!("expected a single Copy token, got {other:?}"),
        }

        // Block-skip proof: the matched path takes the adjacency fast-path
        // exactly once per block transition - N-1 probes for N blocks, every
        // one a hit. This is the iteration count the spec pins: O(blocks), i.e.
        // `file_len / block_len - 1`, not O(bytes) = `file_len`. A broken skip
        // that slid by 1 on the matched path would never enter the bulk-refill
        // inner loop, recording ZERO seq-match probes (and spilling literals,
        // asserted above).
        let probes = counters.probes();
        let hits = counters.hits();
        assert_eq!(
            probes,
            (n_blocks - 1) as u64,
            "expected {} block probes (file_len/block_len - 1), got {probes}; \
             O(file_len)={} probes would mean a per-byte matched path",
            n_blocks - 1,
            data.len(),
        );
        assert_eq!(
            hits,
            (n_blocks - 1) as u64,
            "every adjacency probe must confirm through the seq-match fast path",
        );

        // The skipped-ahead script must still rebuild the source byte-for-byte.
        assert_eq!(reconstruct(&data, &index, &script), data);
    }

    #[test]
    fn has_duplicate_blocks_false_for_unique_content() {
        // A pseudo-random basis has (with overwhelming probability) no two
        // blocks with identical content, so the duplicate-free gate must open.
        let basis = pseudo_random(64 * 1024, 0x00d1_5715);
        let index = build_index(&basis);
        assert!(
            !index.has_duplicate_blocks(),
            "distinct random blocks must not be flagged as duplicates"
        );
    }

    #[test]
    fn has_duplicate_blocks_true_for_repeated_content() {
        // A constant-fill basis makes every full block content-identical
        // regardless of the layout's chosen block length, so the gate must
        // detect duplicates and force the parallel scan to fall back to the
        // pruned sequential path.
        let basis = vec![0x42u8; 64 * 1024];
        let index = build_index(&basis);
        assert!(
            index.has_duplicate_blocks(),
            "repeated block content must be flagged as duplicate"
        );
    }
}
