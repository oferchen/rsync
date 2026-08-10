//! Property-style convergence tests.
//!
//! These tests feed the controller open-loop sample streams (no plant
//! feedback) and assert three closed-loop properties:
//!   1. steady-state convergence under a constant input;
//!   2. bounded output amplitude under a noisy input;
//!   3. respect for the configured upper buffer-size cap under a
//!      saturating signal that would otherwise wind the integrator
//!      unbounded.
//!
//! All randomness is driven by a deterministic seeded SplitMix64 PRNG so
//! the tests are reproducible across runs and platforms. Tolerances are
//! intentionally loose since these are convergence-property checks, not
//! exact-value checks.

use std::time::{Duration, Instant};

use super::super::ControllerConfig;

/// Deterministic 64-bit SplitMix64 PRNG state. Reseeded per-test from a
/// fixed constant so the resulting sample stream is identical on every
/// invocation, regardless of platform RNG behaviour.
struct SplitMix64(u64);

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0.0, 1.0)`.
    fn next_unit(&mut self) -> f64 {
        // 53 bits of precision; standard f64 unit sample.
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Approximate standard-normal sample via the Box-Muller transform.
    fn next_normal(&mut self) -> f64 {
        // Guard against log(0) by clamping the uniform draw away from 0.
        let u1 = self.next_unit().max(f64::MIN_POSITIVE);
        let u2 = self.next_unit();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// Property 1: under a constant input signal, the buffer size converges
/// and the residual oscillation amplitude is small (within +/-5% of the
/// mean across the post-warm-up window).
#[test]
fn property_steady_state_convergence_under_constant_input() {
    // Seed kept here for reproducibility even though this test does not
    // consume any random samples - the seeded RNG is documented as part
    // of the property-test suite contract.
    let _rng = SplitMix64::new(0xCAFE);

    let setpoint = 50 * 1024 * 1024u64;
    let ctrl = ControllerConfig::new(setpoint)
        .gains(0.6, 0.2, 0.05)
        .min_size(16 * 1024)
        .max_size(2 * 1024 * 1024)
        .build();

    // Feed a constant throughput equal to the setpoint for 100 samples.
    // With zero error each step, P and D contribute nothing, and the
    // integrator stays at whatever steady-state value it converged to.
    let mut now = Instant::now();
    for _ in 0..100 {
        now += Duration::from_millis(100);
        ctrl.observe_at(setpoint, now);
    }

    // Collect 50 post-warm-up samples and check the oscillation band.
    let mut sizes = Vec::with_capacity(50);
    for _ in 0..50 {
        now += Duration::from_millis(100);
        ctrl.observe_at(setpoint, now);
        sizes.push(ctrl.buffer_size());
    }

    let mean = sizes.iter().sum::<usize>() as f64 / sizes.len() as f64;
    let min_s = *sizes.iter().min().unwrap() as f64;
    let max_s = *sizes.iter().max().unwrap() as f64;
    // Tolerance: amplitude within +/-5% of the mean. Loose by design;
    // the property is "stays put", not "exact value".
    let amplitude = (max_s - min_s) / mean.max(1.0);
    assert!(
        amplitude < 0.05,
        "steady-state amplitude {amplitude:.4} exceeded 5% of mean {mean:.0}"
    );
}

/// Property 2: under a noisy input (Gaussian, mean below setpoint with
/// non-trivial sigma), the controller's output stays within a bounded
/// amplitude band - it does not diverge or oscillate without limit.
#[test]
fn property_noisy_signal_bounded_output() {
    let mut rng = SplitMix64::new(0xCAFE);

    let setpoint = 100 * 1024 * 1024u64;
    let min = 16 * 1024;
    let max = 4 * 1024 * 1024;
    let ctrl = ControllerConfig::new(setpoint)
        .gains(0.6, 0.2, 0.05)
        .min_size(min)
        .max_size(max)
        .build();

    // Signal model: throughput ~ N(mean=0.3*setpoint, sigma=0.1*setpoint).
    // Mean 0.3 + sigma 0.1 keeps the bulk of samples well below the
    // setpoint, exercising the growth side of the controller under noise.
    let mean = 0.3 * setpoint as f64;
    let sigma = 0.1 * setpoint as f64;

    let mut now = Instant::now();
    // Warm-up: 50 samples to let the controller settle near its
    // working point under the noisy stream.
    for _ in 0..50 {
        now += Duration::from_millis(100);
        let raw = mean + sigma * rng.next_normal();
        let sample = raw.max(0.0) as u64;
        ctrl.observe_at(sample, now);
    }

    // Measure: 200 samples post-warm-up.
    let mut sizes = Vec::with_capacity(200);
    for _ in 0..200 {
        now += Duration::from_millis(100);
        let raw = mean + sigma * rng.next_normal();
        let sample = raw.max(0.0) as u64;
        ctrl.observe_at(sample, now);
        sizes.push(ctrl.buffer_size());
    }

    let mean_size = sizes.iter().sum::<usize>() as f64 / sizes.len() as f64;
    let min_s = *sizes.iter().min().unwrap() as f64;
    let max_s = *sizes.iter().max().unwrap() as f64;

    // Property: output band stays within [0.5x, 2x] of the mean - i.e.
    // the controller damps the noise rather than amplifying it. This is
    // a loose bound chosen so genuine divergence (orders of magnitude
    // swings or rail-to-rail bouncing) is caught while normal residual
    // jitter from a 10%-sigma signal is accepted.
    assert!(
        min_s >= 0.5 * mean_size,
        "output min {min_s:.0} below 0.5x mean {mean_size:.0}"
    );
    assert!(
        max_s <= 2.0 * mean_size,
        "output max {max_s:.0} above 2x mean {mean_size:.0}"
    );

    // And of course, every individual sample respects the hard cap.
    for &s in &sizes {
        assert!(s >= min, "output {s} below configured min {min}");
        assert!(s <= max, "output {s} above configured max {max}");
    }
}

/// Property 3: under a saturating signal that drives the error term
/// hard in one direction for many iterations, the controller respects
/// the configured upper bound - anti-windup prevents the integrator
/// from pushing the recommended size past `max_size`.
#[test]
fn property_cap_respected_under_saturating_signal() {
    // Seed retained for reproducibility parity with the other
    // property tests; this test is deterministic by construction.
    let _rng = SplitMix64::new(0xCAFE);

    let min = 16 * 1024;
    let max = 256 * 1024;
    let setpoint = 100 * 1024 * 1024u64;
    let ctrl = ControllerConfig::new(setpoint)
        .gains(0.6, 0.2, 0.05)
        .min_size(min)
        .max_size(max)
        .build();

    // Feed zero throughput for 500 iterations. Naively, K_i * error * dt
    // accumulated for 500 * 100ms = 50s would push the integrator to
    // K_i * setpoint * 50 = 0.2 * 100MB * 50 = 1 GB, far past `max`.
    // Anti-windup must clamp this so the output never exceeds `max`.
    let mut now = Instant::now();
    for i in 0..500 {
        now += Duration::from_millis(100);
        let size = ctrl.observe_at(0, now);
        assert!(
            size <= max,
            "iteration {i}: output {size} exceeded configured max {max}"
        );
        assert!(
            size >= min,
            "iteration {i}: output {size} below configured min {min}"
        );
    }

    // After the saturating run, the controller should be pinned at the
    // upper bound and the integrator must remain finite.
    assert_eq!(ctrl.buffer_size(), max, "controller must pin to max");
    let state = ctrl.state.lock().unwrap();
    assert!(
        state.integral.is_finite(),
        "integrator must be finite under saturation, got {}",
        state.integral
    );
}
