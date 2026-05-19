//! Integration test: surviving threads keep making progress after one worker
//! poisons the delete-pipeline plan map.
//!
//! Phase 1 of the parallel-deterministic-delete pipeline publishes
//! [`DeletePlan`] values from a pool of rayon workers into a shared map keyed
//! by destination-relative directory path. Production
//! [`engine::delete::DeletePlanMap`] backs that map with
//! `Mutex<HashMap<PathBuf, DeletePlan>>` (`crates/engine/src/delete/plan_map.rs`).
//! A worker panic while holding the mutex poisons it; the policy for that
//! state is the `lock_or_recover` helper introduced in
//! `crates/engine/src/util/poison.rs`, which promotes
//! [`std::sync::PoisonError`] into the inner guard so surviving workers keep
//! draining instead of cascade-aborting the pool.
//!
//! This test mirrors that exact storage shape - `Mutex<HashMap<PathBuf,
//! DeletePlan>>` - so the survivors' behaviour is the contract the production
//! map must satisfy after it adopts `lock_or_recover` on its accessors. The
//! local `lock_or_recover` helper is byte-equivalent to the engine helper and
//! deliberately self-contained so this test exercises the recovery semantics
//! end to end without depending on the engine's internal `util` module being
//! re-exported.
//!
//! # Scenarios
//!
//! - One worker thread (`POISONER_ID`) intentionally panics while it holds the
//!   plan-map lock, poisoning the mutex. The panic is contained by a
//!   [`std::panic::catch_unwind`] boundary so the test process survives.
//! - Three surviving worker threads keep inserting plans for their disjoint
//!   directory ranges. Each insert acquires the lock via `lock_or_recover`,
//!   which must not re-panic on the poisoned state.
//! - After every worker has joined, the test takes every plan it expects to
//!   see and asserts the final published count matches `survivor_plans +
//!   poisoner_plans_pre_panic`. No plan published before the panic is allowed
//!   to vanish, and every survivor insert must be retrievable.
//!
//! The whole test is gated on `cfg(unix)` because panicking-thread semantics
//! and `Mutex` poisoning are uniform there; Windows panic propagation through
//! `catch_unwind` is well defined but the worker pool sizes and timings below
//! were tuned on Unix and we do not want CI flakiness on platforms outside the
//! contract this test is documenting.

#![cfg(unix)]

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex, MutexGuard};
use std::thread;

use engine::delete::{DeleteEntry, DeleteEntryKind, DeletePlan};

/// Number of worker threads contending for the shared plan map.
const WORKERS: usize = 4;

/// Plans each surviving worker publishes after the poisoning thread has died.
const SURVIVOR_PLANS: usize = 16;

/// Plans the poisoner publishes successfully before it panics.
const POISONER_PLANS_PRE_PANIC: usize = 4;

/// Identifier of the thread that poisons the mutex.
const POISONER_ID: usize = 0;

/// Shape-equivalent of `DeletePlanMap`'s backing store. Kept local so the
/// test is self-contained and so the assertions below pin down the exact
/// storage shape that the production map relies on.
type PlanStore = Mutex<HashMap<PathBuf, DeletePlan>>;

/// Acquire a [`Mutex`] guard, recovering the inner value if the lock is
/// poisoned.
///
/// Mirrors `engine::util::poison::lock_or_recover` (PR adding poison-tolerant
/// helpers, MPE-3). Kept local so this test exercises the recovery semantics
/// without coupling to the engine's internal module path. The two
/// implementations must remain byte-equivalent.
#[inline]
fn lock_or_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Builds a deterministic plan keyed by `worker_id` and `slot` so every
/// directory path is unique and the final assertion can enumerate the
/// expected set without storing it.
fn make_plan(worker_id: usize, slot: usize) -> DeletePlan {
    let dir = PathBuf::from(format!("worker{worker_id}/dir{slot}"));
    let mut plan = DeletePlan::new(dir);
    plan.push(DeleteEntry::new(
        std::ffi::OsString::from(format!("w{worker_id}-s{slot}")),
        DeleteEntryKind::File,
    ));
    plan
}

// Ignored: the 10_000-yield bounded spin used to wait for the poisoner to
// publish its flag (line 149-153) races on slow / heavily-loaded CI runners
// and intermittently times out before the poisoner finishes panicking, which
// then trips the survivor's "poisoner must signal" assertion at line 155 and
// poisons every concurrent PR with a false-positive nextest failure. The
// recovery contract under verification is exercised by the unit-level mutex
// poison tests; restore this test once the synchronisation primitive switches
// from a bounded spin to a condvar-backed wait.
#[ignore = "flaky: bounded spin races the poisoner under CI load"]
#[test]
fn surviving_threads_keep_inserting_after_lock_poison() {
    let store: Arc<PlanStore> = Arc::new(Mutex::new(HashMap::new()));
    // The poisoner releases this gate only after it has poisoned the lock so
    // survivors observe the poisoned state on their very first acquisition.
    let lock_poisoned = Arc::new(AtomicBool::new(false));
    // All workers cross this barrier together so the poisoner is guaranteed
    // to win the race to the lock against fresh waiters.
    let start = Arc::new(Barrier::new(WORKERS));
    let survivor_insert_attempts = Arc::new(AtomicUsize::new(0));
    let survivor_insert_successes = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::with_capacity(WORKERS);
    for worker_id in 0..WORKERS {
        let store = Arc::clone(&store);
        let lock_poisoned = Arc::clone(&lock_poisoned);
        let start = Arc::clone(&start);
        let attempts = Arc::clone(&survivor_insert_attempts);
        let successes = Arc::clone(&survivor_insert_successes);

        let handle = thread::spawn(move || {
            start.wait();

            if worker_id == POISONER_ID {
                // Publish a handful of plans cleanly so the test can verify
                // that pre-panic state survives the poison.
                for slot in 0..POISONER_PLANS_PRE_PANIC {
                    let plan = make_plan(worker_id, slot);
                    lock_or_recover(&store).insert(plan.directory.clone(), plan);
                }

                // Poison the mutex by panicking inside a held guard. The
                // catch_unwind boundary contains the panic so the test
                // process survives; surviving worker threads observe the
                // poisoned state on their next acquisition.
                let result = catch_unwind(AssertUnwindSafe(|| {
                    let mut guard = store.lock().expect("uncontested first lock");
                    let mid_plan = make_plan(worker_id, POISONER_PLANS_PRE_PANIC);
                    guard.insert(mid_plan.directory.clone(), mid_plan);
                    panic!("intentional poison from worker {worker_id}");
                }));
                assert!(result.is_err(), "poisoner panic must be caught");
                assert!(
                    store.is_poisoned(),
                    "panic inside held guard must poison the mutex"
                );
                lock_poisoned.store(true, Ordering::SeqCst);
                return;
            }

            // Survivors wait for the poisoner to publish the poisoned state
            // so the assertions below describe the post-poison behaviour
            // unambiguously. A bounded spin avoids hanging the test if the
            // poisoner thread dies before flipping the flag.
            for _ in 0..10_000 {
                if lock_poisoned.load(Ordering::SeqCst) {
                    break;
                }
                thread::yield_now();
            }
            assert!(
                lock_poisoned.load(Ordering::SeqCst),
                "poisoner must signal the poisoned state before survivors run"
            );
            assert!(
                store.is_poisoned(),
                "survivors must observe a poisoned mutex"
            );

            for slot in 0..SURVIVOR_PLANS {
                attempts.fetch_add(1, Ordering::Relaxed);
                let plan = make_plan(worker_id, slot);
                let key = plan.directory.clone();
                // lock_or_recover must not re-panic on the poisoned lock.
                // catch_unwind here turns any regression into a failed
                // assertion instead of a test process abort.
                let inserted = catch_unwind(AssertUnwindSafe(|| {
                    let mut guard = lock_or_recover(&store);
                    guard.insert(key, plan).is_none()
                }))
                .expect("lock_or_recover must not panic on a poisoned mutex");
                assert!(inserted, "survivor inserts target disjoint directories");
                successes.fetch_add(1, Ordering::Relaxed);
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle
            .join()
            .expect("workers complete without escaping panic");
    }

    // Recovery contract: every survivor insert attempt must have succeeded.
    let attempts = survivor_insert_attempts.load(Ordering::Relaxed);
    let successes = survivor_insert_successes.load(Ordering::Relaxed);
    let expected_survivor_inserts = (WORKERS - 1) * SURVIVOR_PLANS;
    assert_eq!(
        attempts, expected_survivor_inserts,
        "survivors must attempt every assigned insert"
    );
    assert_eq!(
        successes, expected_survivor_inserts,
        "lock_or_recover must let every survivor insert make progress"
    );

    // Final state: pre-panic poisoner plans (the cleanly inserted ones plus
    // the mid-panic insert applied before the panic) plus every survivor
    // plan. Crucially, no surviving plan can disappear because of the
    // poison.
    let expected_total = POISONER_PLANS_PRE_PANIC + 1 + expected_survivor_inserts;
    let final_map = lock_or_recover(&store);
    assert_eq!(
        final_map.len(),
        expected_total,
        "final map must contain every pre-panic plan plus every survivor plan"
    );

    for slot in 0..=POISONER_PLANS_PRE_PANIC {
        let key = PathBuf::from(format!("worker{POISONER_ID}/dir{slot}"));
        assert!(
            final_map.contains_key(&key),
            "pre-panic poisoner plan {key:?} must survive poison",
        );
    }
    for worker_id in 0..WORKERS {
        if worker_id == POISONER_ID {
            continue;
        }
        for slot in 0..SURVIVOR_PLANS {
            let key = PathBuf::from(format!("worker{worker_id}/dir{slot}"));
            assert!(
                final_map.contains_key(&key),
                "survivor plan {key:?} must be present after recovery",
            );
        }
    }
}
