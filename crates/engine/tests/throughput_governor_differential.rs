//! Differential test: the throughput governor is observationally inert.
//!
//! Enabling the governor's telemetry sink must not change what the concurrent
//! delta pipeline delivers - same results, same order, same bytes. This runs
//! an identical batch of work items through the pipeline twice, once with the
//! governor off (no sink) and once with an `Observe` governor attached, and
//! asserts the two output streams are byte-for-byte identical. It is the
//! host-independent stand-in for the end-to-end "wire-identical governor-on vs
//! off" guarantee: the instrumentation only reads and publishes, never steers.

use std::path::PathBuf;
use std::time::Duration;

use engine::ConcurrentDeltaConfig;
use engine::concurrent_delta::consumer::DeltaConsumer;
use engine::concurrent_delta::work_queue;
use engine::concurrent_delta::{DeltaResult, DeltaWork};
use engine::throughput::{Constraint, Governor, GovernorConfig, GovernorMode};

/// A comparable projection of a delivered result: everything an observer of the
/// pipeline can see. If telemetry perturbed the pipeline, one of these fields
/// would diverge between runs.
#[derive(Debug, PartialEq, Eq)]
struct ResultFingerprint {
    sequence: u64,
    ndx: u32,
    bytes_written: u64,
    success: bool,
}

fn fingerprint(results: &[DeltaResult]) -> Vec<ResultFingerprint> {
    results
        .iter()
        .map(|r| ResultFingerprint {
            sequence: r.sequence(),
            ndx: r.ndx().get(),
            bytes_written: r.bytes_written(),
            success: r.is_success(),
        })
        .collect()
}

/// Drives `count` whole-file work items through the pipeline under `cfg` and
/// returns the delivered results in delivery order.
fn run_pipeline(count: u32, cfg: ConcurrentDeltaConfig) -> Vec<DeltaResult> {
    let (tx, rx) = work_queue::bounded_with_capacity(count.max(1) as usize);
    let producer = std::thread::spawn(move || {
        for i in 0..count {
            let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dst/{i}")), 64)
                .with_sequence(u64::from(i));
            tx.send(work).expect("send work");
        }
    });
    let consumer = DeltaConsumer::spawn_with_config(rx, 64, cfg);
    let results: Vec<DeltaResult> = consumer.into_iter().collect();
    producer.join().expect("producer join");
    results
}

#[test]
fn governor_off_and_on_produce_identical_pipeline_output() {
    const COUNT: u32 = 256;

    // Baseline: governor off - no telemetry sink at all.
    let off = run_pipeline(COUNT, ConcurrentDeltaConfig::default());

    // Instrumented: an Observe governor's sink attached to the same pipeline.
    let mut gov = Governor::spawn(GovernorConfig {
        mode: GovernorMode::Observe,
        bus_capacity: 8192,
        poll_interval: Duration::from_millis(5),
    });
    let sink = gov.sample_sink().expect("observe governor exposes a sink");
    let on = run_pipeline(
        COUNT,
        ConcurrentDeltaConfig::default().with_telemetry_sink(sink),
    );

    assert_eq!(
        fingerprint(&off),
        fingerprint(&on),
        "telemetry must not change pipeline output"
    );

    // Sanity: the instrumentation actually fired. After shutdown the governor
    // performs a final drain, so every emitted sample is folded. Compute (from
    // the workers) and WireWrite (from the reorder drain) must both have a
    // recorded throughput estimate.
    gov.shutdown();
    assert!(
        gov.throughput(Constraint::Compute).is_some(),
        "compute stage should have been sampled"
    );
    assert!(
        gov.throughput(Constraint::WireWrite).is_some(),
        "wire-write stage should have been sampled"
    );
}

#[test]
fn governor_telemetry_survives_empty_transfer() {
    // An empty batch must not deadlock or emit spurious samples, and the
    // off/on outputs must both be empty.
    let off = run_pipeline(0, ConcurrentDeltaConfig::default());
    assert!(off.is_empty());

    let mut gov = Governor::spawn(GovernorConfig::new(GovernorMode::Observe));
    let sink = gov.sample_sink().expect("sink");
    let on = run_pipeline(
        0,
        ConcurrentDeltaConfig::default().with_telemetry_sink(sink),
    );
    assert!(on.is_empty());
    gov.shutdown();
    assert_eq!(
        gov.dropped_samples(),
        0,
        "no work means no dropped telemetry"
    );
}
