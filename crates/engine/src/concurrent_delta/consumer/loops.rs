//! Reorder loops driven by the delta-reorder background thread.
//!
//! Two variants exist: [`run_bare_loop`] for the in-memory [`ReorderBuffer`]
//! and [`run_spillable_loop`] for the bounded-memory
//! [`SpillableReorderBuffer`]. Both forward contiguous in-order runs to the
//! consumer channel and surface spill or I/O failures as a single failed
//! [`DeltaResult`] for the receiver to map to upstream exit code 11.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

use crate::concurrent_delta::reorder::{Metrics as ReorderMetrics, ReorderBuffer};
use crate::concurrent_delta::spill::{SpillError, SpillableReorderBuffer};
use crate::concurrent_delta::types::DeltaResult;

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
) {
    let mut last_drain_at: Option<Instant> = None;
    let publish = |reorder: &ReorderBuffer<DeltaResult>| {
        if let Ok(mut guard) = metrics.lock() {
            *guard = reorder.metrics();
        }
    };

    for result in stream_rx {
        while reorder.insert(result.sequence(), result.clone()).is_err() {
            match drain_and_record(&mut reorder, result_tx, &mut last_drain_at) {
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

        match drain_and_record(&mut reorder, result_tx, &mut last_drain_at) {
            DrainOutcome::Disconnected => return,
            DrainOutcome::Empty => {}
            DrainOutcome::Forwarded => publish(&reorder),
        }
    }

    match drain_and_record(&mut reorder, result_tx, &mut last_drain_at) {
        DrainOutcome::Disconnected => return,
        DrainOutcome::Empty => {}
        DrainOutcome::Forwarded => publish(&reorder),
    }
    // Ensure the final snapshot reflects steady-state counters even if the
    // last operation was a non-recording drain.
    publish(&reorder);
}

/// Reorder loop for the bounded-memory [`SpillableReorderBuffer`] backend.
///
/// The spill layer handles overflow internally: when the byte threshold is
/// exceeded, the buffer serialises the oldest (highest-sequence) items to a
/// tempfile and reloads them transparently on drain. The legacy
/// "force_insert as deadlock breaker" branch is gone - capacity exhaustion
/// becomes a spill rather than unbounded ring growth.
///
/// Spill-side I/O failures (ENOSPC, missing temp directory, encoder error)
/// are mapped to a [`DeltaResult::failed`] for the offending sequence, which
/// the receiver maps to upstream rsync exit code 11 (`FileIo`) so the
/// transfer aborts with the same semantics as a direct I/O failure.
pub(super) fn run_spillable_loop(
    stream_rx: crossbeam_channel::Receiver<DeltaResult>,
    result_tx: &mpsc::Sender<DeltaResult>,
    mut reorder: SpillableReorderBuffer<DeltaResult>,
    spill_events: &Arc<AtomicU64>,
    spill_activations: &Arc<AtomicU64>,
) {
    let mut prev_spill = reorder.spill_stats().spill_events;
    let mut prev_activations = reorder.spill_stats().spill_activations;

    for result in stream_rx {
        let ndx = result.ndx();
        loop {
            match reorder.insert(result.sequence(), result.clone()) {
                Ok(()) => break,
                Err(SpillError::Capacity(_)) => {
                    // Drain ready items first to free a ring slot.
                    let mut drained_any = false;
                    match reorder.drain_ready() {
                        Ok(items) => {
                            for ready in items {
                                drained_any = true;
                                if result_tx.send(ready).is_err() {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            let _ = result_tx.send(DeltaResult::failed(
                                ndx,
                                format!("spill reload failed: {e}"),
                            ));
                            return;
                        }
                    }
                    if !drained_any {
                        // The head is missing and the ring is full. Force the
                        // insert; the spill layer keeps memory bounded by
                        // displacing higher-sequence items to disk.
                        if let Err(e) = reorder.force_insert(result.sequence(), result.clone()) {
                            let _ = result_tx.send(DeltaResult::failed(
                                ndx,
                                format!("spill force_insert failed: {e}"),
                            ));
                            return;
                        }
                        publish_spill_events(&reorder, spill_events, &mut prev_spill);
                        publish_spill_activations(
                            &reorder,
                            spill_activations,
                            &mut prev_activations,
                        );
                        break;
                    }
                }
                Err(SpillError::Io(e)) => {
                    let _ = result_tx
                        .send(DeltaResult::failed(ndx, format!("spill write failed: {e}")));
                    return;
                }
                Err(SpillError::UnsupportedCompression(tag)) => {
                    let _ = result_tx.send(DeltaResult::failed(
                        ndx,
                        format!("spill record uses unsupported compression tag 0x{tag:02x}"),
                    ));
                    return;
                }
                Err(e @ SpillError::PriorSpillsLost { .. }) => {
                    let _ = result_tx
                        .send(DeltaResult::failed(ndx, format!("spill write failed: {e}")));
                    return;
                }
                Err(SpillError::SpillDisabled) => {
                    let _ = result_tx.send(DeltaResult::failed(
                        ndx,
                        "reorder buffer exceeded capacity and spill-to-disk is disabled"
                            .to_string(),
                    ));
                    return;
                }
            }
        }
        publish_spill_events(&reorder, spill_events, &mut prev_spill);
        publish_spill_activations(&reorder, spill_activations, &mut prev_activations);

        match reorder.drain_ready() {
            Ok(items) => {
                for ready in items {
                    if result_tx.send(ready).is_err() {
                        return;
                    }
                }
            }
            Err(e) => {
                let _ = result_tx.send(DeltaResult::failed(
                    ndx,
                    format!("spill reload failed: {e}"),
                ));
                return;
            }
        }
    }

    // Stream closed - drain whatever is left, including spilled entries.
    loop {
        match reorder.drain_ready() {
            Ok(items) if items.is_empty() => break,
            Ok(items) => {
                for ready in items {
                    if result_tx.send(ready).is_err() {
                        return;
                    }
                }
            }
            Err(e) => {
                let _ = result_tx.send(DeltaResult::failed(
                    0u32,
                    format!("spill reload failed: {e}"),
                ));
                return;
            }
        }
    }
    publish_spill_events(&reorder, spill_events, &mut prev_spill);
    publish_spill_activations(&reorder, spill_activations, &mut prev_activations);
}

/// Republishes the cumulative spill-to-disk event counter so callers can
/// observe progress via [`crate::concurrent_delta::consumer::DeltaConsumer::stats`].
fn publish_spill_events(
    reorder: &SpillableReorderBuffer<DeltaResult>,
    spill_events: &Arc<AtomicU64>,
    prev: &mut u64,
) {
    let current = reorder.spill_stats().spill_events;
    if current != *prev {
        spill_events.store(current, Ordering::Relaxed);
        *prev = current;
    }
}

/// Republishes the cumulative `spill_excess` activation counter so callers
/// can observe normal-operation spill pressure via
/// [`crate::concurrent_delta::consumer::DeltaConsumer::stats`] (ROB-2).
///
/// `spill_activations` is granularity-invariant: one increment per
/// `spill_excess` call that wrote at least one record, regardless of
/// whether the buffer is configured for `PerItem` or `WholeBatch`.
fn publish_spill_activations(
    reorder: &SpillableReorderBuffer<DeltaResult>,
    spill_activations: &Arc<AtomicU64>,
    prev: &mut u64,
) {
    let current = reorder.spill_stats().spill_activations;
    if current != *prev {
        spill_activations.store(current, Ordering::Relaxed);
        *prev = current;
    }
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
fn drain_and_record(
    reorder: &mut ReorderBuffer<DeltaResult>,
    result_tx: &mpsc::Sender<DeltaResult>,
    last_drain_at: &mut Option<Instant>,
) -> DrainOutcome {
    let mut count = 0usize;
    while let Some(ready) = reorder.next_in_order() {
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
    DrainOutcome::Forwarded
}
