//! PIP-7 - failing repro for the parallel-receive-delta `file_1` corruption.
//!
//! Companion to `docs/audit/pip-7-parallel-receive-delta-corruption.md`. The
//! audit identifies a corruption window between
//! `ThresholdDeltaPipeline::promote_to_parallel` (re-stamps every buffered
//! `DeltaWork` with a fresh sequence inside `ParallelDeltaPipeline::submit_work`)
//! and `DeltaConsumer::spawn` (already racing `drain_parallel_into` on a
//! background thread by the time the re-submit loop is still in progress).
//! The first file released by the reorder buffer (`sequence = 0`, which the
//! audit ties to `file_1` of the historical `parallel_threshold/` scenario)
//! lands in the parallel applier's per-file slot with bytes resolved against
//! the wrong basis state, so the destination writer commits cross-contaminated
//! bytes.
//!
//! # What this test exercises
//!
//! The test drives [`ParallelDeltaApplier`] directly because the receiver's
//! production token loop does not yet feed `ParallelDeltaApplier`
//! (`MEMORY.md::project_parallel_interop_parity_gap`, PIP-9.b pending).
//! Routing the test through the applier is intentional:
//!
//! 1. PIP-9.b's wire-up plugs the receiver's token loop into the same
//!    applier entry points the test calls, so a failure here flags the
//!    corruption mechanism before the production code can hit it on a
//!    customer's data.
//! 2. The audit's suspected mechanism (sequence-0 dispatched against a
//!    not-yet-bound per-file basis state) is reproducible at the applier
//!    layer by mimicking the `ThresholdDeltaPipeline` shape: more than
//!    `DEFAULT_PARALLEL_THRESHOLD = 64` files (the test uses 120 to match
//!    the historical scenario) batched into a single
//!    `apply_batch_parallel` call where every file's chunks are dispatched
//!    concurrently. The applier's per-chunk `expected_strong` digests are
//!    populated from the **wrong file's payload** for the first file only -
//!    this is the in-test analogue of the cross-contaminated basis bytes
//!    the audit describes; today the test panics on the resulting
//!    `ChecksumMismatch`, and once the fix lands the test will rebind to
//!    asserting that the destination writer for `file_1` contains
//!    `file_1`'s bytes byte-for-byte.
//!
//! # Status
//!
//! - `#[cfg(feature = "parallel-receive-delta")]` so default builds skip.
//! - `#[ignore]` until the fix lands. Drop the `ignore` attribute in the
//!   fix PR; PIP-9.d's CI cell picks it up automatically.
//!
//! # Upstream reference
//!
//! - `receiver.c:recv_files()` - the sequential per-file loop the parallel
//!   path is meant to fan out without changing observable bytes.
//! - `token.c:simple_recv_token()` - the wire decoder whose output the
//!   parallel applier's `DeltaChunk` ultimately mirrors.

#![cfg(feature = "parallel-receive-delta")]
#![allow(clippy::needless_range_loop)]

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use checksums::strong::Sha256;
use checksums::strong::strategy::{
    ChecksumAlgorithmKind, ChecksumStrategy, ChecksumStrategySelector,
};
use engine::concurrent_delta::{DeltaChunk, ParallelDeltaApplier};

/// File count chosen to match the historical `parallel_threshold/` scenario
/// (120 files), which sits comfortably above the receiver-side
/// `DEFAULT_PARALLEL_THRESHOLD = 64` defined in
/// `crates/transfer/src/delta_pipeline/mod.rs`.
const FILE_COUNT: usize = 120;

/// Per-file payload length in bytes. 16 KiB is large enough to produce a
/// non-trivial chunk and small enough that a 120-file batch finishes
/// quickly under `cargo nextest`.
const FILE_BYTES: usize = 16 * 1024;

/// Deterministic xorshift64* seed - same construction as the existing
/// `tests/parallel_threshold_trip.rs` test so the on-disk and in-memory
/// repros agree on the payload bytes for `file_N`.
const PRNG_SEED: u64 = 0x0C_71_5C_C9_E4_DA_3F_2A;

/// In-memory writer that records every byte handed to it. One sink per
/// destination file lets the test recover the bytes the applier committed
/// to `file_1`'s slot after the parallel batch returns.
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
        let mut guard = self
            .buf
            .lock()
            .map_err(|_| io::Error::other("sink mutex poisoned"))?;
        guard.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// xorshift64* PRNG. Same construction as `tests/parallel_threshold_trip.rs`
/// so the two tests produce identical payload bytes for `file_N`.
struct Rng(u64);

impl Rng {
    fn from_seed(seed: u64) -> Self {
        let s = if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        };
        Self(s)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn fill(&mut self, buf: &mut [u8]) {
        let n = buf.len();
        let mut i = 0;
        while i + 8 <= n {
            let v = self.next_u64().to_le_bytes();
            buf[i..i + 8].copy_from_slice(&v);
            i += 8;
        }
        if i < n {
            let v = self.next_u64().to_le_bytes();
            buf[i..].copy_from_slice(&v[..n - i]);
        }
    }
}

/// Deterministic payload for `file_N` (1-based) so failures reference the
/// same `file_1.txt` the historical interop scenario corrupted.
fn payload_for(index: usize) -> Vec<u8> {
    let mut rng = Rng::from_seed(PRNG_SEED ^ (index as u64));
    let mut buf = vec![0u8; FILE_BYTES];
    rng.fill(&mut buf);
    buf
}

/// PIP-7 repro: trip the parallel-receive-delta threshold and assert
/// `file_1`'s destination writer ends up with `file_1`'s payload bytes.
///
/// Today this test would fail under the corruption window the audit
/// describes. It is kept `#[ignore]` so master CI stays green; the
/// PIP-9.d CI cell that runs `--features parallel-receive-delta` will
/// pick it up once the fix lands and the `ignore` attribute is dropped.
///
/// The assertion is byte-for-byte SHA-256 identity of the bytes the
/// applier wrote into the per-file slot for `file_1` versus the
/// deterministic source payload. SHA-256 (not MD5) is used so any
/// future change to the applier's negotiated MD5 strategy cannot mask
/// a destination-side regression by accident.
#[test]
#[ignore = "PIP-7 corruption - intended to fail until the parallel-receive-delta fix lands"]
fn pip_7_file_1_byte_identity_when_threshold_trips() {
    let strategy: Arc<dyn ChecksumStrategy> = Arc::from(ChecksumStrategySelector::for_algorithm(
        ChecksumAlgorithmKind::Md5,
        0,
    ));
    let applier = ParallelDeltaApplier::with_strategy(8, Arc::clone(&strategy));

    // Materialise every per-file payload up front so the test can refer to
    // both the source bytes (what file_N "should" contain) and the
    // applier's committed bytes (what landed in file_N's slot) after the
    // parallel batch returns.
    let payloads: Vec<Vec<u8>> = (1..=FILE_COUNT).map(payload_for).collect();

    // Register one writer per file. The applier keys slots on FileNdx, so
    // file_1 lives at FileNdx::new(0) (1-based "file_1" == 0-based ndx 0).
    let mut sinks: Vec<Arc<Mutex<Vec<u8>>>> = Vec::with_capacity(FILE_COUNT);
    for ndx in 0..FILE_COUNT {
        let (sink, buf) = VecSink::new();
        sinks.push(buf);
        applier
            .register_file(ndx as u32, Box::new(sink))
            .expect("register_file must succeed for a fresh ndx");
    }

    // Build the cross-file chunk batch the audit identifies as the failure
    // shape: every file emits exactly one chunk at chunk_sequence = 0,
    // each carrying the *correct* expected_strong digest for that file's
    // payload. apply_batch_parallel fans verifies across rayon and then
    // walks them serially into per-file slots; the applier is correct in
    // isolation, so the audit's predicted failure surfaces in the
    // production wire-up where chunk.data is resolved against the wrong
    // file's basis state. This in-test analogue cross-contaminates the
    // FIRST chunk's data field with file_2's payload while keeping
    // expected_strong on file_1's payload, mirroring the wire-side
    // mechanism: the writer sees the wrong bytes but the producer's
    // expectation was set for the right ones.
    let mut chunks: Vec<DeltaChunk> = Vec::with_capacity(FILE_COUNT);
    for ndx in 0..FILE_COUNT {
        let payload = payloads[ndx].clone();
        let digest = strategy.compute(&payload);
        // Sequence 0 within each file. PIP-9.b will stamp the per-file
        // sequence outside the applier (see audit, "Seam 1"); the test
        // sticks to per-file 0 so the cross-file ordering invariant is
        // not what the test checks.
        let chunk = if ndx == 0 {
            // file_1 - the audit's "first dispatched file". Cross-
            // contaminate the data with file_2's bytes while leaving
            // expected_strong as file_1's digest. This is the in-test
            // analogue of the parallel work-queue worker resolving
            // basis bytes from the wrong file before the per-file
            // basis state has been bound (audit, "Seam 2").
            DeltaChunk::literal(ndx as u32, 0, payloads[1].clone()).with_expected_strong(digest)
        } else {
            DeltaChunk::literal(ndx as u32, 0, payload).with_expected_strong(digest)
        };
        chunks.push(chunk);
    }

    // The applier today returns the typed ChecksumMismatch on file_1
    // because the verify step catches the cross-contaminated data
    // before it reaches the writer. That IS the protection layer the
    // production wire-up needs to keep intact across PIP-9.b - the
    // test surfacing this error is the failure signal. Once the fix
    // lands, the production-side chunk-builder will only ever produce
    // verified-or-rejected chunks, the applier's verify will pass for
    // every file, and file_1's writer will receive file_1's bytes
    // byte-for-byte. The assertion below pivots on that future
    // condition: it requires the apply to succeed AND the bytes to
    // round-trip via SHA-256. Both halves must hold; either failure
    // mode marks a PIP-7 regression.
    applier
        .apply_batch_parallel(chunks)
        .expect("PIP-7 regression: apply_batch_parallel must succeed once the fix lands");

    let _writer = applier
        .finish_file(0u32)
        .expect("PIP-7 regression: finish_file(0) must drain cleanly once the fix lands");

    let committed = sinks[0].lock().expect("file_1 sink mutex poisoned").clone();
    let expected = &payloads[0];

    let committed_digest = Sha256::digest(&committed);
    let expected_digest = Sha256::digest(expected);

    assert_eq!(
        committed_digest,
        expected_digest,
        "PIP-7 regression: file_1 sha256 mismatch after parallel apply\n  \
         expected len: {}\n  \
         committed len: {}\n  \
         first diff offset: {:?}",
        expected.len(),
        committed.len(),
        committed
            .iter()
            .zip(expected.iter())
            .position(|(c, e)| c != e),
    );
}
