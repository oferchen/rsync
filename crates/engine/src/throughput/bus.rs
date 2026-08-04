//! Lock-free telemetry bus connecting instrumented stages to the governor.
//!
//! The Observer pattern's transport: stages publish [`StageSample`]s through a
//! cheap [`SampleSink`] handle onto a bounded, lock-free
//! [`crossbeam_queue::ArrayQueue`]; the governor thread drains them on its own
//! cadence. The bus is **drop-on-overflow**: if the governor cannot keep up,
//! excess samples are dropped and counted rather than blocking a producer.
//! Telemetry must never backpressure the data path - a slow or stalled
//! governor can only cost accuracy, never throughput.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crossbeam_queue::ArrayQueue;

use super::sample::{Constraint, StageSample};

/// Capacity of the telemetry ring, in samples.
///
/// Sized to buffer a burst of stage completions across one governor poll
/// window without dropping under normal load. At the pipeline's worker fan-out
/// (`2 * num_threads` in-flight items) even a 64-thread host completes far
/// fewer than 4096 files per 250 ms poll, so the steady state never overflows;
/// the reserve absorbs scheduling jitter. Each [`StageSample`] is a few machine
/// words, so 4096 slots cost on the order of 128 KiB - negligible against the
/// transfer's buffer pool. A power of two keeps `ArrayQueue`'s index masking
/// branch-free.
pub const DEFAULT_BUS_CAPACITY: usize = 4096;

/// Shared, lock-free ring that carries samples from stages to the governor.
///
/// Producers ([`SampleSink`]) push; the governor pops. The queue is
/// multi-producer/multi-consumer, so any number of worker threads may publish
/// concurrently with the single governor drain. Overflowed pushes increment
/// [`TelemetryBus::dropped`] instead of blocking.
#[derive(Debug)]
pub struct TelemetryBus {
    queue: ArrayQueue<StageSample>,
    dropped: AtomicU64,
}

impl TelemetryBus {
    /// Creates a bus with room for `capacity` in-flight samples.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero - a zero-length ring could never hold a
    /// sample and would drop every observation, which is a configuration
    /// error rather than a runtime condition.
    #[must_use]
    pub fn new(capacity: usize) -> Arc<Self> {
        assert!(capacity > 0, "telemetry bus capacity must be non-zero");
        Arc::new(Self {
            queue: ArrayQueue::new(capacity),
            dropped: AtomicU64::new(0),
        })
    }

    /// Publishes `sample`, dropping and counting it if the ring is full.
    ///
    /// Returns `true` when the sample was enqueued and `false` when it was
    /// dropped. This is the single hot-path operation on the data-path side:
    /// one lock-free push, and on overflow one relaxed increment.
    #[inline]
    pub fn push(&self, sample: StageSample) -> bool {
        match self.queue.push(sample) {
            Ok(()) => true,
            Err(_) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Pops the next queued sample, or `None` when the ring is empty.
    ///
    /// Called only by the governor drain thread.
    #[must_use]
    pub fn pop(&self) -> Option<StageSample> {
        self.queue.pop()
    }

    /// Total samples dropped due to ring overflow since construction.
    ///
    /// A non-zero value means the governor fell behind the data path; the
    /// transfer itself was unaffected because pushes never block.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Current number of buffered samples awaiting the governor drain.
    #[must_use]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Whether the ring currently holds no samples.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

/// A cheap, clonable producer handle stages hold to publish telemetry.
///
/// Cloning is an `Arc` bump; emitting is one [`TelemetryBus::push`]. Stages
/// keep an `Option<SampleSink>` and skip emission entirely when the governor
/// is off, so the instrumented path costs nothing beyond a null check in the
/// disabled configuration.
#[derive(Debug, Clone)]
pub struct SampleSink {
    bus: Arc<TelemetryBus>,
}

impl SampleSink {
    /// Wraps a shared bus in a producer handle.
    #[must_use]
    pub fn new(bus: Arc<TelemetryBus>) -> Self {
        Self { bus }
    }

    /// Publishes `sample`; drops and counts it on overflow.
    ///
    /// Returns `true` if enqueued, mirroring [`TelemetryBus::push`]. Callers on
    /// the data path ignore the return - a dropped sample is by design not an
    /// error.
    #[inline]
    pub fn emit(&self, sample: StageSample) -> bool {
        self.bus.push(sample)
    }

    /// Convenience wrapper that builds and emits a [`StageSample`] in one call.
    ///
    /// Equivalent to `self.emit(StageSample::new(stage, bytes, duration,
    /// queue_occupancy))`; keeps instrumentation sites to a single expression.
    #[inline]
    pub fn emit_stage(
        &self,
        stage: Constraint,
        bytes: u64,
        duration: Duration,
        queue_occupancy: usize,
    ) -> bool {
        self.emit(StageSample::new(stage, bytes, duration, queue_occupancy))
    }
}

/// Emits a sample through an optional sink, doing nothing when `sink` is
/// `None`.
///
/// The single choke point every instrumentation site funnels through: with the
/// governor off, callers hold `None` and this compiles to a branch that skips
/// the push, guaranteeing byte-identical, cost-free behaviour on the disabled
/// path.
#[inline]
pub fn emit_if_enabled(
    sink: Option<&SampleSink>,
    stage: Constraint,
    bytes: u64,
    duration: Duration,
    queue_occupancy: usize,
) {
    if let Some(sink) = sink {
        sink.emit_stage(stage, bytes, duration, queue_occupancy);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(bytes: u64) -> StageSample {
        StageSample::new(Constraint::Compute, bytes, Duration::from_millis(1), 0)
    }

    #[test]
    fn push_then_pop_roundtrips_fifo() {
        let bus = TelemetryBus::new(4);
        assert!(bus.push(sample(1)));
        assert!(bus.push(sample(2)));
        assert_eq!(bus.len(), 2);
        assert_eq!(bus.pop().map(|s| s.bytes), Some(1));
        assert_eq!(bus.pop().map(|s| s.bytes), Some(2));
        assert!(bus.pop().is_none());
        assert!(bus.is_empty());
    }

    #[test]
    fn overflow_drops_are_counted_not_blocked() {
        let bus = TelemetryBus::new(2);
        assert!(bus.push(sample(1)));
        assert!(bus.push(sample(2)));
        // Ring is full: further pushes drop and count.
        assert!(!bus.push(sample(3)));
        assert!(!bus.push(sample(4)));
        assert_eq!(bus.dropped(), 2);
        // Draining one slot frees room for a subsequent push.
        assert_eq!(bus.pop().map(|s| s.bytes), Some(1));
        assert!(bus.push(sample(5)));
        assert_eq!(bus.dropped(), 2, "successful push must not count as a drop");
    }

    #[test]
    fn sink_is_a_cheap_clone_over_shared_bus() {
        let bus = TelemetryBus::new(8);
        let sink_a = SampleSink::new(Arc::clone(&bus));
        let sink_b = sink_a.clone();
        sink_a.emit_stage(Constraint::Read, 10, Duration::from_millis(1), 3);
        sink_b.emit_stage(Constraint::WireWrite, 20, Duration::from_millis(1), 0);
        assert_eq!(bus.len(), 2, "both clones publish to the same ring");
    }

    #[test]
    fn emit_if_enabled_is_noop_when_disabled() {
        let bus = TelemetryBus::new(4);
        let sink = SampleSink::new(Arc::clone(&bus));
        emit_if_enabled(None, Constraint::Read, 100, Duration::from_millis(1), 0);
        assert!(bus.is_empty(), "None sink must not publish");
        emit_if_enabled(
            Some(&sink),
            Constraint::Read,
            100,
            Duration::from_millis(1),
            0,
        );
        assert_eq!(bus.len(), 1, "Some sink publishes exactly once");
    }

    #[test]
    #[should_panic(expected = "capacity must be non-zero")]
    fn zero_capacity_panics() {
        let _ = TelemetryBus::new(0);
    }
}
