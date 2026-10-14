//! Reorder loop driven by the delta-reorder background thread.
//!
//! [`run_bare_loop`] drives the in-memory [`ReorderBuffer`] and forwards
//! contiguous in-order runs to the consumer channel.

use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

use crate::concurrent_delta::reorder::{Metrics as ReorderMetrics, ReorderBuffer};
use crate::concurrent_delta::types::DeltaResult;
use crate::throughput::{Constraint, SampleSink};

/// Reorder loop for the bare [`ReorderBuffer`] backend (passthrough or
/// ring-buffer mode). Mirrors the historical control flow: drain ready items
/// to free space, force-insert if the buffer is full and the head is missing.
/// Publishes a metrics snapshot after every `force_insert` and every non-empty
/// drain so callers can poll [`crate::concurrent_delta::consumer::DeltaConsumer::metrics`]
/// without locking the pipeline.
pub(super) fn run_bare_loop(
    stream_rx: crossbeam_channel::Receiver<DeltaResult>,
    result_tx: &mpsc::Sender<DeltaResult>,
    mut reorder: ReorderBuffer<DeltaResult>,
    metrics: &Arc<Mutex<ReorderMetrics>>,
    sink: Option<&SampleSink>,
) {
    let mut last_drain_at: Option<Instant> = None;
    let publish = |reorder: &ReorderBuffer<DeltaResult>| {
        if let Ok(mut guard) = metrics.lock() {
            *guard = reorder.metrics();
        }
    };

    for result in stream_rx {
        while reorder.insert(result.sequence(), result.clone()).is_err() {
            match drain_and_record(&mut reorder, result_tx, &mut last_drain_at, sink) {
                DrainOutcome::Disconnected => return,
                DrainOutcome::Empty => {
                    // Buffer full but next_expected is not buffered.
                    // Force insert to break the deadlock.
                    reorder.force_insert(result.sequence(), result.clone());
                    publish(&reorder);
                    break;
                }
                DrainOutcome::Forwarded => {
                    publish(&reorder);
                }
            }
        }

        match drain_and_record(&mut reorder, result_tx, &mut last_drain_at, sink) {
            DrainOutcome::Disconnected => return,
            DrainOutcome::Empty => {}
            DrainOutcome::Forwarded => publish(&reorder),
        }
    }

    match drain_and_record(&mut reorder, result_tx, &mut last_drain_at, sink) {
        DrainOutcome::Disconnected => return,
        DrainOutcome::Empty => {}
        DrainOutcome::Forwarded => publish(&reorder),
    }
    // Ensure the final snapshot reflects steady-state counters even if the
    // last operation was a non-recording drain.
    publish(&reorder);
}

/// Outcome of one [`drain_and_record`] call.
enum DrainOutcome {
    /// No items were ready to drain.
    Empty,
    /// One or more items were forwarded to the consumer channel.
    Forwarded,
    /// The output channel is closed; the caller must abort.
    Disconnected,
}

/// Drains the contiguous in-order run from `reorder`, forwards each item to
/// `result_tx`, and records the batch size plus the wall-clock pause since
/// the previous non-empty drain.
///
/// Bypasses the metrics path on empty drains so the histograms reflect only
/// the work actually performed by the consumer thread.
///
/// When a throughput-governor `sink` is present, each non-empty drain also
/// publishes a [`Constraint::WireWrite`] sample: the bytes forwarded over the
/// drain's wall-clock span, tagged with the reorder buffer's residual
/// occupancy. Occupancy is the drum-detection signal the governor watches - a
/// persistently full reorder buffer means the consumer, not the workers, is
/// the constraint. Emission is pure observation and never changes what or when
/// items are forwarded.
fn drain_and_record(
    reorder: &mut ReorderBuffer<DeltaResult>,
    result_tx: &mpsc::Sender<DeltaResult>,
    last_drain_at: &mut Option<Instant>,
    sink: Option<&SampleSink>,
) -> DrainOutcome {
    let drain_start = Instant::now();
    let mut count = 0usize;
    let mut bytes = 0u64;
    while let Some(ready) = reorder.next_in_order() {
        bytes = bytes.saturating_add(ready.bytes_written());
        if result_tx.send(ready).is_err() {
            return DrainOutcome::Disconnected;
        }
        count += 1;
    }
    if count == 0 {
        return DrainOutcome::Empty;
    }
    let now = Instant::now();
    if let Some(prev) = *last_drain_at {
        reorder.record_drain_pause(now.saturating_duration_since(prev));
    }
    reorder.record_drain_batch(count);
    *last_drain_at = Some(now);
    if let Some(sink) = sink {
        sink.emit_stage(
            Constraint::WireWrite,
            bytes,
            now.saturating_duration_since(drain_start),
            reorder.buffered_count(),
        );
    }
    DrainOutcome::Forwarded
}
