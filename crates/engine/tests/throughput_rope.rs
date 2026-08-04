//! Property and concurrency tests for the drum-buffer-rope admission actuator.
//!
//! These pin the two safety contracts the rope must never break:
//!
//! 1. **Bounded ceiling.** For any drum rate and any work-unit footprint the
//!    computed admission ceiling stays inside the configured `[min, max]`. A
//!    controller arithmetic slip can never push the semaphore out of range.
//! 2. **Balanced permits.** Driving the rope's resizes concurrently with a live
//!    producer/consumer transfer never leaks or double-returns an admission
//!    permit: at quiescence every item has been delivered and the semaphore's
//!    in-flight count is exactly zero, whatever the resize interleaving.
//!
//! A third test proves the rope gates *admission only, not ordering*: a transfer
//! driven through a rope-resized dynamic queue delivers byte-for-byte the same
//! result stream as the static fixed-capacity queue.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use engine::ConcurrentDeltaConfig;
use engine::concurrent_delta::consumer::DeltaConsumer;
use engine::concurrent_delta::work_queue::{self, bounded_dynamic};
use engine::concurrent_delta::{DeltaResult, DeltaWork};
use engine::throughput::{Rope, RopeConfig};

use proptest::prelude::*;

/// A comparable projection of a delivered result - everything an observer can
/// see. Divergence here would mean the rope perturbed the transfer.
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

// --- property 1: ceiling always in [min, max] -------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn ceiling_stays_within_configured_bounds(
        min in 1usize..=64,
        span in 0usize..=512,
        rate in prop::num::f64::ANY,
        unit in 0u64..=(4u64 << 30),
    ) {
        let max = min + span;
        let dq = bounded_dynamic(min, min, max).expect("dynamic queue");
        let config = RopeConfig::new(min, max).expect("valid range");
        let rope = Rope::new(dq.semaphore, config);

        let ceiling = rope.target_ceiling(rate, unit);
        prop_assert!(
            (min..=max).contains(&ceiling),
            "ceiling {ceiling} escaped [{min}, {max}] for rate={rate} unit={unit}"
        );
    }
}

// --- property 2: permits acquired == released at quiescence ------------------

/// Runs `count` items through a rope-resized dynamic queue while a background
/// thread hammers the ceiling with pseudo-random resizes, then asserts the
/// admission semaphore is fully drained (in_flight == 0) and every item was
/// delivered exactly once, in order.
fn run_dynamic_with_resizes(count: u32, min: usize, max: usize) -> Vec<DeltaResult> {
    let dq = bounded_dynamic(min, min, max).expect("dynamic queue");
    let sender = dq.sender;
    let receiver = dq.receiver;
    let semaphore = Arc::clone(&dq.semaphore);

    let config = RopeConfig::new(min, max).expect("valid range");
    let rope = Rope::new(Arc::clone(&semaphore), config);

    // Background resizer: drive the rope across a spread of synthetic drum rates
    // so the ceiling grows and shrinks continuously while work is in flight.
    let stop = Arc::new(AtomicBool::new(false));
    let resizer_stop = Arc::clone(&stop);
    let resizer = std::thread::spawn(move || {
        let mut i: u64 = 0;
        while !resizer_stop.load(Ordering::Relaxed) {
            // Sweep rate across several orders of magnitude and unit size across
            // the buffer tiers, exercising both grow and shrink paths.
            let rate = 10f64.powi((i % 9) as i32);
            let unit = 1u64 << ((i % 24) as u32);
            rope.actuate(rate, unit);
            i += 1;
            std::thread::yield_now();
        }
    });

    let producer = std::thread::spawn(move || {
        for i in 0..count {
            let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dst/{i}")), 64)
                .with_sequence(u64::from(i));
            sender.send(work).expect("send work");
        }
        // Dropping the sender closes the queue so the consumer can finish.
    });

    let consumer = DeltaConsumer::spawn_with_config(receiver, 64, ConcurrentDeltaConfig::default());
    let results: Vec<DeltaResult> = consumer.into_iter().collect();
    producer.join().expect("producer join");
    stop.store(true, Ordering::Relaxed);
    resizer.join().expect("resizer join");

    // Quiescence: every admitted permit was returned at drain time.
    assert_eq!(
        semaphore.in_flight(),
        0,
        "admission permits leaked: in_flight should be zero at quiescence"
    );
    results
}

#[test]
fn permits_balance_under_concurrent_resizes() {
    for &(count, min, max) in &[(256u32, 1usize, 8usize), (512, 2, 16), (1000, 4, 64)] {
        let results = run_dynamic_with_resizes(count, min, max);
        assert_eq!(
            results.len(),
            count as usize,
            "every item must be delivered"
        );
        // Delivered strictly in submission order (the reorder buffer restores it).
        for (i, r) in results.iter().enumerate() {
            assert_eq!(r.sequence(), i as u64, "out-of-order delivery at {i}");
        }
    }
}

// --- property 3: the rope gates admission, not ordering ----------------------

/// Baseline transfer through the static fixed-capacity queue - "today's"
/// behaviour.
fn run_fixed(count: u32) -> Vec<DeltaResult> {
    let (tx, rx) = work_queue::bounded_with_capacity(count.max(1) as usize);
    let producer = std::thread::spawn(move || {
        for i in 0..count {
            let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dst/{i}")), 64)
                .with_sequence(u64::from(i));
            tx.send(work).expect("send work");
        }
    });
    let consumer = DeltaConsumer::spawn_with_config(rx, 64, ConcurrentDeltaConfig::default());
    let results: Vec<DeltaResult> = consumer.into_iter().collect();
    producer.join().expect("producer join");
    results
}

#[test]
fn rope_resized_output_matches_fixed_output() {
    const COUNT: u32 = 400;
    let baseline = run_fixed(COUNT);
    let roped = run_dynamic_with_resizes(COUNT, 2, 16);
    assert_eq!(
        fingerprint(&baseline),
        fingerprint(&roped),
        "the rope gates admission only; it must not change what or in what order results are delivered"
    );
}

#[test]
fn rope_actuate_from_governor_drives_the_ceiling() {
    use engine::throughput::{Constraint, Governor, GovernorConfig, GovernorMode, StageSample};

    // Stand up a dynamic queue + rope + observing governor, feed the governor a
    // sustained WireWrite-constraint signal, and confirm the rope moves the
    // ceiling off its floor once the drum is identified.
    let dq = bounded_dynamic(2, 2, 128).expect("dynamic queue");
    let semaphore = Arc::clone(&dq.semaphore);
    let rope = Rope::new(Arc::clone(&semaphore), RopeConfig::new(2, 128).unwrap());

    let mut gov = Governor::spawn(GovernorConfig {
        mode: GovernorMode::Observe,
        bus_capacity: 256,
        poll_interval: Duration::from_millis(2),
    });
    let sink = gov.sample_sink().expect("sink");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        sink.emit(StageSample::new(
            Constraint::WireWrite,
            1_000_000,
            Duration::from_millis(1),
            8,
        ));
        if gov.drum() == Constraint::WireWrite {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "drum never committed");
        std::thread::yield_now();
    }

    // A fast WireWrite drum with 4 KiB units demands well above the floor.
    let applied = rope.actuate_from(&gov, 4096);
    assert!(
        applied > 2,
        "rope should grow the ceiling for a fast drum, got {applied}"
    );
    assert_eq!(applied, semaphore.current_cap());
    assert!(applied <= 128, "ceiling must respect the configured max");
    gov.shutdown();
}
