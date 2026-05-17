//! Integration tests for `submit_read_fixed_batch` short-read handling.
//!
//! On NFS, FUSE, and slow block devices the kernel may return fewer bytes
//! than a `ReadFixed` SQE requested, even when the underlying file still has
//! more data. Earlier revisions of `submit_read_fixed_batch` advanced past
//! the short SQE unconditionally, silently zero-filling the tail. These
//! tests pin the post-fix behaviour from outside the crate boundary.
//!
//! Truly simulating an NFS server from a unit test is impractical, but the
//! function only sees CQEs - any path that causes a mid-batch CQE to return
//! `result < requested` exercises the same code. The simplest way to drive
//! that is to size the source file so it straddles SQE boundaries, which is
//! exactly what an NFS read returning at a wsize-boundary looks like to the
//! function under test.
//!
//! Skipped at runtime when the kernel does not support io_uring (`uname`
//! pre-5.6, seccomp-blocked containers) or when buffer registration is
//! rejected (some CI sandboxes deny `IORING_REGISTER_BUFFERS`).

#![cfg(all(target_os = "linux", feature = "io_uring"))]

use std::fs;
use std::os::unix::io::AsRawFd;

use fast_io::io_uring::__test_reexports::{Fd, RawIoUring};
use fast_io::io_uring::{RegisteredBufferGroup, RegisteredBufferSlotInfo, submit_read_fixed_batch};

/// Chunk size matching the most common kernel page size; large enough to
/// register but small enough that small files fit inside a single slot.
const CHUNK: usize = 4096;

/// Builds a ring + registered buffer group, returning `None` when either
/// step fails so the test can skip gracefully on unsupported kernels.
fn setup(ring_entries: u32, slot_count: usize) -> Option<(RawIoUring, RegisteredBufferGroup)> {
    let ring = RawIoUring::new(ring_entries).ok()?;
    let group = RegisteredBufferGroup::new(&ring, CHUNK, slot_count).ok()?;
    Some((ring, group))
}

/// Materialises a `Vec<RegisteredBufferSlotInfo>` from checked-out slots so
/// the batch helper sees the same pointer/index layout the production code
/// hands it. Drop order keeps the slots alive for the duration of the test.
fn slot_infos(
    checked_out: &mut [fast_io::io_uring::RegisteredBufferSlot<'_>],
) -> Vec<RegisteredBufferSlotInfo> {
    checked_out
        .iter_mut()
        .map(|s| RegisteredBufferSlotInfo {
            ptr: s.as_mut_ptr(),
            buf_index: s.buf_index(),
            buffer_size: s.buffer_size(),
        })
        .collect()
}

/// File size sits between SQE boundaries so the second SQE in a two-slot
/// batch returns a short read while the first returns the full chunk. This
/// is the canonical NFS-style mid-batch short read.
#[test]
fn mid_batch_short_read_does_not_zero_fill_tail() {
    let Some((mut ring, group)) = setup(8, 2) else {
        eprintln!("io_uring or buffer registration unavailable; skipping");
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nfs_like_short.bin");

    // 4096 + 333 forces SQE 0 to fully complete and SQE 1 to short-read.
    let payload: Vec<u8> = (0..CHUNK + 333).map(|i| (i % 251) as u8).collect();
    fs::write(&path, &payload).expect("write payload");

    let file = fs::File::open(&path).expect("open payload");
    let fd = Fd(file.as_raw_fd());

    let mut slots: Vec<_> = (0..2).filter_map(|_| group.checkout()).collect();
    assert_eq!(slots.len(), 2, "expected both slots to check out");
    let infos = slot_infos(&mut slots);

    // Request the full payload; the second SQE will short-read.
    let mut out = vec![0xAAu8; payload.len()];
    let n = submit_read_fixed_batch(&mut ring, fd, &mut out, 0, &infos, -1).expect("read ok");

    assert_eq!(n, payload.len(), "must report the actual file length");
    assert_eq!(
        &out[..n],
        &payload[..],
        "data must match the source byte-for-byte"
    );

    drop(slots);
    let _ = group.unregister(&ring);
}

/// Force multiple outer-loop iterations: file is several times the total
/// slot capacity. Each round risks a short read; the function must keep
/// resubmitting from the correct offset until everything is drained.
#[test]
fn multi_round_short_reads_drain_completely() {
    let Some((mut ring, group)) = setup(8, 2) else {
        eprintln!("io_uring or buffer registration unavailable; skipping");
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nfs_like_multiround.bin");

    // 5 full chunks + 777-byte tail: at least three outer-loop rounds with
    // only two slots. Tail size guarantees a short SQE in the last round.
    let total = 5 * CHUNK + 777;
    let payload: Vec<u8> = (0..total).map(|i| ((i * 7) % 253) as u8).collect();
    fs::write(&path, &payload).expect("write payload");

    let file = fs::File::open(&path).expect("open payload");
    let fd = Fd(file.as_raw_fd());

    let mut slots: Vec<_> = (0..2).filter_map(|_| group.checkout()).collect();
    assert_eq!(slots.len(), 2);
    let infos = slot_infos(&mut slots);

    let mut out = vec![0u8; total];
    let n = submit_read_fixed_batch(&mut ring, fd, &mut out, 0, &infos, -1).expect("read ok");

    assert_eq!(n, total);
    assert_eq!(out, payload, "every byte across all rounds must match");

    drop(slots);
    let _ = group.unregister(&ring);
}

/// EOF mid-read variant: caller asks for far more bytes than the file
/// contains. The first SQE returns the full file, every subsequent SQE
/// returns 0 (EOF). The function must report only the bytes actually read.
#[test]
fn eof_mid_read_returns_actual_bytes_only() {
    let Some((mut ring, group)) = setup(8, 4) else {
        eprintln!("io_uring or buffer registration unavailable; skipping");
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nfs_like_eof.bin");

    // Smaller than one chunk: SQE 0 short-reads to file size, SQEs 1-3 hit EOF.
    let payload = b"NFS-style short-read EOF marker payload".to_vec();
    fs::write(&path, &payload).expect("write payload");

    let file = fs::File::open(&path).expect("open payload");
    let fd = Fd(file.as_raw_fd());

    let mut slots: Vec<_> = (0..4).filter_map(|_| group.checkout()).collect();
    assert_eq!(slots.len(), 4);
    let infos = slot_infos(&mut slots);

    // Ask for four full chunks; only payload.len() bytes are available.
    let request = 4 * CHUNK;
    let mut out = vec![0xCDu8; request];
    let n = submit_read_fixed_batch(&mut ring, fd, &mut out, 0, &infos, -1).expect("read ok");

    assert_eq!(
        n,
        payload.len(),
        "must report actual file size on EOF mid-read"
    );
    assert_eq!(&out[..n], &payload[..]);
    // Bytes beyond the reported length must remain untouched - the function
    // must not zero-fill or otherwise mutate the tail of the caller buffer.
    assert!(
        out[n..].iter().all(|b| *b == 0xCD),
        "tail beyond reported bytes must be untouched"
    );

    drop(slots);
    let _ = group.unregister(&ring);
}

/// The function's contract: the reported byte count never exceeds the
/// caller's output capacity. Backed by a `debug_assert!` inside the helper
/// (see "submit_read_fixed_batch invariant" comment in registered_buffers.rs).
/// Reading a non-empty payload into an exact-size buffer must succeed and
/// honour the bound on every iteration path.
#[test]
fn reported_length_never_exceeds_output_capacity() {
    let Some((mut ring, group)) = setup(8, 2) else {
        eprintln!("io_uring or buffer registration unavailable; skipping");
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nfs_like_exact.bin");

    // Exact two-chunk file: no short reads, but exercises the success path
    // where total_read could meet (and must not exceed) output.len().
    let payload: Vec<u8> = (0..2 * CHUNK).map(|i| (i % 199) as u8).collect();
    fs::write(&path, &payload).expect("write payload");

    let file = fs::File::open(&path).expect("open payload");
    let fd = Fd(file.as_raw_fd());

    let mut slots: Vec<_> = (0..2).filter_map(|_| group.checkout()).collect();
    assert_eq!(slots.len(), 2);
    let infos = slot_infos(&mut slots);

    let mut out = vec![0u8; payload.len()];
    let n = submit_read_fixed_batch(&mut ring, fd, &mut out, 0, &infos, -1).expect("read ok");

    assert!(
        n <= out.len(),
        "invariant: reported bytes must fit the output"
    );
    assert_eq!(n, payload.len());
    assert_eq!(out, payload);

    drop(slots);
    let _ = group.unregister(&ring);
}
