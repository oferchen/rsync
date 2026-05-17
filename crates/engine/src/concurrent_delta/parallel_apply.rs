//! Parallel receive-side delta apply scaffold (#1368).
//!
//! Gated behind the `parallel-receive-delta` feature so the production binary
//! continues to drive the sequential apply loop in
//! `crates/transfer/src/receiver/transfer.rs`. The design at
//! `docs/design/parallel-receive-delta-application.md` calls for this code
//! path to be opt-in until the parity-test gap (#4205 G2) closes and the
//! drain-parallel bench from #4214 shows a measurable win at receive-side
//! scale.
//!
//! # Shape
//!
//! [`ParallelDeltaApplier`] owns a configurable concurrency limit and a
//! per-file map of [`Mutex`]-guarded destination writers. Callers hand it
//! [`DeltaChunk`] values (one literal-or-block segment for one file) through
//! [`apply_chunk_parallel`](ParallelDeltaApplier::apply_chunk_parallel). The
//! checksum verify step runs on the rayon pool; the actual file-write happens
//! under the per-file mutex so per-file byte order is preserved.
//!
//! # Ordering preservation
//!
//! Two layers protect the wire-format invariants documented in section 2 of
//! the design doc:
//!
//! 1. **Per-file token order.** Each chunk carries a monotonic
//!    `chunk_sequence` per file. A per-file [`ReorderBuffer`] inside the
//!    applier replays chunks in submission order before they touch the
//!    destination writer, even though the rayon verify step completes out of
//!    order.
//! 2. **Per-file write exclusivity.** The destination writer for each file
//!    sits behind a [`Mutex`], so only one chunk ever holds the writer at a
//!    time. Combined with the reorder buffer above, the bytes hit the file
//!    in the exact sequence the producer submitted them.
//!
//! Cross-file ordering at the wire-output layer is the
//! [`super::ReorderBuffer`] caller's responsibility (the existing
//! `DeltaConsumer` pattern already covers that case).

use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;

use super::reorder::ReorderBuffer;
use super::types::FileNdx;

/// A single contiguous segment of a per-file delta apply.
///
/// One chunk corresponds to either a literal-data span (`is_literal = true`)
/// or a basis-file block reference (`is_literal = false`). Either way it
/// carries the bytes already resolved by the wire reader plus the
/// per-file sequence number assigned at submission time.
///
/// Chunks are CPU-light at this stage; the heavy step is the strong-checksum
/// rollup that [`ParallelDeltaApplier::verify_chunk`] runs on a rayon
/// worker.
#[derive(Debug, Clone)]
pub struct DeltaChunk {
    /// File this chunk belongs to.
    pub ndx: FileNdx,
    /// Monotonic per-file submission sequence number.
    ///
    /// The applier replays chunks for `ndx` in increasing `chunk_sequence`
    /// order, mirroring the per-file byte order the sender emitted.
    pub chunk_sequence: u64,
    /// Resolved bytes for this chunk.
    pub data: Vec<u8>,
    /// `true` for literal payloads, `false` for basis-file matches. The
    /// verify and write paths are identical today; the discriminator is kept
    /// so future stats reporting can split literal vs matched bytes without
    /// touching the public chunk shape.
    pub is_literal: bool,
}

impl DeltaChunk {
    /// Builds a literal-data chunk.
    #[must_use]
    pub fn literal(ndx: impl Into<FileNdx>, chunk_sequence: u64, data: Vec<u8>) -> Self {
        Self {
            ndx: ndx.into(),
            chunk_sequence,
            data,
            is_literal: true,
        }
    }

    /// Builds a basis-match chunk.
    #[must_use]
    pub fn matched(ndx: impl Into<FileNdx>, chunk_sequence: u64, data: Vec<u8>) -> Self {
        Self {
            ndx: ndx.into(),
            chunk_sequence,
            data,
            is_literal: false,
        }
    }
}

/// Per-file destination writer plus the reorder buffer that re-establishes
/// submission order after the rayon verify step completes out of order.
struct FileSlot {
    writer: Box<dyn Write + Send>,
    reorder: ReorderBuffer<DeltaChunk>,
    bytes_written: u64,
}

impl FileSlot {
    fn new(writer: Box<dyn Write + Send>, reorder_capacity: usize) -> Self {
        Self {
            writer,
            reorder: ReorderBuffer::new(reorder_capacity),
            bytes_written: 0,
        }
    }

    /// Inserts `chunk` into the reorder buffer and drains any contiguous run
    /// that is now ready, writing each ready chunk to the destination.
    ///
    /// The reorder buffer is the single source of truth for per-file
    /// sequencing; the writer only sees chunks once they have arrived in
    /// strict `chunk_sequence` order.
    fn ingest(&mut self, chunk: DeltaChunk) -> io::Result<()> {
        let seq = chunk.chunk_sequence;
        self.reorder
            .insert(seq, chunk)
            .map_err(|e| io::Error::other(format!("parallel apply reorder full: {e}")))?;
        for ready in self.reorder.drain_ready() {
            self.write_chunk(ready)?;
        }
        Ok(())
    }

    fn write_chunk(&mut self, chunk: DeltaChunk) -> io::Result<()> {
        self.writer.write_all(&chunk.data)?;
        self.bytes_written = self
            .bytes_written
            .checked_add(chunk.data.len() as u64)
            .ok_or_else(|| io::Error::other("parallel apply byte counter overflow"))?;
        Ok(())
    }

    /// Returns the bytes-written counter.
    fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Returns true if every submitted chunk for this file has hit the writer.
    fn drained(&self) -> bool {
        self.reorder.is_empty()
    }
}

/// CPU-bound verification result handed back from the rayon worker so the
/// owning thread can run the serial per-file write under the per-file mutex.
#[derive(Debug)]
struct VerifiedChunk {
    chunk: DeltaChunk,
    /// Strong-checksum rollup of `chunk.data`. Kept opaque (length-only) so
    /// the scaffold does not commit to a particular strong-checksum
    /// algorithm; the receiver supplies its own [`ChecksumVerifier`] once
    /// the wiring lands in phase 2 of the rollout plan.
    digest_len: usize,
}

/// Parallel receive-side delta applier.
///
/// Fans the CPU-bound verify step across rayon workers while keeping the
/// per-file destination writer serial. The struct is `Send + Sync` so a
/// single instance can back the whole receiver pipeline.
///
/// # Concurrency limit
///
/// The applier respects [`Self::concurrency`] when sharding chunk batches
/// through [`rayon::ThreadPoolBuilder`]'s ambient pool. Callers can size
/// this from [`rayon::current_num_threads`] or from a CLI override.
#[derive(Debug)]
pub struct ParallelDeltaApplier {
    /// Per-file slots keyed by NDX. The outer [`Mutex`] guards map mutation
    /// (file insert/remove); per-file slots have their own inner [`Mutex`]
    /// to keep the write side serial without serialising every file.
    files: Mutex<HashMap<FileNdx, Arc<Mutex<FileSlot>>>>,
    /// Reorder-buffer capacity per file. Bounded so a stalled file does not
    /// pin unbounded memory.
    per_file_reorder_capacity: usize,
    /// Maximum number of chunks the applier dispatches to rayon in parallel.
    concurrency: usize,
}

impl ParallelDeltaApplier {
    /// Default per-file reorder buffer capacity. Sized to hold a handful of
    /// rayon workers' worth of in-flight chunks per file without forcing
    /// the producer to block.
    pub const DEFAULT_PER_FILE_REORDER_CAPACITY: usize = 64;

    /// Builds a new applier with the supplied concurrency limit.
    ///
    /// `concurrency == 0` is treated as "use the ambient rayon pool".
    #[must_use]
    pub fn new(concurrency: usize) -> Self {
        Self {
            files: Mutex::new(HashMap::new()),
            per_file_reorder_capacity: Self::DEFAULT_PER_FILE_REORDER_CAPACITY,
            concurrency,
        }
    }

    /// Builder-style override for the per-file reorder buffer capacity.
    #[must_use]
    pub fn with_per_file_reorder_capacity(mut self, capacity: usize) -> Self {
        assert!(capacity > 0, "per-file reorder capacity must be non-zero");
        self.per_file_reorder_capacity = capacity;
        self
    }

    /// Returns the configured concurrency limit.
    #[must_use]
    pub fn concurrency(&self) -> usize {
        self.concurrency
    }

    /// Registers a destination writer for `ndx`.
    ///
    /// Must be called before the first chunk for `ndx` reaches
    /// [`apply_chunk_parallel`](Self::apply_chunk_parallel). Returns an
    /// error if `ndx` already has a writer (the receiver opens each file
    /// exactly once).
    pub fn register_file(
        &self,
        ndx: impl Into<FileNdx>,
        writer: Box<dyn Write + Send>,
    ) -> io::Result<()> {
        let ndx = ndx.into();
        let mut files = self
            .files
            .lock()
            .map_err(|_| io::Error::other("parallel applier file map poisoned"))?;
        if files.contains_key(&ndx) {
            return Err(io::Error::other(format!(
                "parallel applier file {ndx} already registered"
            )));
        }
        files.insert(
            ndx,
            Arc::new(Mutex::new(FileSlot::new(
                writer,
                self.per_file_reorder_capacity,
            ))),
        );
        Ok(())
    }

    /// Applies one chunk, dispatching the CPU-bound verify step to rayon.
    ///
    /// The verify step runs on a rayon worker via [`rayon::join`] so the
    /// ambient pool (or the worker that owns the current thread) handles
    /// the work without spinning up a new pool. The serial write step then
    /// runs under the per-file mutex so per-file byte order is preserved.
    pub fn apply_chunk_parallel(&self, chunk: DeltaChunk) -> io::Result<()> {
        let slot = self.slot_for(chunk.ndx)?;
        // `rayon::join` schedules the verify on a worker thread when the
        // caller is inside the rayon pool; outside the pool it falls back
        // to the calling thread, which keeps single-threaded callers cheap.
        let (verified, _) = rayon::join(|| Self::verify_chunk(chunk), || ());

        let mut slot = slot
            .lock()
            .map_err(|_| io::Error::other("parallel applier file slot poisoned"))?;
        let _ = verified.digest_len; // reserved for future stats wiring
        slot.ingest(verified.chunk)
    }

    /// Applies a batch of chunks, fanning the verify step across the rayon
    /// pool subject to [`Self::concurrency`]. Order-preserving per file.
    ///
    /// Chunks belonging to different files run independently; chunks for the
    /// same file are merged back through the per-file reorder buffer before
    /// they reach the destination writer.
    pub fn apply_batch_parallel(&self, chunks: Vec<DeltaChunk>) -> io::Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        let total = chunks.len();
        let cap = if self.concurrency == 0 {
            total
        } else {
            self.concurrency.min(total)
        };
        let min_len = total.div_ceil(cap.max(1)).max(1);
        let verified: Vec<VerifiedChunk> = chunks
            .into_par_iter()
            .with_min_len(min_len)
            .map(Self::verify_chunk)
            .collect();

        for v in verified {
            let slot = self.slot_for(v.chunk.ndx)?;
            let mut slot = slot
                .lock()
                .map_err(|_| io::Error::other("parallel applier file slot poisoned"))?;
            slot.ingest(v.chunk)?;
        }
        Ok(())
    }

    /// Returns the total bytes written to `ndx` so far.
    pub fn bytes_written(&self, ndx: impl Into<FileNdx>) -> io::Result<u64> {
        let ndx = ndx.into();
        let slot = self.slot_for(ndx)?;
        let slot = slot
            .lock()
            .map_err(|_| io::Error::other("parallel applier file slot poisoned"))?;
        Ok(slot.bytes_written())
    }

    /// Finalises a file's writer once every submitted chunk has applied.
    ///
    /// Returns the destination writer so the caller can run its own
    /// finalisation step (checksum verify, temp-file rename, metadata
    /// apply). Errors if any chunks remain buffered awaiting a missing
    /// `chunk_sequence`.
    pub fn finish_file(&self, ndx: impl Into<FileNdx>) -> io::Result<Box<dyn Write + Send>> {
        let ndx = ndx.into();
        let slot_arc = {
            let mut files = self
                .files
                .lock()
                .map_err(|_| io::Error::other("parallel applier file map poisoned"))?;
            files
                .remove(&ndx)
                .ok_or_else(|| io::Error::other(format!("parallel applier file {ndx} unknown")))?
        };
        let slot = Arc::try_unwrap(slot_arc)
            .map_err(|_| io::Error::other("parallel applier file slot still in flight"))?
            .into_inner()
            .map_err(|_| io::Error::other("parallel applier file slot poisoned"))?;
        if !slot.drained() {
            return Err(io::Error::other(format!(
                "parallel applier file {ndx} finished with chunks still buffered"
            )));
        }
        Ok(slot.writer)
    }

    fn slot_for(&self, ndx: FileNdx) -> io::Result<Arc<Mutex<FileSlot>>> {
        let files = self
            .files
            .lock()
            .map_err(|_| io::Error::other("parallel applier file map poisoned"))?;
        files
            .get(&ndx)
            .map(Arc::clone)
            .ok_or_else(|| io::Error::other(format!("parallel applier file {ndx} unknown")))
    }

    /// Pure CPU step that the rayon worker runs. Currently a strong-rollup
    /// of `chunk.data.len()` so the scaffold has a measurable verify cost
    /// to amortise across cores; replaced with the real strong checksum
    /// when the phase 2 wiring lands (see design doc 6.3).
    fn verify_chunk(chunk: DeltaChunk) -> VerifiedChunk {
        // The actual strong checksum is supplied by the receiver pipeline;
        // here we only need to model the parallelisable cost so the
        // scaffold's per-file ordering can be validated end-to-end.
        let digest_len = chunk.data.len();
        VerifiedChunk { chunk, digest_len }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::io::Cursor;

    /// In-memory sink that records every byte written so tests can compare
    /// parallel vs sequential output.
    #[derive(Default)]
    struct VecSink {
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl VecSink {
        fn new() -> (Self, Arc<Mutex<Vec<u8>>>) {
            let buf = Arc::new(Mutex::new(Vec::new()));
            (Self { buf: buf.clone() }, buf)
        }
    }

    impl Write for VecSink {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            let mut guard = self.buf.lock().expect("sink mutex poisoned");
            guard.extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn sequential_apply(chunks: &[DeltaChunk]) -> Vec<u8> {
        let mut by_file: HashMap<FileNdx, Vec<&DeltaChunk>> = HashMap::new();
        for c in chunks {
            by_file.entry(c.ndx).or_default().push(c);
        }
        let mut ndxs: Vec<FileNdx> = by_file.keys().copied().collect();
        ndxs.sort();
        let mut out = Vec::new();
        for ndx in ndxs {
            let mut per_file = by_file.remove(&ndx).expect("present");
            per_file.sort_by_key(|c| c.chunk_sequence);
            for c in per_file {
                out.extend_from_slice(&c.data);
            }
        }
        out
    }

    fn collect_file(
        applier: &ParallelDeltaApplier,
        ndx: FileNdx,
        buf: Arc<Mutex<Vec<u8>>>,
    ) -> Vec<u8> {
        let _writer = applier.finish_file(ndx).expect("finish_file");
        buf.lock().expect("sink mutex").clone()
    }

    #[test]
    fn single_file_in_order_matches_sequential() {
        let applier = ParallelDeltaApplier::new(2);
        let (sink, buf) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();

        let chunks: Vec<DeltaChunk> = (0..16)
            .map(|i| DeltaChunk::literal(0u32, i, vec![i as u8; 8]))
            .collect();
        let expected = sequential_apply(&chunks);

        for c in chunks {
            applier.apply_chunk_parallel(c).unwrap();
        }
        assert_eq!(collect_file(&applier, FileNdx::new(0), buf), expected);
    }

    #[test]
    fn single_file_out_of_order_preserves_byte_order() {
        let applier = ParallelDeltaApplier::new(4);
        let (sink, buf) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();

        let chunks: Vec<DeltaChunk> = (0..32)
            .map(|i| DeltaChunk::literal(0u32, i, vec![i as u8; 4]))
            .collect();
        let expected = sequential_apply(&chunks);

        let mut shuffled = chunks.clone();
        // Deterministic non-trivial permutation: rotate by 7.
        shuffled.rotate_left(7);

        for c in shuffled {
            applier.apply_chunk_parallel(c).unwrap();
        }
        assert_eq!(collect_file(&applier, FileNdx::new(0), buf), expected);
    }

    #[test]
    fn batch_apply_matches_sequential_byte_for_byte() {
        let applier = ParallelDeltaApplier::new(8);
        let (sink_a, buf_a) = VecSink::new();
        let (sink_b, buf_b) = VecSink::new();
        applier.register_file(0u32, Box::new(sink_a)).unwrap();
        applier.register_file(1u32, Box::new(sink_b)).unwrap();

        let mut chunks = Vec::new();
        for i in 0..24u64 {
            let payload: Vec<u8> = (0..16).map(|b| ((i as u8).wrapping_add(b))).collect();
            chunks.push(DeltaChunk::literal(0u32, i, payload.clone()));
            chunks.push(DeltaChunk::matched(1u32, i, payload));
        }
        let expected_a = sequential_apply(
            &chunks
                .iter()
                .filter(|c| c.ndx == FileNdx::new(0))
                .cloned()
                .collect::<Vec<_>>(),
        );
        let expected_b = sequential_apply(
            &chunks
                .iter()
                .filter(|c| c.ndx == FileNdx::new(1))
                .cloned()
                .collect::<Vec<_>>(),
        );

        applier.apply_batch_parallel(chunks).unwrap();
        assert_eq!(collect_file(&applier, FileNdx::new(0), buf_a), expected_a);
        assert_eq!(collect_file(&applier, FileNdx::new(1), buf_b), expected_b);
    }

    #[test]
    fn missing_file_registration_errors() {
        let applier = ParallelDeltaApplier::new(1);
        let err = applier
            .apply_chunk_parallel(DeltaChunk::literal(7u32, 0, vec![1, 2, 3]))
            .unwrap_err();
        assert!(err.to_string().contains("unknown"));
    }

    #[test]
    fn double_registration_errors() {
        let applier = ParallelDeltaApplier::new(1);
        let (sink_a, _) = VecSink::new();
        let (sink_b, _) = VecSink::new();
        applier.register_file(3u32, Box::new(sink_a)).unwrap();
        let err = applier.register_file(3u32, Box::new(sink_b)).unwrap_err();
        assert!(err.to_string().contains("already registered"));
    }

    #[test]
    fn finish_file_with_pending_chunks_errors() {
        let applier = ParallelDeltaApplier::new(1);
        let (sink, _) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();

        // Submit out-of-order chunk; sequence 0 never arrives.
        applier
            .apply_chunk_parallel(DeltaChunk::literal(0u32, 1, vec![0u8; 4]))
            .unwrap();
        let err = applier.finish_file(0u32).unwrap_err();
        assert!(err.to_string().contains("still buffered"));
    }

    #[test]
    fn bytes_written_tracks_in_order_writes() {
        let applier = ParallelDeltaApplier::new(2);
        let (sink, _) = VecSink::new();
        applier.register_file(0u32, Box::new(sink)).unwrap();
        applier
            .apply_chunk_parallel(DeltaChunk::literal(0u32, 0, vec![1u8; 100]))
            .unwrap();
        assert_eq!(applier.bytes_written(0u32).unwrap(), 100);
        applier
            .apply_chunk_parallel(DeltaChunk::literal(0u32, 1, vec![2u8; 50]))
            .unwrap();
        assert_eq!(applier.bytes_written(0u32).unwrap(), 150);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        #[test]
        fn random_chunk_sizes_and_permutations_match_sequential(
            sizes in prop::collection::vec(1usize..=64usize, 1..=48),
            seed in 0u64..512,
        ) {
            let chunks: Vec<DeltaChunk> = sizes
                .iter()
                .enumerate()
                .map(|(i, &len)| {
                    let payload: Vec<u8> = (0..len)
                        .map(|b| ((i as u64 ^ seed ^ b as u64) & 0xff) as u8)
                        .collect();
                    DeltaChunk::literal(0u32, i as u64, payload)
                })
                .collect();
            let expected = sequential_apply(&chunks);

            // Permute deterministically by `seed` to simulate parallel-completion order.
            let mut order: Vec<usize> = (0..chunks.len()).collect();
            // Fisher-Yates with a small xorshift seeded by `seed`.
            let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
            for i in (1..order.len()).rev() {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let j = (state as usize) % (i + 1);
                order.swap(i, j);
            }
            let permuted: Vec<DeltaChunk> = order.into_iter().map(|i| chunks[i].clone()).collect();

            let applier = ParallelDeltaApplier::new(((seed % 8) + 1) as usize);
            let (sink, buf) = VecSink::new();
            applier.register_file(0u32, Box::new(sink)).unwrap();
            for c in permuted {
                applier.apply_chunk_parallel(c).unwrap();
            }
            let actual = collect_file(&applier, FileNdx::new(0), buf);
            prop_assert_eq!(actual, expected);
        }
    }

    #[test]
    fn cursor_writer_round_trip() {
        // Smoke test that the trait-object writer wraps anything `Write + Send`.
        let applier = ParallelDeltaApplier::new(1);
        let cursor: Cursor<Vec<u8>> = Cursor::new(Vec::new());
        applier.register_file(0u32, Box::new(cursor)).unwrap();
        applier
            .apply_chunk_parallel(DeltaChunk::literal(0u32, 0, vec![9u8; 32]))
            .unwrap();
        let _writer = applier.finish_file(0u32).unwrap();
    }
}
