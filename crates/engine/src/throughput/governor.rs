//! The throughput [`Governor`] facade: spawn, sense, shut down.
//!
//! This is the Mediator/Facade of the drum-buffer-rope design. Stages never
//! tune each other; they publish telemetry through a [`SampleSink`] and the
//! single governor owns interpretation. In this inert (G2) step the governor
//! *only* senses: its dedicated thread drains the telemetry bus and folds
//! samples into per-stage EWMAs. It takes **no** control action - no buffer
//! sizing, no rope, no I/O actuation - so enabling it cannot change a single
//! byte on the wire. Those actuators arrive in later steps behind the same
//! facade.
//!
//! # Threading model
//!
//! The governor runs on one dedicated [`std::thread`] - not tokio, not rayon -
//! because it must observe both the rayon compute pool and the tokio network
//! edge without living inside either. Shutdown is cooperative: an atomic flag
//! plus a wake channel let [`GovernorHandle::shutdown`] (and `Drop`) join the
//! thread promptly on every exit path, including panic unwinding of unrelated
//! worker threads.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

use super::actuator::{ActuatorHandle, BufferCeiling};
use super::aggregate::StageAggregator;
use super::bus::{DEFAULT_BUS_CAPACITY, SampleSink, TelemetryBus};
use super::drum::DrumIdentifier;
use super::sample::Constraint;

/// Governor operating mode selected at spawn time.
///
/// G2 ships only the inert modes. `Off` is the pre-change static behaviour;
/// `Observe` senses without acting. Active control modes are added behind this
/// same enum in later steps, so callers that match on it must remain
/// exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GovernorMode {
    /// No governor: no thread, no telemetry, byte-identical to the static path.
    Off,
    /// Sense only: drain telemetry and maintain per-stage EWMAs, take no action.
    Observe,
}

impl GovernorMode {
    /// Parses the `OC_RSYNC_GOVERNOR` operator knob.
    ///
    /// Recognises `on` / `observe` / `1` / `true` (case-insensitive) as
    /// [`GovernorMode::Observe`]; every other value, including `off`, `0`, the
    /// empty string, and an absent variable, is [`GovernorMode::Off`]. The
    /// governor is default-off for now, mirroring the opt-out precedent of
    /// `OC_RSYNC_ADAPTIVE_BASIS_DISPATCH`.
    #[must_use]
    pub fn from_env_value(value: Option<&str>) -> Self {
        match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("on" | "observe" | "1" | "true") => GovernorMode::Observe,
            _ => GovernorMode::Off,
        }
    }
}

/// Environment variable selecting the governor mode.
pub const GOVERNOR_ENV: &str = "OC_RSYNC_GOVERNOR";

/// Governor poll window.
///
/// The sense loop drains the telemetry bus and refreshes the per-stage EWMAs
/// every 250 ms. The value matches the DBR design's evaluation window: long
/// enough that a poll folds many per-file samples into a stable estimate and
/// short enough to notice a genuine constraint shift within a second. Draining
/// is O(samples queued), so the window bounds governor CPU to a negligible
/// slice regardless of transfer size.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Immutable configuration for [`Governor::spawn`].
#[derive(Debug, Clone)]
pub struct GovernorConfig {
    /// Operating mode.
    pub mode: GovernorMode,
    /// Telemetry ring capacity in samples.
    pub bus_capacity: usize,
    /// Sense-loop poll window.
    pub poll_interval: Duration,
}

impl GovernorConfig {
    /// Builds a config for `mode` with the default bus capacity and poll window.
    #[must_use]
    pub fn new(mode: GovernorMode) -> Self {
        Self {
            mode,
            bus_capacity: DEFAULT_BUS_CAPACITY,
            poll_interval: POLL_INTERVAL,
        }
    }

    /// Builds a config from the `OC_RSYNC_GOVERNOR` environment variable.
    #[must_use]
    pub fn from_env() -> Self {
        let mode = GovernorMode::from_env_value(std::env::var(GOVERNOR_ENV).ok().as_deref());
        Self::new(mode)
    }
}

impl Default for GovernorConfig {
    /// The default configuration is [`GovernorMode::Off`] - the static,
    /// byte-identical pipeline.
    fn default() -> Self {
        Self::new(GovernorMode::Off)
    }
}

/// Sentinel bit pattern for "no throughput estimate yet".
///
/// `u64::MAX` is a quiet-NaN bit pattern that [`f64::to_bits`] never produces
/// for a finite, non-negative rate, so it can flag an unset slot without
/// colliding with any real value - including a genuine `0.0` stall.
const RATE_UNSET: u64 = u64::MAX;

/// Lock-free snapshot of per-stage throughput published by the governor thread.
///
/// The governor writes each stage's latest smoothed bytes/second here after
/// every fold; readers (tests, and future actuators) load without blocking the
/// sense loop. Each slot stores an `f64` rate via its bit pattern, or
/// [`RATE_UNSET`] before the first usable sample for that stage.
#[derive(Debug)]
struct Snapshot {
    rates: [AtomicU64; Constraint::COUNT],
    /// Latest committed drum designation, stored as a [`Constraint::index`].
    drum: AtomicU8,
}

impl Snapshot {
    fn new() -> Self {
        Self {
            rates: std::array::from_fn(|_| AtomicU64::new(RATE_UNSET)),
            drum: AtomicU8::new(Constraint::Unknown.index() as u8),
        }
    }

    fn store(&self, stage: Constraint, rate: f64) {
        self.rates[stage.index()].store(rate.to_bits(), Ordering::Relaxed);
    }

    fn load(&self, stage: Constraint) -> Option<f64> {
        match self.rates[stage.index()].load(Ordering::Relaxed) {
            RATE_UNSET => None,
            bits => Some(f64::from_bits(bits)),
        }
    }

    fn store_drum(&self, drum: Constraint) {
        self.drum.store(drum.index() as u8, Ordering::Relaxed);
    }

    fn load_drum(&self) -> Constraint {
        Constraint::from_index(self.drum.load(Ordering::Relaxed) as usize)
    }
}

/// Spawns and owns the throughput governor.
///
/// [`Governor::spawn`] is the sole constructor; it returns a
/// [`GovernorHandle`]. The `Governor` type itself is a namespace for the
/// constructor and carries no state - all state lives behind the handle so the
/// facade stays small.
#[derive(Debug)]
pub struct Governor;

impl Governor {
    /// Spawns a governor for `config` and returns its handle.
    ///
    /// When `config.mode` is [`GovernorMode::Off`] no thread is spawned and the
    /// returned handle is inert: [`GovernorHandle::sample_sink`] yields `None`,
    /// so instrumented stages skip emission and the pipeline behaves exactly as
    /// it did before the governor existed. When `Observe`, one dedicated thread
    /// starts draining the telemetry bus.
    #[must_use]
    pub fn spawn(config: GovernorConfig) -> GovernorHandle {
        let bus = TelemetryBus::new(config.bus_capacity);
        let snapshot = Arc::new(Snapshot::new());
        let buffer_ceiling = BufferCeiling::new();

        match config.mode {
            GovernorMode::Off => GovernorHandle {
                mode: GovernorMode::Off,
                bus,
                snapshot,
                buffer_ceiling,
                shutdown: Arc::new(AtomicBool::new(false)),
                wake: None,
                thread: None,
            },
            GovernorMode::Observe => {
                let shutdown = Arc::new(AtomicBool::new(false));
                let (wake_tx, wake_rx) = mpsc::channel::<()>();
                let poll = config.poll_interval;
                let bus_thread = Arc::clone(&bus);
                let snapshot_thread = Arc::clone(&snapshot);
                let shutdown_thread = Arc::clone(&shutdown);
                let thread = std::thread::Builder::new()
                    .name("throughput-governor".to_string())
                    .spawn(move || {
                        sense_loop(
                            &bus_thread,
                            &snapshot_thread,
                            &shutdown_thread,
                            &wake_rx,
                            poll,
                        );
                    })
                    .expect("failed to spawn throughput-governor thread");
                GovernorHandle {
                    mode: GovernorMode::Observe,
                    bus,
                    snapshot,
                    buffer_ceiling,
                    shutdown,
                    wake: Some(wake_tx),
                    thread: Some(thread),
                }
            }
        }
    }
}

/// The governor sense loop: drain, fold, identify the drum, publish, wait,
/// repeat.
///
/// Runs until `shutdown` is set (and a final drain has folded any stragglers).
/// Each iteration drains every queued sample - keeping the bus shallow so
/// producers rarely overflow it - folds each into its stage average, publishes
/// the refreshed rate to `snapshot`, then runs one drum-identification window
/// over the folded signals and publishes the debounced drum. It waits up to
/// `poll` for the next window or an early wake from [`GovernorHandle::shutdown`].
///
/// Drum identification is pure observation: it reads the aggregated telemetry
/// and stores a designation for callers to read. In this step it drives no
/// actuator, so the sensing remains byte-neutral on the data path.
fn sense_loop(
    bus: &TelemetryBus,
    snapshot: &Snapshot,
    shutdown: &AtomicBool,
    wake_rx: &mpsc::Receiver<()>,
    poll: Duration,
) {
    let mut aggregator = StageAggregator::new();
    let mut drum = DrumIdentifier::new();
    loop {
        drain(bus, snapshot, &mut aggregator);
        identify_drum(snapshot, &aggregator, &mut drum);
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        match wake_rx.recv_timeout(poll) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    // Final drain so telemetry emitted between the last poll and shutdown is
    // still folded into the estimates callers read after joining.
    drain(bus, snapshot, &mut aggregator);
    identify_drum(snapshot, &aggregator, &mut drum);
}

/// Drains and folds every currently-queued sample.
fn drain(bus: &TelemetryBus, snapshot: &Snapshot, aggregator: &mut StageAggregator) {
    while let Some(sample) = bus.pop() {
        if let Some(rate) = aggregator.fold(&sample) {
            snapshot.store(sample.stage, rate);
        }
    }
}

/// Runs one drum-identification evaluation window over the aggregated signals
/// and publishes the debounced designation.
fn identify_drum(snapshot: &Snapshot, aggregator: &StageAggregator, drum: &mut DrumIdentifier) {
    let designation = drum.observe(&aggregator.stage_signals());
    snapshot.store_drum(designation);
}

/// Handle to a spawned governor: the only public surface after `spawn`.
///
/// Exposes the three facade operations the design calls for - [`sample_sink`]
/// for stages to publish telemetry, [`subscribe_actuator`] for consumers of
/// tuning signals (a no-op in G2), and [`shutdown`] - plus read-only
/// introspection ([`throughput`], [`dropped_samples`]) used by tests and later
/// actuators. Dropping the handle shuts the governor down and joins its thread.
///
/// [`sample_sink`]: GovernorHandle::sample_sink
/// [`subscribe_actuator`]: GovernorHandle::subscribe_actuator
/// [`shutdown`]: GovernorHandle::shutdown
/// [`throughput`]: GovernorHandle::throughput
/// [`dropped_samples`]: GovernorHandle::dropped_samples
#[derive(Debug)]
pub struct GovernorHandle {
    mode: GovernorMode,
    bus: Arc<TelemetryBus>,
    snapshot: Arc<Snapshot>,
    /// Buffer-pool ceiling the governor publishes to buffer-pool actuators.
    buffer_ceiling: Arc<BufferCeiling>,
    shutdown: Arc<AtomicBool>,
    wake: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl GovernorHandle {
    /// The mode this governor was spawned in.
    #[must_use]
    pub fn mode(&self) -> GovernorMode {
        self.mode
    }

    /// A producer handle for stages to publish [`StageSample`]s, or `None` when
    /// the governor is [`GovernorMode::Off`].
    ///
    /// Stages hold the returned `Option<SampleSink>`; a `None` makes every
    /// emission site a no-op, which is exactly the disabled-path guarantee.
    /// The sink is a cheap `Arc` clone, safe to hand to every worker thread.
    ///
    /// [`StageSample`]: super::sample::StageSample
    #[must_use]
    pub fn sample_sink(&self) -> Option<SampleSink> {
        match self.mode {
            GovernorMode::Off => None,
            GovernorMode::Observe => Some(SampleSink::new(Arc::clone(&self.bus))),
        }
    }

    /// Subscribes to actuator signals.
    ///
    /// An [`GovernorMode::Off`] governor hands out an inert
    /// [`ActuatorHandle`] that reports no tuning action, so a subscribed
    /// actuator keeps its static behaviour and the build stays byte-identical.
    /// An [`GovernorMode::Observe`] governor hands out a handle backed by the
    /// shared buffer-pool ceiling, so [`publish_buffer_pool_ceiling`] becomes
    /// visible to every subscriber. The rope and I/O-depth actuators attach to
    /// this same subscription in later steps.
    ///
    /// [`publish_buffer_pool_ceiling`]: GovernorHandle::publish_buffer_pool_ceiling
    #[must_use]
    pub fn subscribe_actuator(&self) -> ActuatorHandle {
        match self.mode {
            GovernorMode::Off => ActuatorHandle::inert(),
            GovernorMode::Observe => ActuatorHandle::active(Arc::clone(&self.buffer_ceiling)),
        }
    }

    /// Publishes a buffer-pool soft-capacity ceiling to every subscribed
    /// actuator, or clears it with `None`.
    ///
    /// The ceiling is an upper bound: a subscribed buffer pool composes it with
    /// its local pressure tracker via conservative-min, so it can only restrain
    /// growth, never force the pool larger than local demand wants. A `None`
    /// clears the signal and reverts subscribers to local-only pressure.
    ///
    /// A no-op on an [`GovernorMode::Off`] governor, whose subscribers are inert
    /// and never read the signal - the disabled path stays byte-identical.
    pub fn publish_buffer_pool_ceiling(&self, ceiling: Option<usize>) {
        self.buffer_ceiling.publish(ceiling);
    }

    /// Latest smoothed throughput (bytes/second) for `stage`, or `None` before
    /// the governor has folded a usable sample for it. Always `None` when the
    /// governor is off.
    #[must_use]
    pub fn throughput(&self, stage: Constraint) -> Option<f64> {
        self.snapshot.load(stage)
    }

    /// The governor's current drum designation - the identified constraint
    /// stage.
    ///
    /// [`Constraint::Unknown`] until the sense loop has run enough
    /// [hysteresis](super::drum::HYSTERESIS_WINDOWS) windows to commit a drum,
    /// and always [`Constraint::Unknown`] when the governor is off (no sense
    /// loop runs). The designation is debounced, so it reflects a sustained
    /// constraint rather than a single noisy window.
    #[must_use]
    pub fn drum(&self) -> Constraint {
        self.snapshot.load_drum()
    }

    /// Total telemetry samples dropped due to bus overflow.
    ///
    /// A non-zero count means the governor briefly fell behind the data path;
    /// the transfer was unaffected because emission never blocks.
    #[must_use]
    pub fn dropped_samples(&self) -> u64 {
        self.bus.dropped()
    }

    /// Signals shutdown and joins the governor thread.
    ///
    /// Idempotent: sets the cooperative flag, wakes the sense loop, and joins.
    /// Subsequent calls (and the `Drop` path) are no-ops. Off-mode governors
    /// have no thread and return immediately.
    ///
    /// # Panics
    ///
    /// Propagates a panic from the governor thread, matching
    /// [`std::thread::JoinHandle::join`] semantics. The sense loop contains no
    /// `panic!`, `unwrap`, or `expect`, so this cannot fire in practice.
    pub fn shutdown(&mut self) {
        self.stop();
    }

    /// Internal shutdown shared by [`GovernorHandle::shutdown`] and `Drop`.
    fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        // Wake the sense loop out of its `recv_timeout` so it observes the flag
        // immediately rather than after the poll window elapses. Dropping the
        // sender also disconnects the channel, a second independent wake path.
        if let Some(wake) = self.wake.take() {
            let _ = wake.send(());
            drop(wake);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for GovernorHandle {
    /// Guarantees the governor thread is joined on every exit path, including
    /// unwinding when an unrelated worker thread panics.
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::throughput::sample::StageSample;

    fn wait_until<F: Fn() -> bool>(pred: F) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if pred() {
                return;
            }
            std::thread::yield_now();
        }
        panic!("condition not met within timeout");
    }

    #[test]
    fn from_env_value_defaults_off() {
        assert_eq!(GovernorMode::from_env_value(None), GovernorMode::Off);
        assert_eq!(GovernorMode::from_env_value(Some("off")), GovernorMode::Off);
        assert_eq!(GovernorMode::from_env_value(Some("0")), GovernorMode::Off);
        assert_eq!(GovernorMode::from_env_value(Some("")), GovernorMode::Off);
        assert_eq!(
            GovernorMode::from_env_value(Some("ON")),
            GovernorMode::Observe
        );
        assert_eq!(
            GovernorMode::from_env_value(Some(" observe ")),
            GovernorMode::Observe
        );
        assert_eq!(
            GovernorMode::from_env_value(Some("1")),
            GovernorMode::Observe
        );
    }

    #[test]
    fn off_mode_has_no_sink_and_no_thread() {
        let mut gov = Governor::spawn(GovernorConfig::new(GovernorMode::Off));
        assert!(gov.sample_sink().is_none(), "off governor emits nothing");
        assert!(gov.thread.is_none(), "off governor spawns no thread");
        assert_eq!(gov.throughput(Constraint::Read), None);
        gov.shutdown(); // must be a clean no-op
    }

    #[test]
    fn observe_mode_folds_samples_into_stage_throughput() {
        let mut gov = Governor::spawn(GovernorConfig {
            mode: GovernorMode::Observe,
            bus_capacity: 64,
            // Short poll so the loop drains promptly under test.
            poll_interval: Duration::from_millis(5),
        });
        let sink = gov.sample_sink().expect("observe governor exposes a sink");
        // 4096 bytes in 1 ms = 4_096_000 bytes/s.
        for _ in 0..8 {
            sink.emit(StageSample::new(
                Constraint::Compute,
                4_096,
                Duration::from_millis(1),
                0,
            ));
        }
        wait_until(|| gov.throughput(Constraint::Compute).is_some());
        let rate = gov.throughput(Constraint::Compute).expect("folded");
        assert!((rate - 4_096_000.0).abs() < 1.0, "rate {rate}");
        gov.shutdown();
    }

    #[test]
    fn off_mode_subscribe_is_inert_and_publish_is_a_noop() {
        let gov = Governor::spawn(GovernorConfig::new(GovernorMode::Off));
        let actuator = gov.subscribe_actuator();
        assert!(!actuator.is_active());
        // Publishing on an off governor changes nothing an inert subscriber sees.
        gov.publish_buffer_pool_ceiling(Some(64));
        assert_eq!(actuator.buffer_pool_ceiling(), None);
    }

    #[test]
    fn observe_mode_publishes_ceiling_to_subscribers() {
        let mut gov = Governor::spawn(GovernorConfig::new(GovernorMode::Observe));
        let actuator = gov.subscribe_actuator();
        // Wired but unpublished: no guidance yet.
        assert!(!actuator.is_active());
        assert_eq!(actuator.buffer_pool_ceiling(), None);

        gov.publish_buffer_pool_ceiling(Some(24));
        assert!(actuator.is_active());
        assert_eq!(actuator.buffer_pool_ceiling(), Some(24));

        // A late subscriber sees the already-published ceiling.
        assert_eq!(gov.subscribe_actuator().buffer_pool_ceiling(), Some(24));

        gov.publish_buffer_pool_ceiling(None);
        assert_eq!(actuator.buffer_pool_ceiling(), None);
        gov.shutdown();
    }

    #[test]
    fn off_mode_drum_is_unknown() {
        let gov = Governor::spawn(GovernorConfig::new(GovernorMode::Off));
        assert_eq!(
            gov.drum(),
            Constraint::Unknown,
            "off governor names no drum"
        );
    }

    #[test]
    fn sense_loop_identifies_a_sustained_drum() {
        // WireWrite is sampled with a full input queue while its downstream
        // (Consumer) is never sampled - the classic constraint signature. After
        // enough hysteresis windows the governor must commit it as the drum.
        let mut gov = Governor::spawn(GovernorConfig {
            mode: GovernorMode::Observe,
            bus_capacity: 256,
            poll_interval: Duration::from_millis(2),
        });
        let sink = gov.sample_sink().expect("sink");
        // Emit continuously so successive poll windows keep classifying
        // WireWrite as the candidate until hysteresis commits it.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            sink.emit(StageSample::new(
                Constraint::WireWrite,
                4_096,
                Duration::from_millis(1),
                8, // full input queue -> high normalized occupancy
            ));
            if gov.drum() == Constraint::WireWrite {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "drum never committed to WireWrite"
            );
            std::thread::yield_now();
        }
        gov.shutdown();
        assert_eq!(gov.drum(), Constraint::WireWrite);
    }

    #[test]
    fn shutdown_joins_cleanly_and_is_idempotent() {
        let mut gov = Governor::spawn(GovernorConfig::new(GovernorMode::Observe));
        gov.shutdown();
        assert!(gov.thread.is_none(), "thread joined and cleared");
        gov.shutdown(); // second call is a no-op
    }

    #[test]
    fn governor_joins_cleanly_despite_worker_panic() {
        // A data-path worker panicking must not prevent the independent
        // governor thread from joining. Spawn a governor, then panic an
        // unrelated worker and catch it, then shut the governor down.
        let mut gov = Governor::spawn(GovernorConfig::new(GovernorMode::Observe));
        let sink = gov.sample_sink().expect("sink");
        let worker = std::thread::spawn(move || {
            sink.emit(StageSample::new(
                Constraint::Read,
                1,
                Duration::from_millis(1),
                0,
            ));
            panic!("simulated worker panic");
        });
        assert!(worker.join().is_err(), "worker did panic");
        // Governor is unaffected and joins cleanly on shutdown.
        gov.shutdown();
        assert!(gov.thread.is_none());
    }

    #[test]
    fn drop_shuts_down_without_explicit_shutdown() {
        let gov = Governor::spawn(GovernorConfig::new(GovernorMode::Observe));
        // Dropping must join the thread; a leaked thread would keep the process
        // busy. No assertion beyond "drop does not hang or panic".
        drop(gov);
    }

    #[test]
    fn dropped_samples_counts_bus_overflow() {
        // Off governor never drains, but we can exercise the counter via a
        // tiny bus behind an Observe handle whose loop we starve by flooding.
        let gov = Governor::spawn(GovernorConfig {
            mode: GovernorMode::Observe,
            bus_capacity: 1,
            poll_interval: Duration::from_secs(3600),
        });
        let sink = gov.sample_sink().expect("sink");
        // The loop drains at most one before parking for an hour; flood so the
        // ring overflows and increments the drop counter deterministically.
        let mut dropped_seen = false;
        for _ in 0..10_000 {
            sink.emit(StageSample::new(
                Constraint::Read,
                1,
                Duration::from_millis(1),
                0,
            ));
            if gov.dropped_samples() > 0 {
                dropped_seen = true;
                break;
            }
        }
        assert!(dropped_seen, "expected at least one overflow drop");
    }
}
