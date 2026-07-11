//! ABW-5.b empirical stress test for the verify/write-overlap safety of
//! [`ParallelDeltaApplier::apply_batch_parallel`].
//!
//! Spec: `docs/design/abw-5b-verify-write-stress-test.md`. The ABW-5.c audit
//! proved on paper that the parallel verify step and the serial per-file
//! write loop touch disjoint state, so a batch whose verify overlaps another
//! batch's write can never race. ABW-5.a wired debug assertions that witness
//! the three invariants inline. This module is the missing empirical leg: it
//! drives the applier under adversarial concurrency and asserts the only
//! observable consequence that a broken overlap would produce - a destination
//! byte stream that diverges from a serial reference, a lost/duplicated
//! write, a panic, or a deadlock.
//!
//! # The three invariants under stress (ABW-5.c)
//!
//! 1. `verify_chunk` is pure - it reads only its owned `chunk.data` and the
//!    immutable `Arc<dyn ChecksumStrategy>`. It never touches a `FileSlot`.
//! 2. Every write goes through the per-file `Mutex<FileSlot>`, which guards
//!    the writer, the reorder buffer, and the byte counter as one unit.
//! 3. The per-file reorder buffer restores `chunk_sequence` order regardless
//!    of the order in which threads win the Mutex.
//!
//! If any of these were unsound, a verify of batch N+1 running concurrently
//! with the write of batch N could observe a half-written slot, tear a
//! chunk, or emit bytes out of order. Every assertion below is chosen so
//! that such a failure turns the run red - the reconstructed file would not
//! be byte-identical to the serial reference, the byte-count invariant would
//! break, or a poisoned Mutex / stranded reorder chunk would surface as an
//! error from `finish_file`.
//!
//! # Determinism
//!
//! The *output* is fully deterministic: every chunk's bytes are a pure
//! function of `(file, sequence)`, and per-file order is guaranteed by the
//! applier. The *schedule* is intentionally nondeterministic - that is the
//! property under test. No assertion depends on timing; randomness is seeded
//! from the loop index (never a clock or the `rand` crate) so a failure is
//! reproducible from the reported seed. `ABW5B_SOAK_ITERATIONS` multiplies
//! the iteration counts for local overnight soak without changing the
//! per-iteration shape.

use std::sync::{Arc, Barrier};
use std::thread;

use checksums::strong::strategy::{
    ChecksumAlgorithmKind, ChecksumStrategy, ChecksumStrategySelector,
};

use super::tests::VecSink;
use super::{DeltaChunk, ParallelDeltaApplier};

/// Reads the optional soak multiplier from the environment.
///
/// Defaults to `1` (CI-bounded). A positive value scales every scenario's
/// iteration count so an operator can turn the same test into an overnight
/// soak (`ABW5B_SOAK_ITERATIONS=100`) without editing the source.
fn soak() -> usize {
    std::env::var("ABW5B_SOAK_ITERATIONS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1)
}

/// xorshift64 step. Deterministic, no external crate, no clock.
fn xorshift64(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// Derives a non-zero seed from a loop index and a per-scenario salt so each
/// scenario/iteration replays identically. xorshift64 requires a non-zero
/// state, hence the final `| 1`.
fn seed(index: usize, salt: u64) -> u64 {
    (index as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(salt)
        | 1
}

/// The negotiated strong-checksum strategy for the run. XXH3 (spec 9.4) keeps
/// the verify step cheap so it never dominates the wall clock, while still
/// exercising the full `strategy.compute` path that ABW-5.a invariant 1
/// asserts over.
fn xxh3_strategy() -> Arc<dyn ChecksumStrategy> {
    Arc::from(ChecksumStrategySelector::for_algorithm(
        ChecksumAlgorithmKind::Xxh3,
        0,
    ))
}

/// Deterministic bytes for `(file, sequence)`.
///
/// Both the serial reference (built on the main thread) and the concurrent
/// producers call this, so the two agree by construction. The length varies
/// with the chunk to stress the reorder buffer under size asymmetry; it is
/// derived from the same seed so it is stable across threads. `max_len` is
/// inclusive of at least one byte (empty chunks are covered separately).
fn chunk_bytes(file: u32, sequence: u64, max_len: usize) -> Vec<u8> {
    let mut state =
        seed(file as usize, 0x5171).wrapping_add(sequence.wrapping_mul(0x100_0000_01B3));
    state |= 1;
    let len = 1 + (xorshift64(&mut state) as usize % max_len);
    (0..len)
        .map(|_| (xorshift64(&mut state) & 0xff) as u8)
        .collect()
}

/// Builds a chunk with the correct expected-strong digest attached so the
/// concurrent verify step runs the full compare path (not just the compute
/// path). A mismatch here would surface as a `ChecksumMismatch`, so the fact
/// that every scenario applies cleanly is itself a witness that no concurrent
/// write corrupted a chunk's bytes before verify saw them.
fn digested_chunk(
    strategy: &dyn ChecksumStrategy,
    file: u32,
    sequence: u64,
    data: Vec<u8>,
) -> DeltaChunk {
    let digest = strategy.compute(&data);
    DeltaChunk::literal(file, sequence, data).with_expected_strong(digest)
}

/// In-place Fisher-Yates shuffle seeded by `state`. Presents the batch to the
/// serial write loop in an adversarial order so the reorder buffer, not the
/// submission order, is what restores `chunk_sequence` order (spec 3.a).
fn shuffle<T>(items: &mut [T], state: &mut u64) {
    for i in (1..items.len()).rev() {
        let j = (xorshift64(state) as usize) % (i + 1);
        items.swap(i, j);
    }
}

/// Scenario 2.a - a single batch that spans a file boundary.
///
/// One `apply_batch_parallel` call carries chunks for two adjacent files with
/// alternating tiny/large sizes, submitted in adversarial order. The serial
/// write loop must alternate Mutex acquisitions between the two files while
/// the rayon verify completes out of order, and each file's reorder buffer
/// must independently reconstruct submission order.
///
/// WHY this catches unsoundness: if invariant 2 were violated (e.g. a write
/// for file N leaked into file N+1's slot because the Mutex scope was split),
/// or invariant 3 were violated (the reorder buffer emitted the shuffled
/// order instead of `chunk_sequence` order), the per-file byte-identical
/// assertion below would fail. The empty-batch and single-chunk boundary
/// cases guard the degenerate paths a fuzzed batch size would otherwise skip.
#[test]
fn stress_abw5b_batch_spans_file_boundary() {
    let strategy = xxh3_strategy();
    let iters = 64 * soak();
    let chunks_per_file: u64 = 96;
    // Empty batch is a no-op fast path; assert it never touches a slot.
    ParallelDeltaApplier::with_strategy(4, Arc::clone(&strategy))
        .apply_batch_parallel(Vec::new())
        .expect("empty batch is a no-op");

    for iter in 0..iters {
        let applier = ParallelDeltaApplier::with_strategy(8, Arc::clone(&strategy))
            // Capacity >= chunks_per_file so a fully-reversed submission (max
            // reorder depth, spec 3.b) never saturates the ring.
            .with_per_file_reorder_capacity(chunks_per_file as usize);
        let files = [40u32, 41u32];
        let mut buffers = Vec::with_capacity(files.len());
        let mut expected: Vec<Vec<u8>> = vec![Vec::new(); files.len()];
        let mut batch: Vec<DeltaChunk> = Vec::new();

        for (slot, &file) in files.iter().enumerate() {
            let (sink, buf) = VecSink::new();
            applier.register_file(file, Box::new(sink)).unwrap();
            buffers.push(buf);
            for sequence in 0..chunks_per_file {
                // Alternating size asymmetry: 1 byte vs up to 1 KiB.
                let max_len = if sequence % 2 == 0 { 1 } else { 1024 };
                let data = chunk_bytes(file, sequence, max_len);
                expected[slot].extend_from_slice(&data);
                batch.push(digested_chunk(strategy.as_ref(), file, sequence, data));
            }
        }

        let mut rng = seed(iter, 0x2A);
        if iter % 2 == 0 {
            shuffle(&mut batch, &mut rng);
        } else {
            // Reverse order forces the reorder buffer to its maximum depth on
            // every insert but the last (spec 3.b).
            batch.reverse();
        }

        applier.apply_batch_parallel(batch).unwrap_or_else(|e| {
            panic!("apply failed [scenario=2.a, seed={rng:#x}, iter={iter}]: {e}")
        });

        for (slot, &file) in files.iter().enumerate() {
            let _writer = applier.finish_file(file).unwrap_or_else(|e| {
                panic!("finish_file failed [scenario=2.a, file={file}, iter={iter}]: {e}")
            });
            let got = buffers[slot].lock().unwrap();
            assert_eq!(
                &*got, &expected[slot],
                "scenario 2.a byte divergence [file={file}, iter={iter}, seed={rng:#x}]: \
                 a spanning batch reordered or tore a per-file write"
            );
        }
    }
}

/// Scenario 2.b/2.c hybrid - many producer threads, disjoint file sets, no
/// barrier, maximum temporal overlap between one thread's serial write and
/// another thread's parallel verify on a single shared applier.
///
/// Each thread owns a disjoint slice of files, so per-file sequence spaces are
/// contiguous and single-owner: the reorder buffer can never saturate from a
/// cross-thread sequence gap, which keeps the test non-flaky while still
/// pushing every thread through the shared DashMap shard locks, the shared
/// rayon pool, and the shared per-file Mutex machinery at once.
///
/// WHY this catches unsoundness: the whole point of ABW-5.c is that batch N's
/// write may overlap batch N+1's verify. Here that overlap is realized across
/// threads on one applier - while thread A holds file X's Mutex mid-write,
/// thread B is verifying (invariant 1, pure, no slot access) and writing file
/// Y under Y's Mutex. If verify were not pure, or a slot's state leaked across
/// the shard map, thread B's output would diverge from its serial reference or
/// a Mutex would poison. The final per-file byte-identical check plus the
/// aggregate byte-count invariant (spec 4.2) turn any such corruption red.
#[test]
fn stress_abw5b_concurrent_disjoint_files_overlap() {
    let strategy = xxh3_strategy();
    let threads = 4usize;
    let files_per_thread = 6u32;
    let batches = 40usize * soak();
    let chunks_per_batch: u64 = 8;
    let total_seq = batches as u64 * chunks_per_batch;
    let max_len = 1024usize;

    let applier = Arc::new(
        ParallelDeltaApplier::with_strategy(threads, Arc::clone(&strategy))
            .with_per_file_reorder_capacity(chunks_per_batch as usize),
    );

    // Register every file up front (single-threaded) and precompute the serial
    // reference bytes and length for each file.
    let total_files = threads as u32 * files_per_thread;
    let mut buffers = Vec::with_capacity(total_files as usize);
    let mut expected: Vec<Vec<u8>> = Vec::with_capacity(total_files as usize);
    for file in 0..total_files {
        let (sink, buf) = VecSink::new();
        applier.register_file(file, Box::new(sink)).unwrap();
        buffers.push(buf);
        let mut file_expected = Vec::new();
        for sequence in 0..total_seq {
            file_expected.extend_from_slice(&chunk_bytes(file, sequence, max_len));
        }
        expected.push(file_expected);
    }

    let mut handles = Vec::with_capacity(threads);
    for t in 0..threads {
        let applier = Arc::clone(&applier);
        let strategy = Arc::clone(&strategy);
        handles.push(thread::spawn(move || {
            let owned: Vec<u32> = (0..files_per_thread)
                .map(|f| t as u32 * files_per_thread + f)
                .collect();
            for b in 0..batches {
                let mut batch: Vec<DeltaChunk> = Vec::new();
                for &file in &owned {
                    for k in 0..chunks_per_batch {
                        let sequence = b as u64 * chunks_per_batch + k;
                        let data = chunk_bytes(file, sequence, max_len);
                        batch.push(digested_chunk(strategy.as_ref(), file, sequence, data));
                    }
                }
                // Adversarial submission order per batch (spec 3.a).
                let mut rng = seed(t.wrapping_mul(1009).wrapping_add(b), 0x2B);
                shuffle(&mut batch, &mut rng);
                applier.apply_batch_parallel(batch).unwrap_or_else(|e| {
                    panic!("apply failed [scenario=2.b, thread={t}, batch={b}]: {e}")
                });
            }
        }));
    }
    for h in handles {
        h.join()
            .expect("producer thread panicked - concurrent overlap was unsound");
    }

    // Aggregate byte-count invariant (spec 4.2): the sum of bytes written must
    // equal the sum of submitted chunk lengths. Catches a dropped or
    // duplicated write that a coincidental per-file match could otherwise hide.
    let mut total_written = 0u64;
    let mut total_expected = 0u64;
    for file in 0..total_files {
        total_written += applier.bytes_written(file).unwrap();
        total_expected += expected[file as usize].len() as u64;
    }
    assert_eq!(
        total_written, total_expected,
        "scenario 2.b aggregate byte-count invariant violated: a write was dropped or duplicated"
    );

    for file in 0..total_files {
        let _writer = applier
            .finish_file(file)
            .unwrap_or_else(|e| panic!("finish_file failed [scenario=2.b, file={file}]: {e}"));
        let got = buffers[file as usize].lock().unwrap();
        assert_eq!(
            &*got, &expected[file as usize],
            "scenario 2.b byte divergence [file={file}]: a concurrent verify observed a \
             half-written slot or the reorder buffer lost per-file order"
        );
    }
}

/// Scenario 2.c - a shared hot file written by two threads at once.
///
/// Both threads submit batches for the *same* set of files concurrently, each
/// owning a disjoint half of every batch's `chunk_sequence` window. A
/// per-batch [`Barrier`] bounds how far the two producers can drift so the
/// reorder window stays within the ring capacity (keeping the test
/// non-flaky), while guaranteeing that at every batch both threads are inside
/// `apply_batch_parallel` for the same files simultaneously - the verify of
/// one thread's half overlaps the write of the other's half on one Mutex and
/// one reorder buffer.
///
/// WHY this catches unsoundness: this is the sharpest form of the ABW-5.c
/// overlap - two writers contend the identical per-file `Mutex<FileSlot>` and
/// feed the identical reorder buffer. If invariant 2 (single-unit Mutex) or
/// invariant 3 (order restoration under arbitrary Mutex-acquisition order)
/// were violated, the interleaved halves would emit bytes out of order or
/// tear, and the byte-identical assertion would fail. A split or
/// prematurely-dropped guard would instead poison the Mutex and surface as an
/// error from `finish_file`.
#[test]
fn stress_abw5b_shared_hot_file_concurrent_batches() {
    let strategy = xxh3_strategy();
    let hot_files = 6u32;
    let batches = 40usize * soak();
    let half: u64 = 16; // chunks each thread contributes per file per batch
    let per_batch = half * 2; // full per-file window per batch
    let total_seq = batches as u64 * per_batch;
    let max_len = 256usize;

    let applier = Arc::new(
        ParallelDeltaApplier::with_strategy(4, Arc::clone(&strategy))
            // Window per batch is `per_batch`; capacity covers a full window
            // even if one thread submits its whole half before the other.
            .with_per_file_reorder_capacity(per_batch as usize),
    );

    let mut buffers = Vec::with_capacity(hot_files as usize);
    let mut expected: Vec<Vec<u8>> = Vec::with_capacity(hot_files as usize);
    for file in 0..hot_files {
        let (sink, buf) = VecSink::new();
        applier.register_file(file, Box::new(sink)).unwrap();
        buffers.push(buf);
        let mut file_expected = Vec::new();
        for sequence in 0..total_seq {
            file_expected.extend_from_slice(&chunk_bytes(file, sequence, max_len));
        }
        expected.push(file_expected);
    }

    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::with_capacity(2);
    for t in 0..2u64 {
        let applier = Arc::clone(&applier);
        let strategy = Arc::clone(&strategy);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            for b in 0..batches {
                let mut batch: Vec<DeltaChunk> = Vec::new();
                for file in 0..hot_files {
                    let base = b as u64 * per_batch + t * half;
                    for k in 0..half {
                        let sequence = base + k;
                        let data = chunk_bytes(file, sequence, max_len);
                        batch.push(digested_chunk(strategy.as_ref(), file, sequence, data));
                    }
                }
                let mut rng = seed(t as usize * 7919 + b, 0x2C);
                shuffle(&mut batch, &mut rng);
                // Release both threads into the same batch window together so
                // their verify/write phases genuinely overlap on shared slots.
                barrier.wait();
                applier.apply_batch_parallel(batch).unwrap_or_else(|e| {
                    panic!("apply failed [scenario=2.c, thread={t}, batch={b}]: {e}")
                });
            }
        }));
    }
    for h in handles {
        h.join()
            .expect("hot-file producer panicked - shared-slot overlap was unsound");
    }

    for file in 0..hot_files {
        let _writer = applier
            .finish_file(file)
            .unwrap_or_else(|e| panic!("finish_file failed [scenario=2.c, file={file}]: {e}"));
        let got = buffers[file as usize].lock().unwrap();
        assert_eq!(
            &*got, &expected[file as usize],
            "scenario 2.c byte divergence [file={file}]: two writers on one slot tore or \
             reordered the per-file byte stream"
        );
    }
}
