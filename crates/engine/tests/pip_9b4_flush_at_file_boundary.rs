//! PIP-9.b.4 - verify `flush_workers` drain at file boundary in the
//! parallel-receive-delta path.
//!
//! The `ParallelDeltaApplier::finish_file` API bakes a `flush_workers` barrier
//! (drain.rs:58) before reclaiming the per-file writer. This means any caller
//! that invokes `finish_file(ndx)` at each file boundary is guaranteed that all
//! in-flight chunks for that file have been applied before the writer is returned.
//!
//! This test exercises the contract under concurrent load:
//!
//! 1. Multiple files are registered with the applier.
//! 2. Chunks are dispatched concurrently across rayon workers (simulating the
//!    parallel-receive-delta path where verify + write happen on worker threads).
//! 3. At each "file boundary" (after all chunks for file N are submitted),
//!    `finish_file(N)` is called before proceeding to the next file.
//! 4. The test asserts that each file's writer received exactly its own bytes
//!    in the correct order, and no cross-contamination occurred.
//!
//! This proves the production wire-up (PIP-9.b) can safely call `finish_file`
//! at each file boundary without an explicit `flush_workers` - the barrier is
//! internal to `finish_file`.
//!
//! # Upstream reference
//!
//! - `receiver.c:recv_files()` processes files sequentially; the parallel path
//!   must maintain byte-identical per-file output despite concurrent chunk
//!   dispatch.

#![cfg(feature = "parallel-receive-delta")]

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use engine::concurrent_delta::{DeltaChunk, ParallelDeltaApplier};

/// Number of files processed sequentially (each drained at its boundary).
const NUM_FILES: u32 = 16;

/// Chunks per file. High enough to guarantee concurrent in-flight work during
/// the `finish_file` barrier.
const CHUNKS_PER_FILE: u64 = 128;

/// Bytes per chunk payload.
const CHUNK_BYTES: usize = 64;

/// In-memory writer that records bytes written. One per file.
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
        self.buf
            .lock()
            .map_err(|_| io::Error::other("sink mutex poisoned"))?
            .extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Deterministic payload for a given file and chunk sequence.
fn payload_for(ndx: u32, seq: u64) -> Vec<u8> {
    let seed = (ndx as u64) ^ (seq.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    (0..CHUNK_BYTES)
        .map(|i| ((seed ^ i as u64) & 0xFF) as u8)
        .collect()
}

/// Computes expected bytes for a file (all chunks concatenated in order).
fn expected_bytes_for(ndx: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(CHUNKS_PER_FILE as usize * CHUNK_BYTES);
    for seq in 0..CHUNKS_PER_FILE {
        out.extend_from_slice(&payload_for(ndx, seq));
    }
    out
}

/// PIP-9.b.4: `finish_file` drains in-flight chunks at each file boundary.
///
/// Dispatches chunks for each file from multiple rayon workers, then calls
/// `finish_file` at the boundary. Verifies byte-exact output per file.
#[test]
fn finish_file_drains_inflight_at_file_boundary() {
    let applier =
        ParallelDeltaApplier::new(4).with_per_file_reorder_capacity(CHUNKS_PER_FILE as usize);

    // Process files sequentially: for each file, register, dispatch all
    // chunks (potentially out-of-order to exercise reorder buffering),
    // then call finish_file to drain and reclaim the writer.
    for ndx in 0..NUM_FILES {
        let (sink, buf) = VecSink::new();
        applier.register_file(ndx, Box::new(sink)).unwrap();

        // Submit chunks in a scrambled order to exercise the reorder buffer.
        // Rotate by a prime offset to ensure non-trivial reordering.
        let rotate_by = 37 % CHUNKS_PER_FILE as usize;
        let mut order: Vec<u64> = (0..CHUNKS_PER_FILE).collect();
        order.rotate_left(rotate_by);

        for &seq in &order {
            let data = payload_for(ndx, seq);
            let chunk = DeltaChunk::literal(ndx, seq, data);
            applier
                .apply_one_chunk(chunk)
                .unwrap_or_else(|e| panic!("apply_one_chunk(ndx={ndx}, seq={seq}) failed: {e}"));
        }

        // File boundary: finish_file calls flush_workers internally,
        // waits for all in-flight chunks to drain, then returns the writer.
        let _writer = applier
            .finish_file(ndx)
            .unwrap_or_else(|e| panic!("finish_file(ndx={ndx}) failed at file boundary: {e}"));

        // Verify byte-exact output for this file.
        let committed = buf.lock().expect("sink mutex").clone();
        let expected = expected_bytes_for(ndx);
        assert_eq!(
            committed.len(),
            expected.len(),
            "file {ndx}: length mismatch (got {} bytes, expected {} bytes)",
            committed.len(),
            expected.len(),
        );
        assert_eq!(
            committed,
            expected,
            "file {ndx}: byte mismatch at offset {:?}",
            committed
                .iter()
                .zip(expected.iter())
                .position(|(a, b)| a != b),
        );
    }
}

/// PIP-9.b.4: interleaved multi-file dispatch with boundary drains.
///
/// Registers multiple files simultaneously and dispatches chunks for all files
/// concurrently, then drains each file at its boundary in sequence. This models
/// the scenario where the receiver has multiple files in-flight through the
/// applier (INC_RECURSE mode) and must drain each one before committing.
#[test]
fn interleaved_multi_file_dispatch_with_boundary_drain() {
    use rayon::prelude::*;

    let applier = Arc::new(
        ParallelDeltaApplier::new(8).with_per_file_reorder_capacity(CHUNKS_PER_FILE as usize),
    );
    let mut sinks: Vec<Arc<Mutex<Vec<u8>>>> = Vec::with_capacity(NUM_FILES as usize);

    // Register all files up front.
    for ndx in 0..NUM_FILES {
        let (sink, buf) = VecSink::new();
        sinks.push(buf);
        applier.register_file(ndx, Box::new(sink)).unwrap();
    }

    // Dispatch all chunks for all files concurrently via rayon.
    // This exercises the DashMap sharding under cross-file concurrent access.
    let all_chunks: Vec<DeltaChunk> = (0..NUM_FILES)
        .flat_map(|ndx| {
            (0..CHUNKS_PER_FILE).map(move |seq| {
                let data = payload_for(ndx, seq);
                DeltaChunk::literal(ndx, seq, data)
            })
        })
        .collect();

    // Shuffle deterministically: process in ndx-interleaved order so chunks
    // from different files race through the applier simultaneously.
    let mut shuffled = all_chunks;
    // Stripe by sequence: all files' seq=0 first, then all seq=1, etc.
    shuffled.sort_by_key(|c| (c.chunk_sequence, c.ndx.get()));

    shuffled.par_iter().for_each(|chunk| {
        applier.apply_one_chunk(chunk.clone()).unwrap_or_else(|e| {
            panic!(
                "apply_one_chunk(ndx={}, seq={}) failed: {e}",
                chunk.ndx.get(),
                chunk.chunk_sequence
            )
        });
    });

    // Drain each file at its boundary. finish_file bakes flush_workers,
    // so all in-flight chunks for the file are guaranteed applied before
    // the writer is reclaimed.
    for ndx in 0..NUM_FILES {
        let _writer = applier
            .finish_file(ndx)
            .unwrap_or_else(|e| panic!("finish_file(ndx={ndx}) failed at file boundary: {e}"));

        let committed = sinks[ndx as usize].lock().expect("sink mutex").clone();
        let expected = expected_bytes_for(ndx);
        assert_eq!(
            committed.len(),
            expected.len(),
            "file {ndx}: length mismatch (got {} bytes, expected {} bytes)",
            committed.len(),
            expected.len(),
        );
        assert_eq!(
            committed,
            expected,
            "file {ndx}: byte mismatch at offset {:?}",
            committed
                .iter()
                .zip(expected.iter())
                .position(|(a, b)| a != b),
        );
    }
}
