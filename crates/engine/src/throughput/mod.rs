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
//! In this step the governor is **inert**: it senses and nothing more. It has
//! no control loop that acts, no buffer sizing, no backpressure "rope", and no
//! I/O actuation. Enabling it therefore cannot change a single byte on the wire
//! for any protocol version - a property pinned by the differential test in
//! `tests/throughput_governor_differential.rs`. The actuators (dynamic capacity,
//! rope, buffer-pool pressure, I/O depth) attach to the same
//! [`GovernorHandle`] facade in later steps.
//!
//! # Pattern composition
//!
//! - **Observer** - stages publish [`StageSample`]s through a cheap
//!   [`SampleSink`] onto the lock-free [`TelemetryBus`] (drop-on-overflow, so
//!   telemetry never backpressures the data path).
//! - **Mediator / Facade** - a single [`Governor`] owns interpretation; stages
//!   never tune each other. [`Governor::spawn`] returns the sole public handle.
//! - **State** - the [`Constraint`] taxonomy names the stage that can become the
//!   drum.
//! - **Adapter (later)** - platform I/O actuators will adapt to governor signals
//!   through [`ActuatorHandle`] without leaking any `cfg` into this core.
//!
//! # Degradation ladder
//!
//! `OC_RSYNC_GOVERNOR=off` (the default for now) selects [`GovernorMode::Off`]:
//! no thread, and [`GovernorHandle::sample_sink`] returns `None` so every
//! instrumentation site compiles to a skipped branch. The pipeline is then
//! byte-identical to the pre-governor code. `OC_RSYNC_GOVERNOR=on` selects
//! [`GovernorMode::Observe`], which senses only.

mod actuator;
mod aggregate;
mod bus;
mod governor;
mod sample;

pub use actuator::ActuatorHandle;
pub use aggregate::{STAGE_ALPHA, StageAggregator};
pub use bus::{DEFAULT_BUS_CAPACITY, SampleSink, TelemetryBus, emit_if_enabled};
pub use governor::{
    GOVERNOR_ENV, Governor, GovernorConfig, GovernorHandle, GovernorMode, POLL_INTERVAL,
};
pub use sample::{Constraint, StageSample};
