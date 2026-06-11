#![no_main]

//! Adversarial-ordering fuzz target for the parallel receive-delta path.
//!
//! Drives [`engine::concurrent_delta::ParallelDeltaApplier`] with adversarial
//! chunk orderings - reverse completion, threshold-trip mid-batch, cross-file
//! interleaving, slot-recycle races - and asserts a SHA-256 oracle: the
//! reconstructed destination bytes must hash equal to the sequential reference
//! produced by applying the same chunks in `(file_ndx, chunk_sequence)` order.
//!
//! # Why this target exists
//!
//! PIP-7 (#2588) shipped a receiver corruption defect that surfaced only
//! when the file-count threshold tripped mid-transfer. The sequential-ordering
//! unit tests for `ParallelDeltaApplier` passed; the corruption required
//! adversarial ordering at the dispatch boundary. The lesson captured in
//! `docs/design/pip-7-lessons-learned.md` and in the
//! `feedback_concurrent_path_discipline` memory note: any concurrent code path
//! needs adversarial-ordering stress fuzz BEFORE flipping default-on.
//!
//! This target encodes the PIP-7 and DG-3 regression shapes as a fuzz corpus
//! and asserts the SHA-256 oracle on every fuzz input. The seeded corpus
//! entries (`pip_7_threshold_trip.bin`, `dg_3_slot_recycle_race.bin`) carry
//! the known regression shapes forward as a permanent floor.
//!
//! # Input format
//!
//! The fuzzed bytes are parsed as a sequence of chunk records:
//!
//! ```text
//! header  : u8  file_count  (1..=8)
//! header  : u8  payload_size_log2 (4..=10, payload bytes = 1 << this)
//! records : [
//!   u8  file_ndx (mod file_count)
//!   u16 chunk_sequence
//!   payload (payload_size bytes)
//! ] *
//! ```
//!
//! Records are submitted to the applier in arrival order (the adversarial
//! permutation). The oracle re-sorts the same records by
//! `(file_ndx, chunk_sequence)` and computes the SHA-256 of the
//! sequentially-applied byte stream per file.
//!
//! # What the assertion catches
//!
//! - Per-file reorder buffer mis-sequencing (chunks written out of order).
//! - Cross-file slot-map collisions (chunk for file A written into file B).
//! - First-dispatch construction-time races (the PIP-7 shape: first file's
//!   bytes corrupted while pipeline brings up).
//! - Slot recycle while a `DecrementGuard` is still alive (the DG-3 shape).
//!
//! Any divergence between the parallel-applied SHA-256 and the sequential
//! reference SHA-256 fires `assert_eq!`, which libFuzzer treats as a finding.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run parallel_receive_delta_adversarial
//! cargo +nightly fuzz run parallel_receive_delta_adversarial -- -max_total_time=120
//! ```

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use engine::concurrent_delta::{DeltaChunk, ParallelDeltaApplier};
use libfuzzer_sys::fuzz_target;
use sha2::{Digest, Sha256};

/// Maximum number of files a single fuzz iteration registers. Keeps the
/// state surface small enough that 1000+ iterations/sec is achievable.
const MAX_FILES: u8 = 8;

/// Minimum log2 payload size per chunk. `1 << 4 = 16` bytes.
const MIN_PAYLOAD_LOG2: u8 = 4;

/// Maximum log2 payload size per chunk. `1 << 10 = 1024` bytes - enough to
/// exercise the per-file reorder buffer drain path without ballooning state.
const MAX_PAYLOAD_LOG2: u8 = 10;

/// Cap on records per iteration. With `MAX_PAYLOAD_LOG2=10` this puts the
/// largest single iteration at 64 KiB of payload, on par with the other
/// outcome-comparison fuzz targets in this crate.
const MAX_RECORDS: usize = 64;

/// A single chunk record parsed from the fuzz input.
#[derive(Clone, Debug)]
struct Record {
    file_ndx: u8,
    chunk_sequence: u16,
    payload: Vec<u8>,
}

/// Parse the fuzz input into a record list. Returns `None` if the header is
/// invalid or no records can be extracted.
fn parse_input(data: &[u8]) -> Option<(u8, Vec<Record>)> {
    if data.len() < 2 {
        return None;
    }
    let file_count = (data[0] % MAX_FILES) + 1;
    let payload_log2 = MIN_PAYLOAD_LOG2
        + (data[1] % (MAX_PAYLOAD_LOG2 - MIN_PAYLOAD_LOG2 + 1));
    let payload_size = 1usize << payload_log2;
    let record_size = 1 + 2 + payload_size;

    let mut records = Vec::new();
    let mut cursor = 2usize;
    while cursor + record_size <= data.len() && records.len() < MAX_RECORDS {
        let file_ndx = data[cursor] % file_count;
        let chunk_sequence = u16::from_le_bytes([data[cursor + 1], data[cursor + 2]]);
        let payload = data[cursor + 3..cursor + 3 + payload_size].to_vec();
        records.push(Record {
            file_ndx,
            chunk_sequence,
            payload,
        });
        cursor += record_size;
    }

    if records.is_empty() {
        return None;
    }
    Some((file_count, records))
}

/// Deduplicate records by `(file_ndx, chunk_sequence)`. The reorder buffer
/// requires every chunk sequence to be unique per file; duplicates would
/// trigger `CapacityExceeded` on the second insert. Keep the first occurrence.
fn deduplicate(records: Vec<Record>) -> Vec<Record> {
    let mut seen = std::collections::HashSet::new();
    records
        .into_iter()
        .filter(|r| seen.insert((r.file_ndx, r.chunk_sequence)))
        .collect()
}

/// Renumber chunk sequences per file into the dense `0..N` range while
/// preserving the relative order from the fuzz input. The reorder buffer
/// expects `next_expected` to start at 0 and advance by one per drain.
fn renumber(records: Vec<Record>) -> Vec<Record> {
    let mut per_file_order: BTreeMap<u8, Vec<u16>> = BTreeMap::new();
    for r in &records {
        per_file_order
            .entry(r.file_ndx)
            .or_default()
            .push(r.chunk_sequence);
    }
    let mut mapping: BTreeMap<(u8, u16), u16> = BTreeMap::new();
    for (file, mut seqs) in per_file_order {
        seqs.sort_unstable();
        for (idx, seq) in seqs.iter().enumerate() {
            mapping.insert((file, *seq), idx as u16);
        }
    }
    records
        .into_iter()
        .map(|r| Record {
            file_ndx: r.file_ndx,
            chunk_sequence: mapping[&(r.file_ndx, r.chunk_sequence)],
            payload: r.payload,
        })
        .collect()
}

/// Compute the sequential-reference SHA-256 per file by sorting records by
/// `chunk_sequence` and hashing the concatenated payloads.
fn sequential_reference(records: &[Record]) -> BTreeMap<u8, [u8; 32]> {
    let mut per_file: BTreeMap<u8, Vec<&Record>> = BTreeMap::new();
    for r in records {
        per_file.entry(r.file_ndx).or_default().push(r);
    }
    per_file
        .into_iter()
        .map(|(file, mut rs)| {
            rs.sort_by_key(|r| r.chunk_sequence);
            let mut hasher = Sha256::new();
            for r in rs {
                hasher.update(&r.payload);
            }
            (file, hasher.finalize().into())
        })
        .collect()
}

/// A `Write` sink that mirrors writes into a shared `Vec<u8>` so the test
/// can SHA-256 the destination after the applier finishes.
#[derive(Default)]
struct VecSink {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl VecSink {
    fn new() -> (Self, Arc<Mutex<Vec<u8>>>) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                buf: Arc::clone(&buf),
            },
            buf,
        )
    }
}

impl Write for VecSink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Drive the applier with the adversarial record stream and SHA-256 each
/// file's destination buffer.
fn parallel_destination_hashes(
    file_count: u8,
    records: &[Record],
) -> Option<BTreeMap<u8, [u8; 32]>> {
    let applier = ParallelDeltaApplier::new(4);
    let mut buffers: BTreeMap<u8, Arc<Mutex<Vec<u8>>>> = BTreeMap::new();
    for file in 0..file_count {
        let (sink, buf) = VecSink::new();
        applier
            .register_file(u64::from(file), Box::new(sink) as Box<dyn Write + Send>)
            .ok()?;
        buffers.insert(file, buf);
    }
    for r in records {
        let chunk = DeltaChunk::literal(
            u64::from(r.file_ndx),
            u64::from(r.chunk_sequence),
            r.payload.clone(),
        );
        applier.apply_one_chunk(chunk).ok()?;
    }
    applier.drain_inflight().ok()?;

    Some(
        buffers
            .into_iter()
            .map(|(file, buf)| {
                let bytes = buf.lock().unwrap().clone();
                let mut hasher = Sha256::new();
                hasher.update(&bytes);
                (file, hasher.finalize().into())
            })
            .collect(),
    )
}

fuzz_target!(|data: &[u8]| {
    let Some((file_count, records)) = parse_input(data) else {
        return;
    };
    let records = renumber(deduplicate(records));
    if records.is_empty() {
        return;
    }
    let expected = sequential_reference(&records);
    let Some(actual) = parallel_destination_hashes(file_count, &records) else {
        return;
    };
    assert_eq!(
        actual, expected,
        "parallel receive-delta SHA-256 divergence \
         (file_count={file_count}, records={})",
        records.len(),
    );
});
