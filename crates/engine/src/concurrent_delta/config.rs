//! Runtime configuration for the concurrent delta pipeline.
//!
//! [`ConcurrentDeltaConfig`] aggregates the knobs that tune the
//! [`DeltaConsumer`](super::consumer::DeltaConsumer) without changing its
//! public type signature. Today it carries one knob, the optional
//! throughput-governor [`telemetry_sink`](ConcurrentDeltaConfig::telemetry_sink).
//!
//! # Defaults
//!
//! [`ConcurrentDeltaConfig::default`] attaches no telemetry sink, so the
//! consumer behaves exactly like [`DeltaConsumer::spawn`](super::consumer::DeltaConsumer::spawn):
//! a bare in-memory [`ReorderBuffer`](super::reorder::ReorderBuffer) and no
//! sample emission.

use crate::throughput::SampleSink;

/// Tunable runtime knobs for the concurrent delta pipeline.
///
/// Pass an instance to [`DeltaConsumer::spawn_with_config`](super::consumer::DeltaConsumer::spawn_with_config).
/// The default value adds no extra plumbing.
#[derive(Debug, Clone, Default)]
pub struct ConcurrentDeltaConfig {
    /// Optional throughput-governor telemetry sink.
    ///
    /// `None` (the default) leaves the pipeline byte-identical to the
    /// pre-governor code: the compute workers and reorder drain skip sample
    /// emission entirely. When `Some`, each instrumented stage publishes a
    /// [`StageSample`](crate::throughput::StageSample) per unit of work through
    /// this cheap, clonable handle. Emission only observes; it never alters
    /// control flow, ordering, or output.
    pub telemetry_sink: Option<SampleSink>,
}

impl ConcurrentDeltaConfig {
    /// Attaches a throughput-governor telemetry sink.
    ///
    /// With a sink present the compute workers and reorder drain publish
    /// [`StageSample`](crate::throughput::StageSample)s; without one they emit
    /// nothing. The sink only observes, so enabling it never changes transfer
    /// output.
    #[must_use]
    pub fn with_telemetry_sink(mut self, sink: SampleSink) -> Self {
        self.telemetry_sink = Some(sink);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::throughput::TelemetryBus;

    #[test]
    fn default_attaches_no_telemetry_sink() {
        assert!(ConcurrentDeltaConfig::default().telemetry_sink.is_none());
    }

    #[test]
    fn with_telemetry_sink_attaches_the_sink() {
        let sink = SampleSink::new(TelemetryBus::new(4));
        let cfg = ConcurrentDeltaConfig::default().with_telemetry_sink(sink);
        assert!(cfg.telemetry_sink.is_some());
    }
}
