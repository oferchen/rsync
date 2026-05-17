//! Poison-tolerant lock acquisition helpers.
//!
//! [`std::sync::Mutex`] and [`std::sync::RwLock`] poison their inner state when
//! the thread holding the guard panics. The default reaction (`.unwrap()` or
//! `.expect(...)`) propagates that panic to every subsequent waiter, often
//! turning a single-thread bug into a worker-pool-wide cascade.
//!
//! For state that remains structurally valid after a panic - bounded buffers,
//! event recorders, counters, append-only queues - recovery via
//! [`std::sync::PoisonError::into_inner`] is preferable to aborting the
//! workers. The helpers below encapsulate that pattern so call sites stay
//! short and consistent.
//!
//! # When to use these helpers
//!
//! - The protected state is monotonic or otherwise self-consistent regardless
//!   of where the previous panic occurred (counters, ring buffers, queues,
//!   log sinks).
//! - Continuing the transfer is preferable to aborting the worker pool.
//!
//! # When NOT to use these helpers
//!
//! - The protected invariants depend on a multi-step update that the panic
//!   may have interrupted halfway (e.g. partially mutated index structures
//!   where one field has been updated but another has not). Let the panic
//!   propagate so the bug surfaces immediately.
//! - The lock guards external resources (file descriptors, network sockets)
//!   whose state cannot be reasoned about after an unwind.
//!
//! In doubt, prefer propagation. Silent recovery hides bugs.

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Acquire a [`Mutex`] guard, recovering the inner value if the lock is
/// poisoned.
///
/// Use only when the protected state is known to remain valid across a panic;
/// see the module documentation for guidance.
#[inline]
pub fn lock_or_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Acquire an [`RwLock`] read guard, recovering the inner value if the lock
/// is poisoned.
///
/// Use only when the protected state is known to remain valid across a panic;
/// see the module documentation for guidance.
#[inline]
pub fn read_or_recover<T>(rw: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    rw.read().unwrap_or_else(|e| e.into_inner())
}

/// Acquire an [`RwLock`] write guard, recovering the inner value if the lock
/// is poisoned.
///
/// Use only when the protected state is known to remain valid across a panic;
/// see the module documentation for guidance.
#[inline]
pub fn write_or_recover<T>(rw: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    rw.write().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Arc;
    use std::thread;

    fn poison_mutex(m: &Arc<Mutex<Vec<u32>>>) {
        let clone = Arc::clone(m);
        let handle = thread::spawn(move || {
            let mut guard = clone.lock().expect("mutex acquired pre-panic");
            guard.push(7);
            panic!("intentional poison");
        });
        let _ = handle.join();
        assert!(m.is_poisoned(), "mutex should be poisoned after panic");
    }

    fn poison_rwlock(rw: &Arc<RwLock<Vec<u32>>>) {
        let clone = Arc::clone(rw);
        let handle = thread::spawn(move || {
            let mut guard = clone.write().expect("rwlock acquired pre-panic");
            guard.push(11);
            panic!("intentional poison");
        });
        let _ = handle.join();
        assert!(rw.is_poisoned(), "rwlock should be poisoned after panic");
    }

    #[test]
    fn lock_or_recover_returns_guard_when_poisoned() {
        let m = Arc::new(Mutex::new(vec![1, 2, 3]));
        poison_mutex(&m);

        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut guard = lock_or_recover(&m);
            guard.push(4);
            guard.clone()
        }));

        let recovered = result.expect("helper must not panic on poisoned mutex");
        assert_eq!(recovered, vec![1, 2, 3, 7, 4]);
    }

    #[test]
    fn lock_or_recover_works_on_healthy_mutex() {
        let m = Mutex::new(0_u32);
        *lock_or_recover(&m) += 5;
        assert_eq!(*lock_or_recover(&m), 5);
    }

    #[test]
    fn read_or_recover_returns_guard_when_poisoned() {
        let rw = Arc::new(RwLock::new(vec![10_u32]));
        poison_rwlock(&rw);

        let result = catch_unwind(AssertUnwindSafe(|| {
            let guard = read_or_recover(&rw);
            guard.clone()
        }));

        let recovered = result.expect("helper must not panic on poisoned rwlock");
        assert_eq!(recovered, vec![10, 11]);
    }

    #[test]
    fn write_or_recover_returns_guard_when_poisoned() {
        let rw = Arc::new(RwLock::new(vec![20_u32]));
        poison_rwlock(&rw);

        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut guard = write_or_recover(&rw);
            guard.push(30);
            guard.clone()
        }));

        let recovered = result.expect("helper must not panic on poisoned rwlock");
        assert_eq!(recovered, vec![20, 11, 30]);
    }

    #[test]
    fn rwlock_helpers_work_on_healthy_lock() {
        let rw = RwLock::new(String::from("hi"));
        write_or_recover(&rw).push_str(" there");
        assert_eq!(&*read_or_recover(&rw), "hi there");
    }
}
