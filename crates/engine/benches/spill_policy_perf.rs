//! `SpillableReorderBuffer` policy-tuning benchmark.
//!
//! Sweeps the four policy cells that the consumer will pick up once the
//! [`SpillPolicy`] knobs (compression + granularity) are fully wired through
//! [`SpillableReorderBuffer`]:
//!
//! - compression: [`SpillCompression::None`] vs [`SpillCompression::Zstd { level: 3 }`]
//! - granularity: [`SpillGranularity::WholeBatch`] vs [`SpillGranularity::PerItem`]
//!
//! For every cell the benchmark:
//!
//! 1. Builds 1000 deterministic out-of-order [`DeltaResult`] payloads of
//!    varying sizes (mix of `Success`, `NeedsRedo`, and `Failed` variants).
//! 2. Drives the workload through a real [`SpillableReorderBuffer`] sized so
//!    the 16 KiB threshold forces multiple spill/reload cycles.
//! 3. Re-runs the encoded payloads through a hand-rolled simulation that
//!    applies the cell's compression and granularity choices to a `Cursor`
//!    backed by a `Vec<u8>`, so the bytes-on-disk figure reflects what the
//!    cell would write once the codec wiring lands.
//!
//! Criterion captures wall-clock; a `println!` line per cell reports the
//! simulated bytes-on-disk so the cell-vs-cell space tradeoff is visible
//! alongside the timing histogram.
//!
//! Run with:
//! `cargo bench -p engine --features spill-compression --bench spill_policy_perf`

#![cfg(unix)]
#![deny(unsafe_code)]

use std::hint::black_box;
use std::io::{Cursor, Write};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use engine::concurrent_delta::spill::SpillCodec;
use engine::concurrent_delta::{
    DeltaResult, SpillCompression, SpillGranularity, SpillableReorderBuffer,
};

/// Workload size. Matches the task spec: 1000 random `DeltaResult` payloads.
const ITEM_COUNT: usize = 1000;

/// Force-spill memory threshold. Small enough that the bench exercises the
/// spill-to-disk path repeatedly across the 1000-item workload rather than
/// staying resident in the in-memory ring.
const SPILL_THRESHOLD_BYTES: usize = 16 * 1024;

/// Ring capacity. Sized to comfortably exceed the local-drift window so the
/// timed work is dominated by spill activity rather than capacity grow.
const RING_CAPACITY: usize = 256;

/// zstd compression level used for the `Zstd` cells. Matches the spec.
const ZSTD_LEVEL: i32 = 3;

/// Generates a deterministic, mildly out-of-order workload of [`DeltaResult`]
/// payloads with varying encoded sizes. The mix is:
///
/// - 70% `Success` (compact, fixed 36-byte payload)
/// - 20% `NeedsRedo` with a 64-256 byte reason string
/// - 10% `Failed` with a 256-1024 byte reason string
///
/// Sequence numbers are pre-shuffled with a small local window (drift = 8)
/// so the buffer exercises both in-order delivery and short reorder stalls.
fn build_workload(count: usize) -> Vec<(u64, DeltaResult)> {
    let mut state: u64 = 0xC0FF_EE15_DEAD_BEEF;
    let mut next_rng = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        state
    };

    let mut items: Vec<(u64, DeltaResult)> = (0..count as u64)
        .map(|seq| {
            let r = next_rng();
            let bucket = r % 10;
            let bytes_written = (r >> 8) & 0xFFFF;
            let literal_bytes = bytes_written / 2;
            let matched_bytes = bytes_written - literal_bytes;

            let result = if bucket < 7 {
                DeltaResult::success(seq as u32, bytes_written, literal_bytes, matched_bytes)
                    .with_sequence(seq)
            } else if bucket < 9 {
                let len = 64 + ((r >> 24) as usize % 192);
                DeltaResult::needs_redo(seq as u32, "x".repeat(len)).with_sequence(seq)
            } else {
                let len = 256 + ((r >> 32) as usize % 768);
                DeltaResult::failed(seq as u32, "y".repeat(len)).with_sequence(seq)
            };
            (seq, result)
        })
        .collect();

    // Apply a small local-drift shuffle so the workload looks like real
    // worker completions rather than perfect FIFO.
    let drift = 8usize;
    for i in 0..items.len() {
        let r = next_rng();
        let span = ((r >> 33) as usize) % drift;
        let j = (i + span).min(items.len() - 1);
        items.swap(i, j);
    }
    items
}

/// Encodes one item via its `SpillCodec` impl into a fresh `Vec<u8>`.
fn encode_one(item: &DeltaResult) -> Vec<u8> {
    let mut buf = Vec::with_capacity(item.estimated_size());
    item.encode(&mut buf)
        .expect("DeltaResult encode into Vec cannot fail");
    buf
}

/// Compresses `payload` with the cell's compression choice. The `None`
/// branch returns the input unchanged so the bench captures only the
/// container overhead of the surrounding write loop.
fn maybe_compress(payload: Vec<u8>, compression: SpillCompression) -> Vec<u8> {
    match compression {
        SpillCompression::None => payload,
        SpillCompression::Zstd { level } => zstd::encode_all(payload.as_slice(), level)
            .expect("zstd encode_all cannot fail on a Vec source"),
    }
}

/// Applies the cell's policy to an encoded payload stream and returns the
/// total bytes that would land on disk plus the wall-clock cost of the
/// encode + (optional) compress + write path.
///
/// The "disk" target is an in-memory `Cursor` so the bench measures the CPU
/// envelope of the cell rather than the underlying filesystem; spill-file
/// I/O cost is already covered by the real `SpillableReorderBuffer` arm.
fn simulate_cell(
    items: &[(u64, DeltaResult)],
    compression: SpillCompression,
    granularity: SpillGranularity,
) -> u64 {
    let mut sink: Cursor<Vec<u8>> = Cursor::new(Vec::with_capacity(64 * 1024));

    match granularity {
        SpillGranularity::PerItem => {
            for (_, item) in items {
                let payload = encode_one(item);
                let bytes = maybe_compress(payload, compression);
                let len = bytes.len() as u32;
                sink.write_all(&len.to_le_bytes())
                    .expect("Cursor write cannot fail");
                sink.write_all(&bytes).expect("Cursor write cannot fail");
            }
        }
        SpillGranularity::WholeBatch => {
            let mut batch = Vec::with_capacity(items.len() * 64);
            for (_, item) in items {
                let payload = encode_one(item);
                let len = payload.len() as u32;
                batch.extend_from_slice(&len.to_le_bytes());
                batch.extend_from_slice(&payload);
            }
            let bytes = maybe_compress(batch, compression);
            let len = bytes.len() as u64;
            sink.write_all(&len.to_le_bytes())
                .expect("Cursor write cannot fail");
            sink.write_all(&bytes).expect("Cursor write cannot fail");
        }
    }

    sink.into_inner().len() as u64
}

/// Drives the workload through a real [`SpillableReorderBuffer`] so the bench
/// captures the actual production code path (current default: WholeBatch +
/// None). The hand-rolled simulation in [`simulate_cell`] covers the cells
/// that are not yet wired into the buffer.
fn run_real_buffer(items: &[(u64, DeltaResult)]) -> u64 {
    let mut buf: SpillableReorderBuffer<DeltaResult> =
        SpillableReorderBuffer::new(RING_CAPACITY, SPILL_THRESHOLD_BYTES);
    let mut delivered: u64 = 0;
    for (seq, item) in items {
        if buf.insert(*seq, item.clone()).is_err() {
            // Drain to make room then force-insert. The bench is interested
            // in spill cost, not back-pressure error handling.
            while let Some(r) = buf
                .next_in_order()
                .expect("spill reload must not fail in bench")
            {
                delivered = delivered.wrapping_add(r.sequence());
            }
            buf.force_insert(*seq, item.clone())
                .expect("force_insert must not fail in bench");
        }
        while let Some(r) = buf
            .next_in_order()
            .expect("spill reload must not fail in bench")
        {
            delivered = delivered.wrapping_add(r.sequence());
        }
    }
    while let Some(r) = buf
        .next_in_order()
        .expect("spill reload must not fail in bench")
    {
        delivered = delivered.wrapping_add(r.sequence());
    }
    delivered
}

/// Stable, log-friendly label for one bench cell so Criterion history diffs
/// line up across runs.
fn cell_id(compression: SpillCompression, granularity: SpillGranularity) -> String {
    let comp = match compression {
        SpillCompression::None => "none".to_string(),
        SpillCompression::Zstd { level } => format!("zstd{level}"),
    };
    let gran = match granularity {
        SpillGranularity::WholeBatch => "whole_batch",
        SpillGranularity::PerItem => "per_item",
    };
    format!("{comp}/{gran}")
}

fn bench_policy_cells(c: &mut Criterion) {
    let items = build_workload(ITEM_COUNT);

    let mut group = c.benchmark_group("spill_policy_perf");
    group.throughput(Throughput::Elements(ITEM_COUNT as u64));
    group.sample_size(20);

    // Real-buffer baseline: captures the current production wiring
    // (WholeBatch + None) against the actual SpillableReorderBuffer code
    // path so the simulated cells can be read relative to a known anchor.
    group.bench_function("real_buffer/baseline", |b| {
        b.iter(|| black_box(run_real_buffer(black_box(&items))));
    });

    let cells: [(SpillCompression, SpillGranularity); 4] = [
        (SpillCompression::None, SpillGranularity::WholeBatch),
        (SpillCompression::None, SpillGranularity::PerItem),
        (
            SpillCompression::Zstd { level: ZSTD_LEVEL },
            SpillGranularity::WholeBatch,
        ),
        (
            SpillCompression::Zstd { level: ZSTD_LEVEL },
            SpillGranularity::PerItem,
        ),
    ];

    for (compression, granularity) in cells {
        let id = cell_id(compression, granularity);
        let bytes_on_disk = simulate_cell(&items, compression, granularity);
        println!("spill_policy_perf: cell={id} items={ITEM_COUNT} bytes_on_disk={bytes_on_disk}");

        group.bench_with_input(BenchmarkId::new("simulate", &id), &items, |b, items| {
            b.iter(|| {
                let bytes = simulate_cell(black_box(items), compression, granularity);
                black_box(bytes);
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_policy_cells);
criterion_main!(benches);
