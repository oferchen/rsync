//! Criterion micro-benchmark: per-item overhead of `std::sync::mpsc` versus
//! `crossbeam_channel` across producer/consumer fan-in/fan-out shapes.
//!
//! # Why this exists
//!
//! The transfer hot path moves work between threads through channels. Today
//! the workspace uses a mix: `std::sync::mpsc` in some places and
//! `crossbeam_channel` (both `unbounded` and `bounded`) in others. The
//! question this bench answers is: at the 100K-item rates the small-file
//! transfer pipeline actually sees, what is the per-item send+recv cost of
//! each channel kind, and does the choice matter once payload work is layered
//! on top.
//!
//! # What it measures
//!
//! Per-item send-then-receive cost only. Payloads are pre-allocated outside
//! the timed section and shipped through the channel by `Arc`-wrapped clone;
//! the consumer drains every item before the iteration completes so the
//! channel is fully empty before the next sample. There is no I/O, no
//! filesystem, no syscall beyond what the channel itself performs - this
//! isolates channel cost from any payload work.
//!
//! Three channel kinds:
//!
//! 1. `std::sync::mpsc::channel()` - the stdlib MPSC. Single-consumer by
//!    construction; with N>1 receivers each receiver gets its own channel
//!    and producers round-robin across them.
//! 2. `crossbeam_channel::unbounded()` - lock-free MPMC, no back-pressure.
//! 3. `crossbeam_channel::bounded(1024)` - lock-free MPMC with a fixed
//!    1024-slot ring, which is the size most call sites in the repo use
//!    when they want back-pressure.
//!
//! Payload sizes `{32 B, 256 B, 4 KB}` bracket the realistic transfer
//! sizes: a 32-byte stat record, a 256-byte file-list entry, and a 4 KB
//! block of file data. Thread shapes `{1S+1R, 4S+1R, 1S+4R, 4S+4R}` cover
//! the contention regimes that appear in the engine and receiver pipelines.
//! Item count fixed at `ITEMS = 100_000` to model the small-file hot path
//! and match the scale of sister benches (`par_bridge_vs_deque`,
//! `parallel_stat_collector_contention`).
//!
//! # Expected outcome and the action it informs
//!
//! - At T=1S+1R the two crossbeam variants and stdlib mpsc should converge
//!   on small payloads (no contention, no back-pressure pressure).
//! - At 4S+1R the stdlib MPSC pays its single mutex-protected receive head;
//!   crossbeam should pull ahead because its MPMC ring is lock-free under
//!   producer contention.
//! - At 1S+4R the stdlib mpsc must spread work across N channels because it
//!   is single-consumer; crossbeam's unbounded fans out naturally.
//! - At 4S+4R the bounded(1024) row exposes how much back-pressure parking
//!   costs when both sides spin.
//!
//! Action this evidence informs:
//!
//! - If crossbeam beats stdlib by > 20% at any contention shape, #1681
//!   (transfer pipeline channel standardisation) should land on crossbeam
//!   workspace-wide.
//! - If stdlib MPSC is within 10% across all shapes, #1370 (replace
//!   crossbeam in low-fan-out sites) becomes attractive: drop a dependency
//!   for parity performance.
//! - The bounded vs unbounded delta sets the threshold at which adding
//!   back-pressure to a hot path is free versus measurable.
//!
//! # Cross-platform
//!
//! Pure userspace synchronisation only. No platform-specific I/O, no
//! io_uring, no IOCP. Runs identically on Linux, macOS, and Windows.
//!
//! Run: `cargo bench -p transfer -- sync_channel_overhead`

#![deny(unsafe_code)]

use std::hint::black_box;
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::thread;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use crossbeam_channel::{bounded as cb_bounded, unbounded as cb_unbounded};

/// Item count modelling a 100K small-file transfer. Held constant so each
/// criterion row reports the same throughput denominator.
const ITEMS: usize = 100_000;

/// Bounded-channel capacity. 1024 mirrors the size most repo call sites use
/// when they want back-pressure without choking the producer.
const BOUNDED_CAP: usize = 1024;

/// Payload sizes that bracket realistic transfer traffic: a stat-sized
/// record, a file-list-entry-sized record, and a small file-data block.
const PAYLOAD_SIZES: &[usize] = &[32, 256, 4096];

/// Thread-shape sweep. First entry is the no-contention baseline; the rest
/// cover producer-fan-in, consumer-fan-out, and balanced fan-in/fan-out.
const THREAD_SHAPES: &[(usize, usize)] = &[(1, 1), (4, 1), (1, 4), (4, 4)];

/// The payload shipped through the channel. `Arc<[u8]>` keeps cloning to a
/// single refcount bump so the bench measures channel cost, not allocator
/// cost. The trailing `index` field varies per-item so the optimiser cannot
/// fold the send loop into a constant.
#[derive(Clone)]
struct Item {
    index: u64,
    payload: Arc<[u8]>,
}

/// Pre-builds `ITEMS` items of `payload_size` bytes. Called once per
/// iteration outside the timed section so allocation cost is not charged
/// to the channel measurement.
fn build_items(payload_size: usize) -> Vec<Item> {
    let payload: Arc<[u8]> = Arc::from(vec![0xA5u8; payload_size].into_boxed_slice());
    (0..ITEMS as u64)
        .map(|index| Item {
            index,
            payload: Arc::clone(&payload),
        })
        .collect()
}

/// XOR-fold the consumer applies to each received item. Returns a value
/// the caller can `black_box` so the consumer loop cannot be optimised
/// away. The fold touches both fields of `Item` so neither can be elided.
#[inline]
fn consume(item: &Item) -> u64 {
    let mut h = item.index;
    if let Some(&b) = item.payload.first() {
        h ^= u64::from(b);
    }
    if let Some(&b) = item.payload.last() {
        h = h.wrapping_add(u64::from(b));
    }
    h
}

/// Splits `ITEMS` into `producers` contiguous chunks. Each producer ships
/// exactly its slice; together they cover the full set without overlap.
fn chunk_ranges(producers: usize) -> Vec<(usize, usize)> {
    let chunk = ITEMS / producers;
    let mut ranges = Vec::with_capacity(producers);
    let mut start = 0;
    for i in 0..producers {
        let end = if i + 1 == producers {
            ITEMS
        } else {
            start + chunk
        };
        ranges.push((start, end));
        start = end;
    }
    ranges
}

/// Drives `producers` sender threads and `consumers` receiver threads
/// against a single stdlib `mpsc` channel. The stdlib MPSC is
/// single-consumer, so multi-consumer shapes share the receiver through a
/// mutex - this is the only way to use the stdlib type for fan-out and is
/// the cost the bench is measuring.
fn run_std_mpsc(items: &[Item], producers: usize, consumers: usize) -> u64 {
    let (tx, rx) = std_mpsc::channel::<Item>();
    let rx = Arc::new(std::sync::Mutex::new(rx));
    let ranges = chunk_ranges(producers);

    let mut producer_handles = Vec::with_capacity(producers);
    for (start, end) in ranges {
        let tx = tx.clone();
        let slice: Vec<Item> = items[start..end].to_vec();
        producer_handles.push(thread::spawn(move || {
            for item in slice {
                tx.send(item).expect("std mpsc send");
            }
        }));
    }
    drop(tx);

    let mut consumer_handles = Vec::with_capacity(consumers);
    for _ in 0..consumers {
        let rx = Arc::clone(&rx);
        consumer_handles.push(thread::spawn(move || {
            let mut acc = 0u64;
            loop {
                let next = {
                    let guard = rx.lock().expect("std mpsc rx mutex");
                    guard.recv()
                };
                match next {
                    Ok(item) => acc ^= consume(&item),
                    Err(_) => break,
                }
            }
            acc
        }));
    }

    for h in producer_handles {
        h.join().expect("std mpsc producer join");
    }
    let mut acc = 0u64;
    for h in consumer_handles {
        acc ^= h.join().expect("std mpsc consumer join");
    }
    acc
}

/// Drives `producers` senders and `consumers` receivers against a single
/// `crossbeam_channel::unbounded` channel. Crossbeam is MPMC natively,
/// so receivers share the same handle without an external mutex.
fn run_crossbeam_unbounded(items: &[Item], producers: usize, consumers: usize) -> u64 {
    let (tx, rx) = cb_unbounded::<Item>();
    let ranges = chunk_ranges(producers);

    let mut producer_handles = Vec::with_capacity(producers);
    for (start, end) in ranges {
        let tx = tx.clone();
        let slice: Vec<Item> = items[start..end].to_vec();
        producer_handles.push(thread::spawn(move || {
            for item in slice {
                tx.send(item).expect("crossbeam unbounded send");
            }
        }));
    }
    drop(tx);

    let mut consumer_handles = Vec::with_capacity(consumers);
    for _ in 0..consumers {
        let rx = rx.clone();
        consumer_handles.push(thread::spawn(move || {
            let mut acc = 0u64;
            while let Ok(item) = rx.recv() {
                acc ^= consume(&item);
            }
            acc
        }));
    }

    for h in producer_handles {
        h.join().expect("crossbeam unbounded producer join");
    }
    let mut acc = 0u64;
    for h in consumer_handles {
        acc ^= h.join().expect("crossbeam unbounded consumer join");
    }
    acc
}

/// Drives `producers` senders and `consumers` receivers against a
/// `crossbeam_channel::bounded(BOUNDED_CAP)` channel. Adds back-pressure:
/// senders park when the ring is full, receivers park when it is empty.
fn run_crossbeam_bounded(items: &[Item], producers: usize, consumers: usize) -> u64 {
    let (tx, rx) = cb_bounded::<Item>(BOUNDED_CAP);
    let ranges = chunk_ranges(producers);

    let mut producer_handles = Vec::with_capacity(producers);
    for (start, end) in ranges {
        let tx = tx.clone();
        let slice: Vec<Item> = items[start..end].to_vec();
        producer_handles.push(thread::spawn(move || {
            for item in slice {
                tx.send(item).expect("crossbeam bounded send");
            }
        }));
    }
    drop(tx);

    let mut consumer_handles = Vec::with_capacity(consumers);
    for _ in 0..consumers {
        let rx = rx.clone();
        consumer_handles.push(thread::spawn(move || {
            let mut acc = 0u64;
            while let Ok(item) = rx.recv() {
                acc ^= consume(&item);
            }
            acc
        }));
    }

    for h in producer_handles {
        h.join().expect("crossbeam bounded producer join");
    }
    let mut acc = 0u64;
    for h in consumer_handles {
        acc ^= h.join().expect("crossbeam bounded consumer join");
    }
    acc
}

/// Registers one bench group per payload size, with three channel-kind
/// rows parametric on each thread shape. Throughput is `Elements(ITEMS)`
/// so the reader gets items/sec directly off the criterion summary.
fn bench_channels(c: &mut Criterion) {
    for &payload_size in PAYLOAD_SIZES {
        // Pre-allocate items once per payload size, outside the timed
        // section. The per-iteration cost is the channel transit, not the
        // payload construction.
        let items = build_items(payload_size);

        let mut group = c.benchmark_group(format!("sync_channel_overhead/payload_{payload_size}B"));
        group.throughput(Throughput::Elements(ITEMS as u64));
        group.sample_size(10);

        for &(producers, consumers) in THREAD_SHAPES {
            let shape = format!("{producers}S+{consumers}R");

            group.bench_with_input(BenchmarkId::new("std_mpsc", &shape), &shape, |b, _| {
                b.iter(|| black_box(run_std_mpsc(&items, producers, consumers)))
            });

            group.bench_with_input(
                BenchmarkId::new("crossbeam_unbounded", &shape),
                &shape,
                |b, _| b.iter(|| black_box(run_crossbeam_unbounded(&items, producers, consumers))),
            );

            group.bench_with_input(
                BenchmarkId::new("crossbeam_bounded_1024", &shape),
                &shape,
                |b, _| b.iter(|| black_box(run_crossbeam_bounded(&items, producers, consumers))),
            );
        }

        group.finish();
    }
}

criterion_group!(benches, bench_channels);
criterion_main!(benches);
