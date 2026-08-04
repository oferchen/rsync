//! Drum-buffer-rope throughput governor: telemetry foundation (inert Observer).
//!
//! This module is the sensing half of a Theory-of-Constraints throughput
//! governor for the transfer pipeline. It composes existing building blocks -
//! the shared [`Ewma`](fast_io::ewma::Ewma) primitive and a lock-free
//! `crossbeam_queue::ArrayQueue` - into an Observer that watches every pipeline
//! stage without perturbing it.
//!
//! # What this step does (and does not) do
//!
//! The governor **senses and identifies the drum**: its dedicated thread folds
//! per-stage telemetry into EWMAs and runs a debounced [`DrumIdentifier`] over
//! them every window, publishing the current constraint stage through
//! [`GovernorHandle::drum`]. Identification is pure observation - it stores a
//! designation and drives no actuator on the data path - so enabling the
//! governor still cannot change a single byte on the wire for any protocol
//! version, a property pinned by the differential test in
//! `tests/throughput_governor_differential.rs`.
//!
//! The **rope** ([`Rope`]) is the first actuator: it sizes the concurrent-delta
//! work queue's [`AdaptiveSemaphore`](crate::concurrent_delta::work_queue::AdaptiveSemaphore)
//! ceiling to the drum's service time, memory-bounded and clamped to a
//! configured range. It is **opt-in**: a caller constructs it over a dynamic
//! queue and drives it from a governor handle. Nothing wires it into the default
//! transfer path, which keeps the static [`CapacitySource::Fixed`](crate::concurrent_delta::work_queue)
//! bound, so the default build stays byte-identical to the pre-governor code.
//! The remaining actuators (buffer-pool pressure, I/O depth) attach to the same
//! [`GovernorHandle`] facade in later steps.
//!
//! # Pattern composition
//!
//! - **Observer** - stages publish [`StageSample`]s through a cheap
//!   [`SampleSink`] onto the lock-free [`TelemetryBus`] (drop-on-overflow, so
//!   telemetry never backpressures the data path).
//! - **Mediator / Facade** - a single [`Governor`] owns interpretation; stages
//!   never tune each other. [`Governor::spawn`] returns the sole public handle.
//! - **State** - [`DrumIdentifier`] debounces the [`Constraint`] designation,
//!   committing a new drum only after several consecutive windows agree.
//! - **Adapter** - the [`Rope`] actuator adapts the drum signal to an admission
//!   ceiling; further platform I/O actuators attach through [`ActuatorHandle`]
//!   without leaking any `cfg` into this core.
//!
//! # Degradation ladder
//!
//! `OC_RSYNC_GOVERNOR=off` (the default for now) selects [`GovernorMode::Off`]:
//! no thread, and [`GovernorHandle::sample_sink`] returns `None` so every
//! instrumentation site compiles to a skipped branch. The pipeline is then
//! byte-identical to the pre-governor code. `OC_RSYNC_GOVERNOR=on` selects
//! [`GovernorMode::Observe`], which senses and identifies the drum but engages
//! no actuator unless one is explicitly attached.

mod actuator;
mod aggregate;
mod bus;
mod drum;
mod governor;
mod rope;
mod sample;

pub use actuator::ActuatorHandle;
pub use aggregate::{STAGE_ALPHA, StageAggregator};
pub use bus::{DEFAULT_BUS_CAPACITY, SampleSink, TelemetryBus, emit_if_enabled};
pub use drum::{
    DrumIdentifier, HIGH_OCCUPANCY, HYSTERESIS_WINDOWS, LOW_OCCUPANCY, StageSignal, StageSignals,
    classify,
};
pub use governor::{
    GOVERNOR_ENV, Governor, GovernorConfig, GovernorHandle, GovernorMode, POLL_INTERVAL,
};
pub use rope::{
    DEFAULT_MEM_BUDGET_BYTES, DEFAULT_SAFETY_FACTOR, Rope, RopeConfig, RopeConfigError,
    permit_weight_bytes,
};
pub use sample::{Constraint, StageSample};
