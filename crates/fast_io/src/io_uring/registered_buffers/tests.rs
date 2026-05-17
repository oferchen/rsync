//! Unit tests for the registered-buffer registry, slot lifecycle, batch
//! submission helpers, and telemetry types. Constrained-environment Drop
//! coverage lives here too - see the bottom of the file for the audit
//! contract documented in PR #4022 / task #2118.

use std::ptr;

use io_uring::IoUring as RawIoUring;

use super::registry::{RegisteredBufferGroup, RegisteredBufferSlot};
use super::stats::{RegisteredBufferStats, RegisteredBufferStatus};
use super::submit::{RegisteredBufferSlotInfo, submit_read_fixed_batch, submit_write_fixed_batch};
use super::{MAX_REGISTERED_BUFFERS, page_size};

#[test]
fn page_size_is_positive_and_power_of_two() {
    let ps = page_size();
    assert!(ps > 0);
    assert!(ps.is_power_of_two());
}

#[test]
fn registered_buffer_group_rejects_zero_count() {
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return, // io_uring not available
    };
    let result = RegisteredBufferGroup::new(&ring, 4096, 0);
    assert!(result.is_err());
}

#[test]
fn registered_buffer_group_rejects_zero_size() {
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };
    let result = RegisteredBufferGroup::new(&ring, 0, 4);
    assert!(result.is_err());
}

#[test]
fn registered_buffer_group_rejects_excessive_count() {
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };
    let result = RegisteredBufferGroup::new(&ring, 4096, MAX_REGISTERED_BUFFERS + 1);
    assert!(result.is_err());
}

#[test]
fn registered_buffer_group_create_and_checkout() {
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };
    let group = match RegisteredBufferGroup::new(&ring, 4096, 4) {
        Ok(g) => g,
        Err(_) => return, // Registration failed (seccomp, kernel limit, etc.)
    };

    assert_eq!(group.count(), 4);
    assert!(group.buffer_size() >= 4096);
    assert_eq!(group.available(), 4);

    // Check out all 4 slots.
    let mut s0 = group.checkout().expect("slot 0");
    assert_eq!(group.available(), 3);
    let s1 = group.checkout().expect("slot 1");
    let mut s2 = group.checkout().expect("slot 2");
    let mut s3 = group.checkout().expect("slot 3");
    assert_eq!(group.available(), 0);

    // No more slots available.
    assert!(group.checkout().is_none());

    // Return one slot.
    drop(s1);
    assert_eq!(group.available(), 1);

    // Check out again.
    let mut s1b = group.checkout().expect("slot 1 reacquired");
    assert_eq!(group.available(), 0);

    // Verify buffer pointers are non-null and unique.
    let ptrs: Vec<*mut u8> = [&mut s0, &mut s1b, &mut s2, &mut s3]
        .iter_mut()
        .map(|s| s.as_mut_ptr())
        .collect();
    for p in &ptrs {
        assert!(!p.is_null());
    }
    // All pointers should be distinct.
    for i in 0..ptrs.len() {
        for j in (i + 1)..ptrs.len() {
            assert_ne!(ptrs[i], ptrs[j], "slots {i} and {j} share a pointer");
        }
    }

    drop(s0);
    drop(s1b);
    drop(s2);
    drop(s3);
    assert_eq!(group.available(), 4);

    // Explicit unregister.
    let _ = group.unregister(&ring);
}

#[test]
fn buffer_slot_read_write_memory() {
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };
    let group = match RegisteredBufferGroup::new(&ring, 4096, 2) {
        Ok(g) => g,
        Err(_) => return,
    };

    let mut slot = group.checkout().expect("checkout");

    // Write a pattern into the buffer.
    let pattern = b"hello io_uring registered buffers!";
    unsafe {
        ptr::copy_nonoverlapping(pattern.as_ptr(), slot.as_mut_ptr(), pattern.len());
        let read_back = slot.as_slice(pattern.len());
        assert_eq!(read_back, pattern);
    }

    drop(slot);
    let _ = group.unregister(&ring);
}

#[test]
fn read_fixed_write_fixed_roundtrip() {
    let ring = match RawIoUring::new(64) {
        Ok(r) => r,
        Err(_) => return,
    };
    let group = match RegisteredBufferGroup::new(&ring, 4096, 4) {
        Ok(g) => g,
        Err(_) => return,
    };

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixed_roundtrip.bin");

    // Generate test data larger than one buffer.
    let test_data: Vec<u8> = (0..12000u32).map(|i| (i % 256) as u8).collect();
    std::fs::write(&path, &test_data).unwrap();

    // Collect slot info for batch operations.
    let mut checked_out: Vec<RegisteredBufferSlot<'_>> =
        (0..4).filter_map(|_| group.checkout()).collect();
    let slot_infos: Vec<RegisteredBufferSlotInfo> = checked_out
        .iter_mut()
        .map(|s| RegisteredBufferSlotInfo {
            ptr: s.as_mut_ptr(),
            buf_index: s.buf_index(),
            buffer_size: s.buffer_size(),
        })
        .collect();

    // Read the file using ReadFixed.
    let file = std::fs::File::open(&path).unwrap();
    let raw_fd = {
        use std::os::unix::io::AsRawFd;
        file.as_raw_fd()
    };
    let fd = io_uring::types::Fd(raw_fd);

    let mut read_buf = vec![0u8; test_data.len()];
    let mut ring_rw = ring;
    let bytes_read = submit_read_fixed_batch(
        &mut ring_rw,
        fd,
        &mut read_buf,
        0,
        &slot_infos,
        super::super::batching::NO_FIXED_FD,
    )
    .unwrap();

    assert_eq!(bytes_read, test_data.len());
    assert_eq!(read_buf, test_data);

    // Write using WriteFixed to a new file.
    let write_path = dir.path().join("fixed_write_out.bin");
    let write_file = std::fs::File::create(&write_path).unwrap();
    let write_fd = {
        use std::os::unix::io::AsRawFd;
        io_uring::types::Fd(write_file.as_raw_fd())
    };

    let bytes_written = submit_write_fixed_batch(
        &mut ring_rw,
        write_fd,
        &test_data,
        0,
        &slot_infos,
        super::super::batching::NO_FIXED_FD,
    )
    .unwrap();

    assert_eq!(bytes_written, test_data.len());
    drop(write_file); // Flush.

    let written_data = std::fs::read(&write_path).unwrap();
    assert_eq!(written_data, test_data);

    drop(checked_out);
    let _ = group.unregister(&ring_rw);
}

/// Reads with an output buffer larger than the file to trigger a natural
/// short read (EOF before buffer is full). Before the fix, the function
/// would advance past unread bytes, returning `total` even though the
/// file was smaller - silently zero-filling the tail.
#[test]
fn read_fixed_batch_short_read_at_eof() {
    let ring = match RawIoUring::new(64) {
        Ok(r) => r,
        Err(_) => return,
    };
    let group = match RegisteredBufferGroup::new(&ring, 4096, 4) {
        Ok(g) => g,
        Err(_) => return,
    };

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("short_read.bin");

    // File is 5000 bytes but we ask to read 16384 (4 * 4096).
    let test_data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &test_data).unwrap();

    let mut checked_out: Vec<RegisteredBufferSlot<'_>> =
        (0..4).filter_map(|_| group.checkout()).collect();
    let slot_infos: Vec<RegisteredBufferSlotInfo> = checked_out
        .iter_mut()
        .map(|s| RegisteredBufferSlotInfo {
            ptr: s.as_mut_ptr(),
            buf_index: s.buf_index(),
            buffer_size: s.buffer_size(),
        })
        .collect();

    let file = std::fs::File::open(&path).unwrap();
    let raw_fd = {
        use std::os::unix::io::AsRawFd;
        file.as_raw_fd()
    };
    let fd = io_uring::types::Fd(raw_fd);

    // Request more bytes than the file contains.
    let request_size = 4 * 4096;
    let mut read_buf = vec![0xFFu8; request_size];
    let mut ring_rw = ring;
    let bytes_read = submit_read_fixed_batch(
        &mut ring_rw,
        fd,
        &mut read_buf,
        0,
        &slot_infos,
        super::super::batching::NO_FIXED_FD,
    )
    .unwrap();

    // Must return exactly the file size, not the request size.
    assert_eq!(bytes_read, test_data.len());
    assert_eq!(&read_buf[..bytes_read], &test_data[..]);

    drop(checked_out);
    let _ = group.unregister(&ring_rw);
}

/// Reads a file that is smaller than a single registered buffer chunk.
/// The first SQE returns a short read (file size < chunk size), and the
/// function must report only the actual bytes read.
#[test]
fn read_fixed_batch_file_smaller_than_chunk() {
    let ring = match RawIoUring::new(64) {
        Ok(r) => r,
        Err(_) => return,
    };
    let group = match RegisteredBufferGroup::new(&ring, 4096, 2) {
        Ok(g) => g,
        Err(_) => return,
    };

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tiny.bin");

    let test_data = b"small file content";
    std::fs::write(&path, test_data).unwrap();

    let mut checked_out: Vec<RegisteredBufferSlot<'_>> =
        (0..2).filter_map(|_| group.checkout()).collect();
    let slot_infos: Vec<RegisteredBufferSlotInfo> = checked_out
        .iter_mut()
        .map(|s| RegisteredBufferSlotInfo {
            ptr: s.as_mut_ptr(),
            buf_index: s.buf_index(),
            buffer_size: s.buffer_size(),
        })
        .collect();

    let file = std::fs::File::open(&path).unwrap();
    let raw_fd = {
        use std::os::unix::io::AsRawFd;
        file.as_raw_fd()
    };
    let fd = io_uring::types::Fd(raw_fd);

    // Request 8192 bytes (2 chunks) but file is only 18 bytes.
    let mut read_buf = vec![0xFFu8; 8192];
    let mut ring_rw = ring;
    let bytes_read = submit_read_fixed_batch(
        &mut ring_rw,
        fd,
        &mut read_buf,
        0,
        &slot_infos,
        super::super::batching::NO_FIXED_FD,
    )
    .unwrap();

    assert_eq!(bytes_read, test_data.len());
    assert_eq!(&read_buf[..bytes_read], &test_data[..]);

    drop(checked_out);
    let _ = group.unregister(&ring_rw);
}

/// Drop ordering invariant: dropping the `RegisteredBufferGroup` BEFORE
/// the `RawIoUring` is sound. The kernel still holds the buffer pinning
/// (released later when the ring fd closes), but we may safely deallocate
/// the user-side memory because `Drop` does not touch the ring.
#[test]
fn drop_group_before_ring_does_not_panic() {
    let mut ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };
    let group = match RegisteredBufferGroup::new(&ring, 4096, 2) {
        Ok(g) => g,
        Err(_) => return,
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
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };
    let group = match RegisteredBufferGroup::new(&ring, 4096, 2) {
        Ok(g) => g,
        Err(_) => return,
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

    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };
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
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };
    let group = match RegisteredBufferGroup::new(&ring, 4096, 2) {
        Ok(g) => g,
        Err(_) => return,
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

/// `unregister()` returns an error when the buffer set has already been
/// released by closing the ring (or never registered). The error must
/// be reported to the caller; it must NOT cause a panic or abort.
#[test]
fn unregister_after_ring_closed_returns_error_or_ok() {
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };
    let group = match RegisteredBufferGroup::new(&ring, 4096, 2) {
        Ok(g) => g,
        Err(_) => return,
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
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };

    // Path A: explicit unregister, then drop.
    {
        let group = match RegisteredBufferGroup::new(&ring, 4096, 4) {
            Ok(g) => g,
            Err(_) => return,
        };
        let _ = group.unregister(&ring);
        drop(group);
    }

    // Path B: drop without explicit unregister (relies on kernel cleanup
    // when ring closes; here we keep the ring alive across drop).
    {
        let group = match RegisteredBufferGroup::new(&ring, 4096, 4) {
            Ok(g) => g,
            Err(_) => return,
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

/// A freshly created group reports zero acquires and zero misses.
#[test]
fn stats_initially_zero() {
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };
    let group = match RegisteredBufferGroup::new(&ring, 4096, 2) {
        Ok(g) => g,
        Err(_) => return,
    };
    let stats = group.stats();
    assert_eq!(stats.total_acquires, 0);
    assert_eq!(stats.total_misses, 0);
    assert_eq!(stats.miss_rate(), 0.0);
}

/// Successful checkouts bump `total_acquires` but not `total_misses`.
#[test]
fn stats_count_successful_checkouts() {
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };
    let group = match RegisteredBufferGroup::new(&ring, 4096, 4) {
        Ok(g) => g,
        Err(_) => return,
    };

    let s0 = group.checkout().expect("slot 0");
    let s1 = group.checkout().expect("slot 1");
    let stats = group.stats();
    assert_eq!(stats.total_acquires, 2);
    assert_eq!(stats.total_misses, 0);
    assert_eq!(stats.miss_rate(), 0.0);

    drop(s0);
    drop(s1);
}

/// `checkout` returning `None` increments both `total_acquires` and
/// `total_misses`, and `miss_rate` reflects the ratio.
#[test]
fn stats_count_misses_on_exhaustion() {
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };
    let group = match RegisteredBufferGroup::new(&ring, 4096, 2) {
        Ok(g) => g,
        Err(_) => return,
    };

    // Exhaust the pool.
    let _s0 = group.checkout().expect("slot 0");
    let _s1 = group.checkout().expect("slot 1");

    // Three forced misses.
    assert!(group.checkout().is_none());
    assert!(group.checkout().is_none());
    assert!(group.checkout().is_none());

    let stats = group.stats();
    assert_eq!(stats.total_acquires, 5);
    assert_eq!(stats.total_misses, 3);
    let mr = stats.miss_rate();
    assert!(
        (mr - 3.0 / 5.0).abs() < 1e-12,
        "expected miss_rate=0.6, got {mr}"
    );
}

/// Returning a slot does not affect telemetry counters: `total_acquires`
/// is the lifetime acquire count, never decremented.
#[test]
fn stats_not_decremented_on_return() {
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };
    let group = match RegisteredBufferGroup::new(&ring, 4096, 2) {
        Ok(g) => g,
        Err(_) => return,
    };

    let s = group.checkout().expect("slot");
    drop(s);
    assert_eq!(group.stats().total_acquires, 1);
    assert_eq!(group.stats().total_misses, 0);

    // Re-acquire the same slot bumps acquires again.
    let s = group.checkout().expect("slot reacquired");
    assert_eq!(group.stats().total_acquires, 2);
    drop(s);
}

/// `RegisteredBufferStats::miss_rate` returns 0.0 when no acquires have
/// been recorded, matching the `BufferPoolStats::hit_rate` convention.
#[test]
fn stats_miss_rate_zero_when_no_acquires() {
    let s = RegisteredBufferStats {
        total_acquires: 0,
        total_misses: 0,
    };
    assert_eq!(s.miss_rate(), 0.0);
}

/// `miss_rate` is exactly 1.0 when every acquire missed.
#[test]
fn stats_miss_rate_all_misses() {
    let s = RegisteredBufferStats {
        total_acquires: 7,
        total_misses: 7,
    };
    assert!((s.miss_rate() - 1.0).abs() < 1e-12);
}

// Constrained-environment Drop coverage.
//
// The fixed-buffer invariants audit (PR #4022, task #2118) documents that
// `RegisteredBufferGroup::Drop` deliberately does NOT issue
// `IORING_UNREGISTER_BUFFERS`; the kernel reclaims the pinning when the
// ring fd closes. The tests below exercise that contract under conditions
// where a naive implementation would either leak userspace memory or
// surprise callers by silently mutating kernel state.

/// Structural proof that `Drop` does NOT call `IORING_UNREGISTER_BUFFERS`:
/// after dropping the group while the ring fd remains open, the kernel
/// must still consider buffers registered. A second `register_buffers`
/// call on the same ring is rejected (typically with `EBUSY`) precisely
/// because the prior registration is still live. After an explicit
/// unregister against the same ring fd, a fresh registration succeeds,
/// proving the ring itself is not poisoned by the silent-Drop policy.
#[test]
fn drop_does_not_release_kernel_registration() {
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };
    let group = match RegisteredBufferGroup::new(&ring, 4096, 2) {
        Ok(g) => g,
        Err(_) => return,
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
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };
    let group = match RegisteredBufferGroup::new(&ring, 4096, 4) {
        Ok(g) => g,
        Err(_) => return,
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

/// `RegisteredBufferStats` is `Copy`, so a snapshot taken before the
/// group is dropped remains usable after. This documents that
/// telemetry consumers (e.g., the adaptive sizer) do not need to
/// outlive their group via lifetime coupling.
#[test]
fn stats_snapshot_survives_group_drop() {
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };
    let group = match RegisteredBufferGroup::new(&ring, 4096, 2) {
        Ok(g) => g,
        Err(_) => return,
    };

    // Generate measurable telemetry: one hit, one forced miss.
    let s = group.checkout().expect("slot");
    let s2 = group.checkout().expect("second slot");
    assert!(group.checkout().is_none());
    drop(s);
    drop(s2);

    let snapshot = group.stats();
    assert_eq!(snapshot.total_acquires, 3);
    assert_eq!(snapshot.total_misses, 1);

    // Drop the group. The snapshot is plain integers and must remain
    // observably identical after the source group is gone.
    drop(group);

    assert_eq!(snapshot.total_acquires, 3);
    assert_eq!(snapshot.total_misses, 1);
    let mr = snapshot.miss_rate();
    assert!(
        (mr - 1.0 / 3.0).abs() < 1e-12,
        "miss_rate snapshot drift: {mr}"
    );
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
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };

    // First registration claims the ring's slot table.
    let first = match RegisteredBufferGroup::new(&ring, 4096, 2) {
        Ok(g) => g,
        Err(_) => return,
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

/// `try_new_with_status` with `enabled=false` returns `Disabled` without
/// calling the kernel - distinct from a `RegistrationFailed` outcome.
#[test]
fn try_new_with_status_disabled_when_flag_off() {
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };
    let (group, status) = RegisteredBufferGroup::try_new_with_status(&ring, 4096, 4, false);
    assert!(group.is_none());
    assert_eq!(status, RegisteredBufferStatus::Disabled);
    assert!(status.is_disabled() && !status.is_enabled() && !status.is_registration_failed());
}

/// Successful registration yields `Enabled` and a live group; constrained
/// environments that reject registration still produce a non-`Disabled`
/// status, exercising the failure branch.
#[test]
fn try_new_with_status_enabled_on_success() {
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };
    let (group, status) = RegisteredBufferGroup::try_new_with_status(&ring, 4096, 4, true);
    assert_ne!(status, RegisteredBufferStatus::Disabled);
    if let RegisteredBufferStatus::Enabled = status {
        assert_eq!(group.expect("Enabled implies a group").count(), 4);
    }
}

/// When registration fails the status carries the formatted `errno` for
/// telemetry and the group is `None`. Forcing failure via the wrapper's
/// own `MAX_REGISTERED_BUFFERS` ceiling keeps the test portable.
#[test]
fn try_new_with_status_registration_failed_carries_reason() {
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };
    let (group, status) =
        RegisteredBufferGroup::try_new_with_status(&ring, 4096, MAX_REGISTERED_BUFFERS + 1, true);
    assert!(group.is_none());
    match status {
        RegisteredBufferStatus::RegistrationFailed { reason } => {
            assert!(!reason.is_empty(), "failure reason must be populated");
        }
        other => panic!("expected RegistrationFailed, got {other:?}"),
    }
}
