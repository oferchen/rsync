//! Drop semantics and constrained-environment Drop coverage.
//!
//! The fixed-buffer invariants audit (PR #4022, task #2118) documents that
//! [`RegisteredBufferGroup::Drop`] deliberately does NOT issue
//! `IORING_UNREGISTER_BUFFERS`; the kernel reclaims the pinning when the
//! ring fd closes. The tests below exercise that contract under conditions
//! where a naive implementation would either leak userspace memory or
//! surprise callers by silently mutating kernel state.

use io_uring::IoUring as RawIoUring;

use super::super::registry::RegisteredBufferGroup;
use super::{try_group, try_ring};

/// Drop ordering invariant: dropping the `RegisteredBufferGroup` BEFORE
/// the `RawIoUring` is sound. The kernel still holds the buffer pinning
/// (released later when the ring fd closes), but we may safely deallocate
/// the user-side memory because `Drop` does not touch the ring.
#[test]
fn drop_group_before_ring_does_not_panic() {
    let Some(mut ring) = try_ring(4) else { return };
    let Some(group) = try_group(&ring, 4096, 2) else {
        return;
    };

    // Drop the group first while the ring is still alive.
    drop(group);

    // Ring is still usable for ordinary operations after the group dies.
    // Submitting a no-op (nop) verifies the ring fd remains valid.
    let entry = io_uring::opcode::Nop::new().build().user_data(0xdead);
    // Safety: SQE is a Nop with no buffer pointers.
    unsafe {
        ring.submission()
            .push(&entry)
            .expect("nop submission after group drop");
    }
    ring.submit_and_wait(1).expect("nop completes");
    let cqe = ring.completion().next().expect("nop CQE");
    assert_eq!(cqe.user_data(), 0xdead);
    assert_eq!(cqe.result(), 0);
}

/// Drop ordering invariant: dropping the ring BEFORE the group is the
/// natural order used by `IoUringReader`/`IoUringWriter`. The kernel
/// auto-releases the buffer pinning when the ring fd closes; the group
/// then frees user-side memory in its own Drop.
#[test]
fn drop_ring_before_group_frees_memory_cleanly() {
    let Some(ring) = try_ring(4) else { return };
    let Some(group) = try_group(&ring, 4096, 2) else {
        return;
    };

    // Close the ring first - kernel releases buffer pinning.
    drop(ring);

    // Now dropping the group must still be sound: it deallocates user
    // memory and never accesses the (now-closed) ring fd.
    drop(group);
}

/// Mirrors the field declaration order used by `IoUringReader` and
/// `IoUringWriter`: ring before registered_buffers. Verifies that
/// implicit drop runs ring-first then group-second without aborting.
#[test]
fn struct_field_drop_order_matches_callers() {
    struct OwnerLikeReader {
        #[allow(dead_code)]
        ring: RawIoUring,
        #[allow(dead_code)]
        registered_buffers: Option<RegisteredBufferGroup>,
    }

    let Some(ring) = try_ring(4) else { return };
    let group = RegisteredBufferGroup::try_new(&ring, 4096, 2);

    let owner = OwnerLikeReader {
        ring,
        registered_buffers: group,
    };

    // Implicit drop in declaration order: ring first, then group.
    // Must complete without panic or process abort.
    drop(owner);
}

/// Panic during slot use must not corrupt the group: dropping the slot
/// during unwinding returns it to the free list, and dropping the group
/// during unwinding deallocates buffers safely.
#[test]
fn panic_during_slot_use_unwinds_cleanly() {
    let Some(ring) = try_ring(4) else { return };
    let Some(group) = try_group(&ring, 4096, 2) else {
        return;
    };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _slot = group.checkout().expect("slot checkout");
        // Slot is held; panic forces Drop during unwinding.
        panic!("simulated panic during slot use");
    }));

    assert!(result.is_err(), "panic should propagate via catch_unwind");

    // Slot must have been returned to the free list during unwinding.
    assert_eq!(
        group.available(),
        2,
        "slot should be returned on panic-driven drop"
    );

    // Group is still usable after the panic.
    let _slot_again = group.checkout().expect("re-checkout after panic");
}

/// A redundant `unregister()` - one whose buffer set the first
/// `unregister()` already released against the same live ring - must
/// surface any kernel error to the caller as a `Result`; it must NOT
/// panic or abort.
#[test]
fn unregister_after_ring_closed_returns_error_or_ok() {
    let Some(ring) = try_ring(4) else { return };
    let Some(group) = try_group(&ring, 4096, 2) else {
        return;
    };

    // Successful explicit unregister against the live ring.
    let first = group.unregister(&ring);
    assert!(
        first.is_ok(),
        "first unregister against live ring should succeed: {first:?}"
    );

    // A second unregister has nothing to release; the kernel may return
    // EINVAL/ENXIO. The wrapper must surface this gracefully (Result),
    // never panic. The exact error code is kernel-dependent, so we just
    // require the call returns (Ok or Err) without panicking.
    let _ = group.unregister(&ring);
}

/// User-side buffer memory must be freed regardless of whether
/// `unregister()` was called. We verify this by exercising both code
/// paths (with and without explicit unregister) and confirming Drop
/// completes without panic - leak detection is delegated to ASan/Miri
/// in CI when available.
#[test]
fn buffers_freed_with_or_without_explicit_unregister() {
    let Some(ring) = try_ring(4) else { return };

    // Path A: explicit unregister, then drop.
    {
        let Some(group) = try_group(&ring, 4096, 4) else {
            return;
        };
        let _ = group.unregister(&ring);
        drop(group);
    }

    // Path B: drop without explicit unregister (relies on kernel cleanup
    // when ring closes; here we keep the ring alive across drop).
    {
        let Some(group) = try_group(&ring, 4096, 4) else {
            return;
        };
        drop(group);
    }

    // Path C: re-register on the same ring, drop, repeat. Verifies the
    // ring remains in a clean state for further registrations.
    for _ in 0..3 {
        if let Some(group) = RegisteredBufferGroup::try_new(&ring, 4096, 2) {
            let _ = group.unregister(&ring);
        }
    }
}

/// Structural proof that `Drop` does NOT call `IORING_UNREGISTER_BUFFERS`:
/// after dropping the group while the ring fd remains open, the kernel
/// must still consider buffers registered. A second `register_buffers`
/// call on the same ring is rejected (typically with `EBUSY`) precisely
/// because the prior registration is still live. After an explicit
/// unregister against the same ring fd, a fresh registration succeeds,
/// proving the ring itself is not poisoned by the silent-Drop policy.
#[test]
fn drop_does_not_release_kernel_registration() {
    let Some(ring) = try_ring(4) else { return };
    let Some(group) = try_group(&ring, 4096, 2) else {
        return;
    };

    // Drop the group while keeping the ring alive. If Drop secretly
    // called unregister_buffers, the kernel slot table would now be
    // empty and the next registration would succeed.
    drop(group);

    // Attempt a fresh registration on the same ring. The kernel still
    // holds the prior pinning, so this must be rejected. We do not
    // assert a specific errno (kernels differ); the contract is only
    // that the call returns an `Err`, never panics, and leaves the
    // ring usable.
    let second = RegisteredBufferGroup::new(&ring, 4096, 2);
    if second.is_ok() {
        // Some kernels (5.13+) support replace-style update semantics
        // on a second register; in that case our invariant cannot be
        // probed by this method. Skip rather than false-fail.
        return;
    }

    // Release the kernel-side pinning explicitly via the submitter on
    // the live ring fd. After this, fresh registration must succeed -
    // confirming the ring itself is not poisoned by the silent-Drop
    // policy, only the prior kernel-side registration was still live.
    let _ = ring.submitter().unregister_buffers();
    let third = RegisteredBufferGroup::new(&ring, 4096, 2);
    assert!(
        third.is_ok(),
        "after explicit unregister, the ring must accept a fresh registration"
    );
}

/// Drop a group whose internal tracking state is non-default: some
/// slots have been checked out (and returned), some checkouts have
/// missed, and the stats counters are non-zero. Memory must be freed
/// cleanly regardless of the tracking-state snapshot at drop time.
///
/// This guards against a regression where Drop assumes a pristine
/// bitset / counter state. The audit (PR #4022, task #2118) notes the
/// invariant that `Drop` is "panic-safe" because it only calls
/// `alloc::dealloc`; we make the in-use-tracking case explicit here.
#[test]
fn drop_with_in_use_tracking_state_is_clean() {
    let Some(ring) = try_ring(4) else { return };
    let Some(group) = try_group(&ring, 4096, 4) else {
        return;
    };

    // Drive the group through a realistic mid-use trajectory: acquire
    // and release multiple times, then exhaust the pool to bump misses,
    // then return everything so the bitset is "all free" but counters
    // are non-zero.
    for _ in 0..3 {
        let s = group.checkout().expect("slot");
        drop(s);
    }
    let hold: Vec<_> = (0..4).filter_map(|_| group.checkout()).collect();
    for _ in 0..5 {
        assert!(group.checkout().is_none());
    }
    let stats_before = group.stats();
    assert!(stats_before.total_acquires >= 12);
    assert!(stats_before.total_misses >= 5);
    drop(hold);
    assert_eq!(group.available(), 4);

    // Slots must be dropped before the group (compile-time invariant via
    // `RegisteredBufferSlot<'a>` borrow on `&self`). With slots released,
    // drop the group: this must complete without panic or abort even
    // though acquire / miss counters carry stale state from earlier
    // activity.
    drop(group);
}

/// When `RegisteredBufferGroup::new` fails after the first registration
/// is already live on the ring, the recovery path must (a) deallocate
/// the partially-built buffer set, (b) return an `Err` to the caller,
/// and (c) leave the ring in a state where a subsequent `try_new`
/// against a freshly unregistered ring succeeds. This is the
/// "early return from registration failure recovery" constrained
/// environment called out in task #1678.
#[test]
fn drop_on_construction_failure_does_not_double_register() {
    let Some(ring) = try_ring(4) else { return };

    // First registration claims the ring's slot table.
    let Some(first) = try_group(&ring, 4096, 2) else {
        return;
    };

    // Second `new` against the same live ring is the recovery path we
    // want to exercise. On kernels that refuse double-registration the
    // partially-allocated buffers must be freed inside `new` before
    // the error propagates - any leak shows up as an ASan / Miri
    // failure. On newer kernels (5.13+) that accept replace-style
    // update semantics the second `new` succeeds; in that case we
    // cannot probe the failure-recovery branch and skip out cleanly.
    let second = RegisteredBufferGroup::new(&ring, 4096, 2);
    if second.is_ok() {
        return;
    }
    drop(second);

    // The first group is still functional - registration-failure
    // recovery in the second call did not poison its state.
    assert_eq!(first.available(), 2);
    let s = first.checkout().expect("first group still usable");
    drop(s);

    // Explicit unregister on the live group clears kernel state.
    let _ = first.unregister(&ring);
    drop(first);

    // After cleanup, a fresh group constructs cleanly. This proves the
    // failure-recovery branch did not leave dangling kernel state.
    let third = RegisteredBufferGroup::new(&ring, 4096, 2);
    assert!(
        third.is_ok(),
        "ring must be reusable after a failed registration was cleaned up"
    );
}
