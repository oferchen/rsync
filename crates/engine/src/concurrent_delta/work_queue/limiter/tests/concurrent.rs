//! Multi-threaded contention, panic-path Drop safety, and concurrent error
//! injection bounds.

use std::io;
use std::panic;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use super::super::OverloadReason;
use super::limiter_with;

#[test]
fn acquire_saturation_under_threads() {
    // Stress the CAS loop: two threads racing to acquire the last slot.
    let limiter = Arc::new(limiter_with(1, 1, 4));
    let stop = Arc::new(AtomicBool::new(false));
    let stop_w = stop.clone();
    let limiter_w = limiter.clone();
    let handle = thread::spawn(move || {
        while !stop_w.load(Ordering::Acquire) {
            if let Some(t) = limiter_w.try_acquire() {
                t.record_success();
            }
        }
    });
    thread::sleep(Duration::from_millis(20));
    for _ in 0..1000 {
        if let Some(t) = limiter.try_acquire() {
            t.record_success();
        }
    }
    stop.store(true, Ordering::Release);
    handle.join().unwrap();
    // The CAS loop guarantees in_flight returns to 0 after all tickets are
    // released. If the loop were broken, in_flight would be non-zero or
    // would have raced past target.
    assert_eq!(limiter.in_flight(), 0);
}

#[test]
fn ticket_drop_on_panic_decrements_in_flight() {
    let limiter = Arc::new(limiter_with(2, 1, 8));
    let limiter_clone = limiter.clone();
    let result = panic::catch_unwind(panic::AssertUnwindSafe(move || {
        let _t = limiter_clone.try_acquire().expect("slot");
        assert_eq!(limiter_clone.in_flight(), 1);
        panic!("simulated worker panic");
    }));
    assert!(result.is_err(), "panic should propagate");
    assert_eq!(
        limiter.in_flight(),
        0,
        "Drop must decrement in_flight on unwind"
    );
    // Target untouched: drop path does not record success or overload.
    assert_eq!(limiter.target(), 2);
}

#[test]
fn concurrent_acquire_release_preserves_in_flight_invariant() {
    // Multiple threads doing acquire-record cycles should never leave
    // in_flight stuck or negative. This tests the CAS loop and Drop
    // safety under contention.
    let limiter = Arc::new(limiter_with(8, 1, 64));
    let barrier = Arc::new(std::sync::Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let limiter = limiter.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                for _ in 0..500 {
                    if let Some(t) = limiter.try_acquire() {
                        // Alternate between success and overload to
                        // exercise both paths concurrently.
                        if i % 2 == 0 {
                            t.record_success();
                        } else {
                            t.record_overload(OverloadReason::QueueSaturated);
                        }
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(
        limiter.in_flight(),
        0,
        "all slots must be released after concurrent workers finish",
    );
    // Target must be within valid bounds.
    let target = limiter.target();
    assert!(
        (1..=64).contains(&target),
        "target {target} must be within [min_limit, max_limit]",
    );
}

#[test]
fn dropped_ticket_does_not_affect_convergence() {
    // Simulate a panic mid-computation: the ticket is dropped without
    // recording. This must not disturb subsequent convergence.
    let limiter = Arc::new(limiter_with(4, 1, 32));

    // Exit slow-start.
    let t = limiter.try_acquire().unwrap();
    t.record_overload(OverloadReason::RttSpike);
    assert_eq!(limiter.target(), 2);

    thread::sleep(Duration::from_millis(1));

    // Drop a ticket without recording (simulated panic).
    {
        let _t = limiter.try_acquire().unwrap();
        assert_eq!(limiter.in_flight(), 1);
        // _t dropped here
    }
    assert_eq!(limiter.in_flight(), 0, "drop must release slot");
    assert_eq!(limiter.target(), 2, "drop must not change target",);

    // Convergence should still work normally after dropped ticket.
    for _ in 0..2 {
        let t = limiter.try_acquire().unwrap();
        t.record_success();
    }
    assert_eq!(
        limiter.target(),
        3,
        "additive increase resumes after dropped ticket",
    );
}

#[test]
fn concurrent_error_injection_preserves_bounds() {
    // Multiple threads injecting errors and successes concurrently
    // must never push target outside [min_limit, max_limit].
    let min = 2;
    let max = 64;
    let limiter = Arc::new(limiter_with(16, min, max));
    for _ in 0..8 {
        limiter.update_rtt(500_000);
    }

    let barrier = Arc::new(std::sync::Barrier::new(4));
    let violation = Arc::new(AtomicBool::new(false));
    let handles: Vec<_> = (0..4)
        .map(|thread_id| {
            let limiter = limiter.clone();
            let barrier = barrier.clone();
            let violation = violation.clone();
            thread::spawn(move || {
                barrier.wait();
                for i in 0..200 {
                    if let Some(t) = limiter.try_acquire() {
                        // Thread 0,1: success; thread 2,3: overload.
                        if thread_id < 2 {
                            t.record_success();
                        } else {
                            // Transient error -> overload path.
                            t.record_error(if i % 3 == 0 {
                                io::ErrorKind::TimedOut
                            } else {
                                io::ErrorKind::WouldBlock
                            });
                        }
                    }
                    let target = limiter.target();
                    if target < min || target > max {
                        violation.store(true, Ordering::Release);
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert!(
        !violation.load(Ordering::Acquire),
        "target went outside [{min}, {max}] during concurrent error injection",
    );
    assert_eq!(
        limiter.in_flight(),
        0,
        "all slots must be released after workers finish",
    );
}
