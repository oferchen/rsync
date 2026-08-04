//! Composition of adaptive resizing with the throughput-governor ceiling.
//!
//! The buffer pool subscribes to the governor's buffer-pool actuator signal and
//! folds it with the local pressure tracker via conservative-min. These tests
//! pin the composition truth table, the byte-identical governor-off fallback,
//! the single-action ("no double-resize") guarantee, and grow/shrink under
//! concurrent acquire/release.

use super::super::pressure::ResizeAction;
use super::super::*;
use crate::throughput::{Governor, GovernorConfig, GovernorHandle, GovernorMode};
use std::sync::{Arc, Barrier};
use std::thread;

/// An Observe governor with a ceiling published, plus the pool subscribed to it.
fn pool_with_ceiling(start_cap: usize, ceiling: usize) -> (BufferPool, GovernorHandle) {
    let gov = Governor::spawn(GovernorConfig::new(GovernorMode::Observe));
    gov.publish_buffer_pool_ceiling(Some(ceiling));
    let pool = BufferPool::new(start_cap)
        .with_adaptive_resizing()
        .with_governor_actuator(gov.subscribe_actuator());
    (pool, gov)
}

// --- composition truth table (pure, deterministic) --------------------------

#[test]
fn no_actuator_passes_local_action_through_unchanged() {
    let pool = BufferPool::new(2).with_adaptive_resizing();
    assert_eq!(
        pool.compose_with_governor(ResizeAction::Grow(64), 2),
        ResizeAction::Grow(64)
    );
    assert_eq!(
        pool.compose_with_governor(ResizeAction::Shrink(2), 8),
        ResizeAction::Shrink(2)
    );
    assert_eq!(
        pool.compose_with_governor(ResizeAction::Hold, 4),
        ResizeAction::Hold
    );
}

#[test]
fn inert_governor_handle_is_local_only() {
    // An Off governor hands out an inert actuator: composition must be identical
    // to having no actuator at all - the byte-identical disabled path.
    let gov = Governor::spawn(GovernorConfig::new(GovernorMode::Off));
    let pool = BufferPool::new(2)
        .with_adaptive_resizing()
        .with_governor_actuator(gov.subscribe_actuator());
    assert_eq!(
        pool.compose_with_governor(ResizeAction::Grow(64), 2),
        ResizeAction::Grow(64)
    );
}

#[test]
fn active_handle_without_a_published_ceiling_is_local_only() {
    // Subscribed to an Observe governor, but nothing published yet.
    let gov = Governor::spawn(GovernorConfig::new(GovernorMode::Observe));
    let pool = BufferPool::new(2)
        .with_adaptive_resizing()
        .with_governor_actuator(gov.subscribe_actuator());
    assert_eq!(
        pool.compose_with_governor(ResizeAction::Grow(64), 2),
        ResizeAction::Grow(64)
    );
}

#[test]
fn conservative_min_caps_local_grow_at_the_ceiling() {
    // Governor wants X=8, local wants Y=64 -> min is 8.
    let (pool, gov) = pool_with_ceiling(2, 8);
    assert_eq!(
        pool.compose_with_governor(ResizeAction::Grow(64), 2),
        ResizeAction::Grow(8)
    );
    drop(pool);
    drop(gov);
}

#[test]
fn conservative_min_keeps_local_grow_when_local_is_smaller() {
    // Governor ceiling X=64, local wants Y=4 -> min is 4 (local wins).
    let (pool, gov) = pool_with_ceiling(2, 64);
    assert_eq!(
        pool.compose_with_governor(ResizeAction::Grow(4), 2),
        ResizeAction::Grow(4)
    );
    drop(pool);
    drop(gov);
}

#[test]
fn ceiling_at_or_below_current_demotes_grow_to_hold() {
    // Ceiling below the current capacity must not force a shrink: the grow is
    // simply demoted to Hold.
    let (pool, gov) = pool_with_ceiling(4, 2);
    assert_eq!(
        pool.compose_with_governor(ResizeAction::Grow(8), 4),
        ResizeAction::Hold
    );
    drop(pool);
    drop(gov);
}

#[test]
fn governor_never_alters_shrink_or_hold() {
    // The ceiling restrains growth only; local shrink and hold pass through.
    let (pool, gov) = pool_with_ceiling(8, 2);
    assert_eq!(
        pool.compose_with_governor(ResizeAction::Shrink(4), 8),
        ResizeAction::Shrink(4)
    );
    assert_eq!(
        pool.compose_with_governor(ResizeAction::Hold, 8),
        ResizeAction::Hold
    );
    drop(pool);
    drop(gov);
}

// --- end-to-end through the acquire path ------------------------------------

#[test]
fn adaptive_grow_is_bounded_by_the_governor_ceiling() {
    // Cap starts at 2; local pressure alone would double repeatedly, but the
    // governor caps the pool at 4. Force sustained misses by holding buffers.
    let (pool, gov) = pool_with_ceiling(2, 4);
    let pool = Arc::new(pool);
    let mut held = Vec::new();
    for _ in 0..256 {
        held.push(BufferPool::acquire_from(Arc::clone(&pool)));
    }
    let cap = pool.max_buffers();
    assert!(cap > 2, "expected some growth, got {cap}");
    assert!(cap <= 4, "governor ceiling must bound growth, got {cap}");
    drop(held);
    drop(gov);
}

#[test]
fn single_evaluation_applies_one_capped_action_not_two() {
    // Exactly one check interval (64 acquires, all misses) must grow the pool to
    // min(local_double, ceiling) in a single step - never to the local double
    // first and then a second corrective resize. Local would double 2 -> 4; the
    // ceiling of 3 clamps the single applied action to 3.
    let (pool, gov) = pool_with_ceiling(2, 3);
    let pool = Arc::new(pool);
    let held: Vec<_> = (0..64)
        .map(|_| BufferPool::acquire_from(Arc::clone(&pool)))
        .collect();
    assert_eq!(
        pool.max_buffers(),
        3,
        "one evaluation must land exactly on the composed min"
    );
    drop(held);
    drop(gov);
}

#[test]
fn grow_and_shrink_under_concurrent_acquire_release_stays_within_bounds() {
    // Hammer the pool from many threads with the governor active. The soft
    // capacity must never escape [MIN, ceiling] regardless of interleaving, and
    // no acquire/return may panic.
    const CEILING: usize = 8;
    let (pool, gov) = pool_with_ceiling(2, CEILING);
    let pool = Arc::new(pool);
    let threads = 8;
    let barrier = Arc::new(Barrier::new(threads));

    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let pool = Arc::clone(&pool);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                for i in 0..2000 {
                    if i % 3 == 0 {
                        // Briefly hold several buffers to spike misses (grow).
                        let bufs: Vec<_> = (0..4)
                            .map(|_| BufferPool::acquire_from(Arc::clone(&pool)))
                            .collect();
                        drop(bufs);
                    } else {
                        // Acquire/release one at a time (hits -> idle -> shrink).
                        let _b = BufferPool::acquire_from(Arc::clone(&pool));
                    }
                    let cap = pool.max_buffers();
                    assert!(
                        (1..=CEILING).contains(&cap),
                        "capacity {cap} escaped [1, {CEILING}]"
                    );
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("worker thread panicked");
    }
    let cap = pool.max_buffers();
    assert!(
        (1..=CEILING).contains(&cap),
        "final capacity {cap} escaped [1, {CEILING}]"
    );
    drop(gov);
}

#[test]
fn lowering_the_ceiling_stops_further_growth() {
    // Grow to the initial ceiling, then lower it; subsequent evaluations must
    // not push past the new, tighter bound.
    let (pool, gov) = pool_with_ceiling(2, 8);
    let pool = Arc::new(pool);
    let warm: Vec<_> = (0..128)
        .map(|_| BufferPool::acquire_from(Arc::clone(&pool)))
        .collect();
    drop(warm);
    let grown = pool.max_buffers();
    assert!(grown > 2, "expected growth toward 8, got {grown}");

    gov.publish_buffer_pool_ceiling(Some(grown));
    let held: Vec<_> = (0..256)
        .map(|_| BufferPool::acquire_from(Arc::clone(&pool)))
        .collect();
    assert!(
        pool.max_buffers() <= grown,
        "tightened ceiling must halt growth, got {}",
        pool.max_buffers()
    );
    drop(held);
    drop(gov);
}
