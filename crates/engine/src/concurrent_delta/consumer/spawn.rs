//! Background-thread spawn machinery for [`DeltaConsumer`].
//!
//! Defines the private [`ReorderMode`] selector consumed by [`spawn_inner`]
//! and the cross-thread plumbing that wires the drain and reorder threads to
//! the public [`DeltaConsumer`] handle.

use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use super::DeltaConsumer;
use super::loops::run_bare_loop;
use crate::concurrent_delta::reorder::{Metrics as ReorderMetrics, ReorderBuffer};
use crate::concurrent_delta::strategy;
use crate::concurrent_delta::types::{DeltaResult, DeltaWork};
use crate::concurrent_delta::work_queue::WorkQueueReceiver;
use crate::throughput::{Constraint, SampleSink};

/// Selects the reorder backend driven by [`spawn_inner`].
pub(super) enum ReorderMode {
    /// Bypass mode: passthrough FIFO, no sequence reordering.
    Bypass,
    /// Bare in-memory ring with the historical doubling fallback on overflow.
    Bare { capacity: usize },
}

/// Spawns the two background threads and returns the assembled
/// [`DeltaConsumer`] handle. Shared by all public factory methods.
///
/// `sink` is the optional throughput-governor telemetry handle. When `None`
/// (the default, and always so when the governor is off) neither the compute
/// workers nor the reorder drain emit samples, so the pipeline runs exactly as
/// it did before instrumentation. When `Some`, each compute worker publishes a
/// [`Constraint::Compute`] sample and the reorder drain publishes
/// [`Constraint::WireWrite`] samples carrying the live reorder-buffer
/// occupancy. Emission only observes; it never affects ordering or output.
pub(super) fn spawn_inner(
    rx: WorkQueueReceiver,
    mode: ReorderMode,
    sink: Option<SampleSink>,
) -> DeltaConsumer {
    let (result_tx, result_rx) = mpsc::channel();

    // Bounded channel between drain and reorder threads. Capacity matches
    // reorder buffer so workers can stay ahead without unbounded buffering.
    let (stream_capacity, reorder) = match mode {
        ReorderMode::Bypass => (
            rayon::current_num_threads() * 2,
            ReorderBuffer::passthrough(),
        ),
        ReorderMode::Bare { capacity } => (
            capacity.max(rayon::current_num_threads() * 2),
            ReorderBuffer::new(capacity),
        ),
    };
    let (stream_tx, stream_rx) = crossbeam_channel::bounded::<DeltaResult>(stream_capacity);

    // Thread 1: runs rayon::scope, streaming results as workers complete.
    // Each worker times its own delta computation and, when telemetry is on,
    // publishes a Compute sample - the delta-computation stage boundary.
    let worker_sink = sink.clone();
    let drain_handle = thread::Builder::new()
        .name("delta-drain".to_string())
        .spawn(move || {
            rx.drain_parallel_into(
                move |work| dispatch_with_telemetry(worker_sink.as_ref(), work),
                stream_tx,
            );
        })
        .expect("failed to spawn delta-drain thread");

    // Shared metrics snapshot updated by the reorder thread; callers can
    // poll it through `DeltaConsumer::metrics` without blocking the
    // delta pipeline.
    let metrics = Arc::new(Mutex::new(ReorderMetrics::default()));
    let metrics_thread = Arc::clone(&metrics);

    // Read the force_insert counter before the buffer moves into the thread
    // so it is observable from the parent before delivery starts.
    let force_inserts = reorder.force_insert_counter();

    // Thread 2: receives streamed results, reorders (or passes through),
    // and forwards to the consumer channel.
    let handle = thread::Builder::new()
        .name("delta-reorder".to_string())
        .spawn(move || {
            run_bare_loop(
                stream_rx,
                &result_tx,
                reorder,
                &metrics_thread,
                sink.as_ref(),
            );

            // Wait for drain thread to finish (propagates panics).
            let _ = drain_handle.join();
        })
        .expect("failed to spawn delta-reorder thread");

    DeltaConsumer {
        result_rx,
        handle: Some(handle),
        metrics,
        force_inserts,
    }
}

/// Dispatches one work item and, when telemetry is enabled, publishes a
/// [`Constraint::Compute`] sample timing the delta computation.
///
/// With `sink` `None` this is exactly `strategy::dispatch(&work)` - no timing,
/// no branch beyond the null check - so the disabled path is cost-free and
/// byte-identical. The sample reports the result's `bytes_written` as the work
/// completed over the measured wall-clock span; `queue_occupancy` is `0`
/// because per-file compute has no explicit input queue to sample here.
fn dispatch_with_telemetry(sink: Option<&SampleSink>, work: DeltaWork) -> DeltaResult {
    match sink {
        None => strategy::dispatch(&work),
        Some(sink) => {
            let start = std::time::Instant::now();
            let result = strategy::dispatch(&work);
            sink.emit_stage(
                Constraint::Compute,
                result.bytes_written(),
                start.elapsed(),
                0,
            );
            result
        }
    }
}
