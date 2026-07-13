//! Concurrent map of per-directory [`DeletePlan`] values.
//!
//! Phase 1 of the parallel-deterministic-delete pipeline produces one
//! [`DeletePlan`] per content directory from rayon worker threads; phase 2
//! drains them on the single emitter thread in upstream traversal order.
//! [`DeletePlanMap`] is the rendezvous between the two phases.
//!
//! # Concurrent Map Choice
//!
//! This first cut uses [`std::sync::Mutex`] wrapping
//! [`std::collections::HashMap`]. The choice is deliberate:
//!
//! - It pulls in no new dependency. The workspace's `dashmap` crate is
//!   already available, but adding it to the `engine` crate's dependency
//!   set increases compile time and surface area for a hot module that
//!   does not yet have a measured bottleneck.
//! - Insert and remove operations are O(1) on the hot path. The lock is
//!   held only for the map operation itself; no I/O or sorting happens
//!   under the lock.
//! - Phase 1 publishes a plan exactly once per directory, and phase 2
//!   drains it exactly once. The expected contention pattern is
//!   short-lived, with at most as many concurrent writers as the rayon
//!   worker pool and exactly one reader.
//!
//! The bench-driven selection between [`std::sync::Mutex`] +
//! [`HashMap`](std::collections::HashMap), [`dashmap::DashMap`], and a
//! sharded [`HashMap`](std::collections::HashMap) is tracked as task
//! DDP-B4. Swap the inner type without changing the public surface of
//! this module.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::plan::DeletePlan;

/// Concurrent map from destination-relative directory path to its
/// publish-once [`DeletePlan`].
///
/// All methods are thread-safe. The map is intended to be wrapped in
/// [`std::sync::Arc`] and shared between the rayon workers that compute
/// extras and the single emitter that consumes them.
#[derive(Debug, Default)]
pub struct DeletePlanMap {
    // NOTE(DDP-B4): single global Mutex is the simplest correct backing
    // store. Bench against `dashmap::DashMap` and a sharded variant
    // before promoting either into the engine crate.
    inner: Mutex<HashMap<PathBuf, DeletePlan>>,
}

impl DeletePlanMap {
    /// Creates an empty map.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Creates an empty map with the given pre-allocated capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(HashMap::with_capacity(capacity)),
        }
    }

    /// Inserts a plan, indexed by `plan.directory`.
    ///
    /// Returns the previously published plan for the same directory, if
    /// any. A non-`None` return indicates the publish-once invariant has
    /// been violated and the caller should treat it as a bug.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned. A poisoned map signals
    /// that a peer thread crashed mid-publish, leaving the publish-once
    /// invariant in an undefined state; continuing would risk emitting
    /// duplicate or out-of-order deletes, so the only safe response is
    /// to abort.
    pub fn insert(&self, plan: DeletePlan) -> Option<DeletePlan> {
        let key = plan.directory.clone();
        self.inner
            .lock()
            .expect("DeletePlanMap mutex poisoned")
            .insert(key, plan)
    }

    /// Removes and returns the plan for `dir`, if any.
    ///
    /// The emitter calls this exactly once per directory after the
    /// [`super::DirTraversalCursor`] yields the directory and the plan
    /// has been published. Returns `None` if the slot is empty.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned. The take side of the
    /// publish-once protocol cannot recover from a producer crash; the
    /// unread half of the map is undefined and continuing would risk
    /// silent under-deletion.
    pub fn take(&self, dir: &Path) -> Option<DeletePlan> {
        self.inner
            .lock()
            .expect("DeletePlanMap mutex poisoned")
            .remove(dir)
    }

    /// Reports whether the map has no published plans right now.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned. See [`Self::insert`]
    /// for why this map treats poisoning as fatal.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner
            .lock()
            .expect("DeletePlanMap mutex poisoned")
            .is_empty()
    }

    /// Returns the number of plans currently published in the map.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned. See [`Self::insert`]
    /// for why this map treats poisoning as fatal.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("DeletePlanMap mutex poisoned")
            .len()
    }

    /// Returns the total number of extras summed across every published
    /// plan.
    ///
    /// Used by the drain to size the delete workload before choosing
    /// between the sequential fast path and the parallel consumer. The
    /// lock is held only for the summation, mirroring the other accessors
    /// in this module.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned. See [`Self::insert`]
    /// for why this map treats poisoning as fatal.
    #[must_use]
    pub fn total_extras_count(&self) -> usize {
        self.inner
            .lock()
            .expect("DeletePlanMap mutex poisoned")
            .values()
            .map(|plan| plan.extras.len())
            .sum()
    }

    /// Returns `true` when a plan for `dir` is currently published.
    ///
    /// Useful for tests and for the emitter to distinguish "not yet
    /// produced" from "already consumed". For draining, prefer
    /// [`Self::take`].
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned. See [`Self::insert`]
    /// for why this map treats poisoning as fatal.
    #[must_use]
    pub fn contains(&self, dir: &Path) -> bool {
        self.inner
            .lock()
            .expect("DeletePlanMap mutex poisoned")
            .contains_key(dir)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::*;
    use crate::delete::plan::{DeleteEntry, DeleteEntryKind};

    fn make_plan(dir: &str) -> DeletePlan {
        let mut plan = DeletePlan::new(PathBuf::from(dir));
        plan.push(DeleteEntry::new(
            std::ffi::OsString::from(format!("{dir}-entry")),
            DeleteEntryKind::File,
        ));
        plan
    }

    #[test]
    fn new_map_is_empty() {
        let map = DeletePlanMap::new();
        assert!(map.is_empty());
        assert_eq!(map.len(), 0);
    }

    #[test]
    fn insert_and_take_roundtrip() {
        let map = DeletePlanMap::new();
        let plan = make_plan("sub");
        assert!(map.insert(plan.clone()).is_none());
        assert!(!map.is_empty());
        assert_eq!(map.len(), 1);
        assert!(map.contains(Path::new("sub")));
        let taken = map.take(Path::new("sub")).expect("plan present");
        assert_eq!(taken.directory, plan.directory);
        assert_eq!(taken.extras.len(), 1);
        assert!(map.is_empty());
        assert!(!map.contains(Path::new("sub")));
    }

    #[test]
    fn take_missing_returns_none() {
        let map = DeletePlanMap::new();
        assert!(map.take(Path::new("missing")).is_none());
    }

    #[test]
    fn duplicate_insert_returns_previous() {
        let map = DeletePlanMap::new();
        let first = make_plan("dup");
        let mut second = make_plan("dup");
        second.push(DeleteEntry::new(
            std::ffi::OsString::from("extra"),
            DeleteEntryKind::Dir,
        ));
        assert!(map.insert(first).is_none());
        let displaced = map.insert(second.clone());
        assert!(displaced.is_some(), "duplicate insert displaces previous");
        let current = map.take(Path::new("dup")).expect("plan present");
        assert_eq!(current.extras.len(), 2);
    }

    #[test]
    fn total_extras_count_sums_across_plans() {
        let map = DeletePlanMap::new();
        assert_eq!(map.total_extras_count(), 0);
        // `make_plan` publishes one extra per directory.
        map.insert(make_plan("a"));
        map.insert(make_plan("b"));
        assert_eq!(map.total_extras_count(), 2);
        // A plan with three extras lifts the sum to 5.
        let mut wide = DeletePlan::new(PathBuf::from("wide"));
        for name in ["one", "two", "three"] {
            wide.push(DeleteEntry::new(
                std::ffi::OsString::from(name),
                DeleteEntryKind::File,
            ));
        }
        map.insert(wide);
        assert_eq!(map.total_extras_count(), 5);
        // Draining a directory removes its extras from the sum.
        map.take(Path::new("wide"));
        assert_eq!(map.total_extras_count(), 2);
    }

    #[test]
    fn with_capacity_preallocates() {
        // Capacity is a hint; we can only assert correctness of the
        // resulting map.
        let map = DeletePlanMap::with_capacity(8);
        assert!(map.is_empty());
        for i in 0..8 {
            map.insert(make_plan(&format!("d{i}")));
        }
        assert_eq!(map.len(), 8);
    }

    #[test]
    fn concurrent_inserts_all_visible() {
        // Spawn N writers, each publishing a distinct directory. After
        // joining, every plan must be retrievable via take().
        const THREADS: usize = 16;
        const PER_THREAD: usize = 32;
        let map = Arc::new(DeletePlanMap::new());
        let mut handles = Vec::new();
        for t in 0..THREADS {
            let map = Arc::clone(&map);
            handles.push(thread::spawn(move || {
                for i in 0..PER_THREAD {
                    let dir = format!("t{t}/d{i}");
                    let plan = make_plan(&dir);
                    assert!(
                        map.insert(plan).is_none(),
                        "no two writers share a directory"
                    );
                }
            }));
        }
        for h in handles {
            h.join().expect("writer joined");
        }
        assert_eq!(map.len(), THREADS * PER_THREAD);
        for t in 0..THREADS {
            for i in 0..PER_THREAD {
                let dir = PathBuf::from(format!("t{t}/d{i}"));
                let plan = map.take(&dir).expect("plan retrievable");
                assert_eq!(plan.directory, dir);
            }
        }
        assert!(map.is_empty());
    }

    #[test]
    fn concurrent_take_does_not_lose_plans() {
        // Pre-populate, then drain from multiple threads. Use an atomic
        // counter to ensure every plan was taken exactly once.
        use std::sync::atomic::{AtomicUsize, Ordering};
        const N: usize = 256;
        let map = Arc::new(DeletePlanMap::new());
        for i in 0..N {
            map.insert(make_plan(&format!("d{i}")));
        }
        let taken = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let map = Arc::clone(&map);
            let taken = Arc::clone(&taken);
            handles.push(thread::spawn(move || {
                for i in 0..N {
                    if map.take(Path::new(&format!("d{i}"))).is_some() {
                        taken.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }));
        }
        for h in handles {
            h.join().expect("reader joined");
        }
        assert_eq!(taken.load(Ordering::Relaxed), N);
        assert!(map.is_empty());
    }
}
