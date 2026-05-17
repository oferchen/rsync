//! Integration test for io_uring + mmap interaction under page-cache pressure.
//!
//! Issue #1664. The intent is to demonstrate (not measure) that the io_uring
//! submission path tolerates concurrent page-cache eviction of the underlying
//! file: even after `MADV_DONTNEED` drops every resident page, fresh `READ`
//! SQEs against the same file must complete without panic, hang, or
//! submission error.
//!
//! Scenario:
//!
//! 1. Create a 64 MiB temp file with a deterministic byte pattern.
//! 2. Map it read-only into the test process so that page-cache state is
//!    observable through the mapping.
//! 3. Open a second fd to the same file and drive a [`SharedRing`] against
//!    it, submitting four `IORING_OP_READ` SQEs at distinct page offsets.
//!    Verify every CQE returns the requested length and the bytes match the
//!    pattern.
//! 4. Apply `MADV_DONTNEED` to the entire mapping to force the kernel to
//!    drop every resident page, then re-submit the same set of reads. The
//!    second batch must complete without io_uring submission errors and
//!    must read the correct bytes - the kernel page-fault handler is
//!    expected to re-page the file under the io_uring worker.
//!
//! Skipped at runtime when [`is_io_uring_available`] returns `false` so the
//! suite stays green on kernels < 5.6 or inside seccomp-filtered containers.

#![cfg(all(target_os = "linux", feature = "io_uring"))]

use std::os::unix::io::AsRawFd;

use fast_io::{SharedCompletion, SharedRing, SharedRingConfig, is_io_uring_available};
use memmap2::MmapOptions;
use tempfile::tempdir;

/// 64 MiB matches the size called out in issue #1664; large enough to span
/// many page-cache entries while staying well below typical CI tmpfs limits.
const FILE_SIZE: usize = 64 * 1024 * 1024;

/// Page granularity assumed by `MADV_DONTNEED` and by the read offsets used
/// in this test. Linux x86_64, aarch64, and ppc64le all have a 4 KiB or
/// larger base page; all chosen offsets are 4 KiB-aligned so the test does
/// not rely on a specific page size.
const PAGE_SIZE: usize = 4096;

/// Number of read SQEs submitted per pass. Four is enough to exercise
/// completion demuxing without saturating the default 256-entry submission
/// queue.
const READ_COUNT: usize = 4;

/// Read length per SQE. One page keeps the kernel's per-op work small while
/// still requiring the page-fault path under MADV_DONTNEED.
const READ_LEN: usize = PAGE_SIZE;

#[test]
fn io_uring_reads_tolerate_mmap_madv_dontneed() {
    if !is_io_uring_available() {
        eprintln!("skipping mmap pressure test: io_uring unavailable");
        return;
    }

    // Deterministic payload: byte at offset i is (i % 251) so every page
    // looks distinct from its neighbours.
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("mmap_pressure.bin");
    let payload: Vec<u8> = (0..FILE_SIZE).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &payload).expect("write payload");

    // Read-only mapping for observation and for the MADV_DONTNEED stress.
    // Mapping is unsafe because the file's contents may be mutated by other
    // processes during the lifetime of the map; the test owns the file
    // exclusively, so the mapping is sound.
    let map_file = std::fs::File::open(&path).expect("open for mmap");
    // SAFETY: the test owns the file exclusively for its lifetime; no other
    // process mutates the contents during the mapping, so the kernel's view
    // remains stable as required by `Mmap`.
    let mmap = unsafe {
        MmapOptions::new()
            .len(FILE_SIZE)
            .map(&map_file)
            .expect("mmap 64 MiB")
    };
    assert_eq!(mmap.len(), FILE_SIZE);

    // Touch the first byte of each chosen page to ensure the pages are
    // resident before the first io_uring pass. This is the realistic
    // scenario - the application has already faulted these pages in, then
    // the kernel later evicts them under memory pressure.
    let offsets: [u64; READ_COUNT] = [
        0,
        (FILE_SIZE / 4) as u64,
        (FILE_SIZE / 2) as u64,
        (FILE_SIZE - READ_LEN) as u64,
    ];
    for &off in &offsets {
        // Read one byte through the mapping. The read forces the page in.
        let _ = std::hint::black_box(mmap[off as usize]);
    }

    // Separate fd for io_uring. Kept distinct from the mapping fd so that
    // dropping the mapping at the end of the test cannot close the ring's
    // registered fd.
    let ring_file = std::fs::File::open(&path).expect("open for io_uring");

    // Writer fd is unused but `SharedRing` requires a writer; a pipe is the
    // cheapest fd that supports POLL_ADD. The writer side is never polled
    // here.
    let (_pipe_r, pipe_w) = pipe();

    let cfg = SharedRingConfig::default();
    let mut ring = match SharedRing::try_new(ring_file.as_raw_fd(), pipe_w.as_raw_fd(), &cfg) {
        Some(r) => r,
        None => {
            eprintln!("skipping mmap pressure test: SharedRing::try_new returned None");
            return;
        }
    };

    // ---- Pass 1: pages resident -------------------------------------
    let mut bufs_pass1: Vec<Vec<u8>> = (0..READ_COUNT).map(|_| vec![0u8; READ_LEN]).collect();
    submit_and_verify_reads(&mut ring, &offsets, &mut bufs_pass1, &payload, "pass-1");

    // ---- Drop every resident page ----------------------------------
    // Direct `libc::madvise(MADV_DONTNEED)` call: the advice is best-effort,
    // and the assertion below only requires that subsequent io_uring reads
    // still succeed, regardless of how many pages the kernel actually
    // evicted. Going through libc keeps the test independent of any
    // specific memmap2 `Advice` variant naming.
    {
        // `madvise` takes a non-const pointer, but the kernel only inspects
        // page-cache state - the mapping bytes themselves are not written.
        let ptr = mmap.as_ptr().cast::<libc::c_void>().cast_mut();
        // SAFETY: `mmap.as_ptr()` returns the start of a live mapping of
        // FILE_SIZE bytes; `madvise` does not retain the pointer beyond the
        // call and only signals page-cache state to the kernel.
        let rc = unsafe { libc::madvise(ptr, FILE_SIZE, libc::MADV_DONTNEED) };
        assert_eq!(
            rc,
            0,
            "MADV_DONTNEED on 64 MiB mapping failed: {}",
            std::io::Error::last_os_error()
        );
    }

    // ---- Pass 2: pages evicted, page-fault path under io_uring ------
    let mut bufs_pass2: Vec<Vec<u8>> = (0..READ_COUNT).map(|_| vec![0u8; READ_LEN]).collect();
    submit_and_verify_reads(
        &mut ring,
        &offsets,
        &mut bufs_pass2,
        &payload,
        "pass-2-after-dontneed",
    );

    // Hold the mapping until the second pass returns so that the mmap
    // VMA is alive during io_uring completion. Dropping it earlier would
    // remove the test's stress source between submission and reap.
    drop(mmap);
}

/// Submits one `IORING_OP_READ` per `(offset, buf)` pair, drives the ring to
/// completion, and verifies the returned bytes against `payload`.
fn submit_and_verify_reads(
    ring: &mut SharedRing,
    offsets: &[u64],
    bufs: &mut [Vec<u8>],
    payload: &[u8],
    label: &str,
) {
    assert_eq!(
        offsets.len(),
        bufs.len(),
        "{label}: offset/buf count mismatch"
    );

    // Distinct op_id per SQE so the demux can tell them apart.
    for (i, buf) in bufs.iter_mut().enumerate() {
        ring.submit_read(0xA000 + i as u64, offsets[i], buf)
            .unwrap_or_else(|e| panic!("{label}: submit_read[{i}] failed: {e}"));
    }

    let target = bufs.len();
    let mut received = 0usize;
    let mut seen = vec![false; target];
    while received < target {
        let n = ring
            .submit_and_wait(1)
            .unwrap_or_else(|e| panic!("{label}: submit_and_wait failed: {e}"));
        assert!(n > 0, "{label}: submit_and_wait returned 0");

        let completions = ring
            .reap()
            .unwrap_or_else(|e| panic!("{label}: reap failed: {e}"));
        for c in completions {
            match c {
                SharedCompletion::Read { op_id, result } => {
                    let idx = (op_id - 0xA000) as usize;
                    assert!(idx < target, "{label}: stray op_id {op_id:#x}");
                    assert!(
                        !seen[idx],
                        "{label}: duplicate completion for op_id {op_id:#x}"
                    );
                    seen[idx] = true;
                    assert!(
                        result >= 0,
                        "{label}: read[{idx}] returned negative errno {result}"
                    );
                    assert_eq!(
                        result as usize, READ_LEN,
                        "{label}: short read at offset {}",
                        offsets[idx]
                    );
                    received += 1;
                }
                other => panic!("{label}: unexpected completion {other:?}"),
            }
        }
    }

    // Bytes must match the deterministic pattern even after page eviction.
    for (i, buf) in bufs.iter().enumerate() {
        let off = offsets[i] as usize;
        assert_eq!(
            &buf[..],
            &payload[off..off + READ_LEN],
            "{label}: payload mismatch at offset {off}"
        );
    }
}

/// Creates an anonymous pipe and returns the (read, write) ends as `File`s
/// so they participate in normal `Drop` cleanup. The write end is registered
/// as the writer fd of the [`SharedRing`]; the read end is held alive only
/// to keep the kernel from delivering EPIPE to a stray POLL submission.
fn pipe() -> (std::fs::File, std::fs::File) {
    use std::os::unix::io::FromRawFd;
    let mut fds = [0i32; 2];
    // SAFETY: `fds` is a valid two-element buffer; libc::pipe writes both
    // file descriptors before returning. The fds are wrapped in `File`
    // immediately so the kernel handle ownership is recorded.
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(
        rc,
        0,
        "libc::pipe failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: `pipe` returned 0, so both fds are valid kernel handles owned
    // by this process; wrapping them in `File` transfers ownership safely.
    let r = unsafe { std::fs::File::from_raw_fd(fds[0]) };
    let w = unsafe { std::fs::File::from_raw_fd(fds[1]) };
    (r, w)
}
