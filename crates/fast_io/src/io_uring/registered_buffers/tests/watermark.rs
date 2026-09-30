//! Watermark resize and arena tests. After every resize the kernel table
//! must match the group's bookkeeping: a lease reads through every slot, and
//! a slot the kernel lacks would fail `READ_FIXED` with `EFAULT`.

use std::io::{self, Write};
use std::os::unix::io::AsRawFd;
use std::rc::Rc;

use io_uring::IoUring as RawIoUring;
use io_uring::types::Fd;

use super::super::lease::read_fixed_lease;
use super::super::registry::RegisteredBufferGroup;
use super::super::{ArenaFile, ArenaRing};
use super::try_ring;

const SLOT: usize = 4096;

fn pattern(len: usize, seed: usize) -> Vec<u8> {
    (0..len).map(|i| ((i * 31 + seed) % 251) as u8).collect()
}

fn file_with(data: &[u8]) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().expect("tempfile");
    f.write_all(data).expect("write");
    f
}

/// Registration refused by the host (`RLIMIT_MEMLOCK`, seccomp) is a skip.
fn or_skip<T>(result: io::Result<T>) -> Option<T> {
    result
        .inspect_err(|e| eprintln!("skipping: registration refused: {e}"))
        .ok()
}

/// Leases the whole group and checks every slot came back with file bytes.
fn assert_every_slot_reads(
    ring: &mut RawIoUring,
    group: &Rc<RegisteredBufferGroup>,
    file: &tempfile::NamedTempFile,
    data: &[u8],
) {
    let fd = Fd(file.as_file().as_raw_fd());
    let lease = read_fixed_lease(ring, group, fd, 0, data.len()).expect("READ_FIXED lease");
    assert_eq!(lease.chunks().count(), group.count(), "one chunk per slot");
    let got: Vec<u8> = lease.chunks().flatten().copied().collect();
    assert_eq!(got.len(), group.count() * SLOT);
    assert_eq!(got, data[..got.len()]);
}

fn resize_and_verify(mut ring: RawIoUring, group: RegisteredBufferGroup, calls_per_resize: u64) {
    let data = pattern(16 * SLOT, 0);
    let file = file_with(&data);
    let mut group = Rc::new(group);
    for count in [9, 16, 1, 5, 2] {
        let before = group.register_calls();
        Rc::get_mut(&mut group)
            .expect("no lease outstanding")
            .resize(&ring, count)
            .expect("resize");
        assert_eq!(group.count(), count);
        assert_eq!(group.registered_bytes(), count * SLOT);
        assert_eq!(group.register_calls() - before, calls_per_resize);
        assert_every_slot_reads(&mut ring, &group, &file, &data);
    }
}

#[test]
fn watermark_sparse_resize_updates_only_changed_slots() {
    let Some(ring) = try_ring(64) else { return };
    let Some(group) = or_skip(RegisteredBufferGroup::new_sparse(&ring, SLOT, 16, 2)) else {
        return;
    };
    resize_and_verify(ring, group, 1);
}

#[test]
fn watermark_dense_resize_reregisters_the_table() {
    let Some(ring) = try_ring(64) else { return };
    let Some(group) = or_skip(RegisteredBufferGroup::new(&ring, SLOT, 2)) else {
        return;
    };
    resize_and_verify(ring, group, 2);
}

#[test]
fn watermark_resize_refuses_checked_out_slots_and_bad_counts() {
    let Some(ring) = try_ring(8) else { return };
    let Some(mut group) = or_skip(RegisteredBufferGroup::new_sparse(&ring, SLOT, 4, 2)) else {
        return;
    };
    let index = group.checkout_detached().expect("slot");
    let err = group.resize(&ring, 3).expect_err("slot in use");
    assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
    group.return_slot(index);
    for bad in [0, 5] {
        let err = group.resize(&ring, bad).expect_err("out of range");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
    group.resize(&ring, 3).expect("resize once slots are back");
    assert_eq!(group.available(), 3);
}

/// A pool larger than the submission queue must be read in rounds rather
/// than failing with "submission queue full".
#[test]
fn lease_spans_more_slots_than_the_submission_queue() {
    let Some(mut ring) = try_ring(4) else { return };
    let Some(group) = or_skip(RegisteredBufferGroup::new(&ring, SLOT, 16)).map(Rc::new) else {
        return;
    };
    assert!((ring.params().sq_entries() as usize) < group.count());
    let data = pattern(16 * SLOT, 3);
    let file = file_with(&data);
    assert_every_slot_reads(&mut ring, &group, &file, &data);
}

fn read_through_arena(ring: &mut ArenaRing, file: &ArenaFile, len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let lease = ring.read_lease(file, out.len() as u64, len).expect("lease");
        if lease.is_empty() {
            return out;
        }
        out.extend_from_slice(lease);
    }
}

#[test]
fn arena_reads_sub_ranges_of_one_registration() {
    let Some(mut ring) = or_skip(ArenaRing::new(8 * SLOT, SLOT, 4, false)) else {
        return;
    };
    // Longer than the arena and not a chunk multiple: several leases, a
    // short tail, and more chunks per lease than SQ entries.
    let data = pattern(20 * SLOT + 123, 5);
    let file = file_with(&data);
    let handle = ring.open(file.path()).expect("open");
    assert_eq!(read_through_arena(&mut ring, &handle, usize::MAX), data);
    let stats = ring.stats();
    assert_eq!(stats.register_calls, 1);
    assert!(stats.read_fixed_sqes >= 21, "{stats:?}");
}

#[test]
fn arena_direct_open_replaces_the_fixed_file_slot() {
    let Some(mut ring) = or_skip(ArenaRing::new(4 * SLOT, SLOT, 8, true)) else {
        return;
    };
    let first = pattern(3 * SLOT, 1);
    let second = pattern(2 * SLOT + 9, 2);
    for data in [&first, &second] {
        let file = file_with(data);
        let handle = ring.open(file.path()).expect("direct open");
        assert!(matches!(handle, ArenaFile::Direct));
        assert_eq!(&read_through_arena(&mut ring, &handle, data.len()), data);
    }
    let stats = ring.stats();
    assert_eq!(stats.register_calls, 2);
    assert_eq!(stats.direct_opens, 2);
}
