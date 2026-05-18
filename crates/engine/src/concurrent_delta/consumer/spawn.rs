//! Background-thread spawn machinery for [`DeltaConsumer`].
//!
//! Defines the private [`ReorderMode`] selector consumed by [`spawn_inner`],
//! the helper that constructs a [`SpillableReorderBuffer`], and the
//! cross-thread plumbing that wires the drain and reorder threads to the
//! public [`DeltaConsumer`] handle.

use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use super::DeltaConsumer;
use super::loops::{run_bare_loop, run_spillable_loop};
use crate::concurrent_delta::config::ConcurrentDeltaConfig;
use crate::concurrent_delta::reorder::{Metrics as ReorderMetrics, ReorderBuffer};
use crate::concurrent_delta::spill::{self, SpillableReorderBuffer};
use crate::concurrent_delta::strategy;
use crate::concurrent_delta::types::DeltaResult;
use crate::concurrent_delta::work_queue::WorkQueueReceiver;

/// Selects the reorder backend driven by [`spawn_inner`].
///
/// Encoded as an enum rather than two booleans so future modes (e.g.
/// hybrid memory + spill with adaptive sizing) can extend the variant set
/// without churning the call sites.
pub(super) enum ReorderMode {
    /// Bypass mode: passthrough FIFO, no sequence reordering.
    Bypass,
    /// Bare in-memory ring with the historical doubling fallback on overflow.
    Bare { capacity: usize },
    /// Bounded-memory ring with spill-to-tempfile when the byte threshold is
    /// exceeded. `dir` is `None` for the default `SpooledTempFile` backend.
    /// `memory_pressure_bytes` is `Some` when the RSS-aware spill trigger is
    /// engaged (see [`SpillPolicy::memory_pressure_bytes`](crate::concurrent_delta::spill::SpillPolicy::memory_pressure_bytes)).
    Spillable {
        capacity: usize,
        threshold: usize,
        dir: Option<PathBuf>,
        granularity: spill::SpillGranularity,
        memory_pressure_bytes: Option<u64>,
    },
}

impl ReorderMode {
    /// Maps a [`ConcurrentDeltaConfig`] to the matching reorder backend.
    pub(super) fn from_config(reorder_capacity: usize, mut cfg: ConcurrentDeltaConfig) -> Self {
        // STN-8/9/10: layer OC_RSYNC_SPILL_* env overrides on top of the
        // caller-supplied policy so operators can tune the spill backend
        // without recompiling. Absent vars leave fields unchanged.
        spill::apply_env_overrides(&mut cfg.spill_policy);
        match cfg.spill_policy.threshold_bytes {
            Some(threshold) => ReorderMode::Spillable {
                capacity: reorder_capacity,
                threshold: usize::try_from(threshold).unwrap_or(usize::MAX),
                dir: cfg.spill_policy.dir,
                granularity: cfg.spill_policy.granularity,
                memory_pressure_bytes: cfg.spill_policy.memory_pressure_bytes,
            },
            None => ReorderMode::Bare {
                capacity: reorder_capacity,
            },
        }
    }
}

/// Spawns the two background threads and returns the assembled
/// [`DeltaConsumer`] handle. Shared by all public factory methods.
pub(super) fn spawn_inner(rx: WorkQueueReceiver, mode: ReorderMode) -> DeltaConsumer {
    let (result_tx, result_rx) = mpsc::channel();
    let spill_events = Arc::new(AtomicU64::new(0));

    // Bounded channel between drain and reorder threads. Capacity matches
    // reorder buffer so workers can stay ahead without unbounded buffering.
    let stream_capacity = match &mode {
        ReorderMode::Bypass => rayon::current_num_threads() * 2,
        ReorderMode::Bare { capacity } | ReorderMode::Spillable { capacity, .. } => {
            (*capacity).max(rayon::current_num_threads() * 2)
        }
    };
    let (stream_tx, stream_rx) = crossbeam_channel::bounded::<DeltaResult>(stream_capacity);

    // Thread 1: runs rayon::scope, streaming results as workers complete.
    let drain_handle = thread::Builder::new()
        .name("delta-drain".to_string())
        .spawn(move || {
            rx.drain_parallel_into(|work| strategy::dispatch(&work), stream_tx);
        })
        .expect("failed to spawn delta-drain thread");

    // Shared metrics snapshot updated by the reorder thread; callers can
    // poll it through `DeltaConsumer::metrics` without blocking the
    // delta pipeline.
    let metrics = Arc::new(Mutex::new(ReorderMetrics::default()));
    let metrics_thread = Arc::clone(&metrics);

    // Thread 2: receives streamed results, reorders (or passes through),
    // and forwards to the consumer channel.
    let spill_events_thread = Arc::clone(&spill_events);
    let handle = thread::Builder::new()
        .name("delta-reorder".to_string())
        .spawn(move || {
            match mode {
                ReorderMode::Bypass => {
                    run_bare_loop(
                        stream_rx,
                        &result_tx,
                        ReorderBuffer::passthrough(),
                        &metrics_thread,
                    );
                }
                ReorderMode::Bare { capacity } => {
                    run_bare_loop(
                        stream_rx,
                        &result_tx,
                        ReorderBuffer::new(capacity),
                        &metrics_thread,
                    );
                }
                ReorderMode::Spillable {
                    capacity,
                    threshold,
                    dir,
                    granularity,
                    memory_pressure_bytes,
                } => match build_spillable(
                    capacity,
                    threshold,
                    dir,
                    granularity,
                    memory_pressure_bytes,
                ) {
                    Ok(buf) => {
                        run_spillable_loop(stream_rx, &result_tx, buf, &spill_events_thread);
                    }
                    Err(e) => {
                        // Construction failed (e.g., spill dir cannot be
                        // created). Surface as a single failed result so
                        // the receiver maps to exit code 11 and aborts.
                        let _ = result_tx.send(DeltaResult::failed(
                            0u32,
                            format!("spill backend unavailable: {e}"),
                        ));
                    }
                },
            }

            // Wait for drain thread to finish (propagates panics).
            let _ = drain_handle.join();
        })
        .expect("failed to spawn delta-reorder thread");

    DeltaConsumer {
        result_rx,
        handle: Some(handle),
        metrics,
        spill_events,
    }
}

/// Constructs a [`SpillableReorderBuffer`] backed by the configured backend.
fn build_spillable(
    capacity: usize,
    threshold: usize,
    dir: Option<PathBuf>,
    granularity: spill::SpillGranularity,
    memory_pressure_bytes: Option<u64>,
) -> std::io::Result<SpillableReorderBuffer<DeltaResult>> {
    let buf = match dir {
        Some(d) => SpillableReorderBuffer::with_spill_dir(capacity, threshold, d)?,
        None => SpillableReorderBuffer::new(capacity, threshold),
    };
    Ok(buf
        .with_granularity(granularity)
        .with_memory_pressure_bytes(memory_pressure_bytes))
}
