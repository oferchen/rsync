//! Registry-level tests: page size, validation guards on
//! [`RegisteredBufferGroup::new`], slot checkout / return / exhaustion,
//! and direct read/write access through the slot pointer.

use std::ptr;

use super::super::registry::RegisteredBufferGroup;
use super::super::{MAX_REGISTERED_BUFFERS, page_size};
use super::{try_group, try_ring};

#[test]
fn page_size_is_positive_and_power_of_two() {
    let ps = page_size();
    assert!(ps > 0);
    assert!(ps.is_power_of_two());
}

#[test]
fn registered_buffer_group_rejects_zero_count() {
    let Some(ring) = try_ring(4) else { return };
    let result = RegisteredBufferGroup::new(&ring, 4096, 0);
    assert!(result.is_err());
}

#[test]
fn registered_buffer_group_rejects_zero_size() {
    let Some(ring) = try_ring(4) else { return };
    let result = RegisteredBufferGroup::new(&ring, 0, 4);
    assert!(result.is_err());
}

#[test]
fn registered_buffer_group_rejects_excessive_count() {
    let Some(ring) = try_ring(4) else { return };
    let result = RegisteredBufferGroup::new(&ring, 4096, MAX_REGISTERED_BUFFERS + 1);
    assert!(result.is_err());
}

#[test]
fn registered_buffer_group_create_and_checkout() {
    let Some(ring) = try_ring(4) else { return };
    let Some(group) = try_group(&ring, 4096, 4) else {
        return;
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
    let Some(ring) = try_ring(4) else { return };
    let Some(group) = try_group(&ring, 4096, 2) else {
        return;
    };

    let mut slot = group.checkout().expect("checkout");

    // Write a pattern into the buffer.
    let pattern = b"hello io_uring registered buffers!";
    // SAFETY: the slot was just checked out, so we hold exclusive access; the
    // slot's capacity (4096 bytes) exceeds `pattern.len()`, and the destination
    // pointer is from the registered arena which is non-null and aligned.
    unsafe {
        ptr::copy_nonoverlapping(pattern.as_ptr(), slot.as_mut_ptr(), pattern.len());
        let read_back = slot.as_slice(pattern.len());
        assert_eq!(read_back, pattern);
    }

    drop(slot);
    let _ = group.unregister(&ring);
}
