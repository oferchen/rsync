//! Integration tests for the `ZeroCopySender` zero-copy socket sender.
//!
//! Sends 1 MiB of pseudo-random bytes over a TCP loopback pair and asserts
//! that the receive end observes the exact same bytes, and drives the
//! daemon's socket writer under the default `Auto` policy (never SEND_ZC)
//! and under an explicit `--zero-copy`. The tests run only on Linux
//! with both the `io_uring` and `iouring-send-zc` cargo features enabled and
//! state on stderr why they skip when the running kernel does not advertise
//! `IORING_OP_SEND_ZC` (kernel < 6.0, seccomp-restricted environments,
//! container runtimes that block the opcode).
//!
//! The kernel refuses a SEND_ZC with `ENOMEM` when the user's locked-memory
//! budget (`RLIMIT_MEMLOCK`, shared by every process of that user) cannot
//! cover the pinned pages, so whether a send goes zero-copy depends on what
//! else the runner is doing. The tests below therefore accept either a SEND_ZC
//! or the counted copying send that replaces a refused one, and two of them
//! empty the budget on purpose so the copying send is what runs.

#![cfg(all(target_os = "linux", feature = "iouring-send-zc"))]

use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::io::{AsRawFd, OwnedFd, RawFd};
use std::thread;
use std::time::Duration;

// `FromRawFd` is required by the OwnedFd reconstruction inside
// `socketpair_unix_stream`.

use fast_io::ZeroCopySender;
use fast_io::io_uring::{is_io_uring_available, send_zc};

#[path = "../src/io_uring/send_zc_test_env.rs"]
mod send_zc_test_env;

use send_zc_test_env::{
    SendZcEnv, assert_setup_refused_by_memlock, assert_writer_ring_refused, classify, report,
};

/// Classifies the environment for `test` on stderr; `true` when the real
/// opcode can be exercised.
fn send_zc_runnable(test: &str) -> bool {
    report(
        test,
        classify(is_io_uring_available(), send_zc::is_supported()),
    ) == SendZcEnv::Ok
}

/// Wraps `socketpair(AF_UNIX, SOCK_STREAM, 0)` and returns the two
/// endpoints as owned file descriptors. The OwnedFd Drop impl closes the
/// fd on scope exit so the test never leaks descriptors even on a
/// short-circuited skip.
fn socketpair_unix_stream() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds: [RawFd; 2] = [-1, -1];
    // SAFETY: `fds` is a stack-allocated `[RawFd; 2]`; on success libc
    // writes two valid file descriptors that we immediately wrap in
    // `OwnedFd` for RAII. No aliasing is possible because nothing else
    // observes `fds` before the wrap.
    let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: socketpair returned 0 above, so both entries in `fds` are
    // valid open descriptors that we are taking exclusive ownership of.
    let a = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    // SAFETY: same as above for the second descriptor.
    let b = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    Ok((a, b))
}

#[test]
fn zero_copy_sender_roundtrips_1mib_over_tcp_loopback() {
    const TEST: &str = "zero_copy_sender_roundtrips_1mib_over_tcp_loopback";
    if !send_zc_runnable(TEST) {
        return;
    }

    // SEND_ZC's notification path is wired for TCP/UDP, not AF_UNIX (a
    // socketpair answers EOPNOTSUPP), so the round-trip needs a TCP pair.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let send_fd = OwnedFd::from(
        std::net::TcpStream::connect(listener.local_addr().expect("loopback addr"))
            .expect("connect"),
    );
    let (mut recv_stream, _) = listener.accept().expect("accept");

    // Generate a deterministic 1 MiB pseudo-random payload. We avoid a
    // crate dependency on `rand` for this single byte-fill by using a
    // tiny linear-congruential generator seeded from a fixed value so
    // failures are reproducible.
    let payload: Vec<u8> = {
        let mut state: u64 = 0x1234_5678_9abc_def0;
        (0..1024 * 1024)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 33) as u8
            })
            .collect()
    };

    // Spawn the receive thread before we start sending so the kernel
    // socket buffer cannot fill and block the sender.
    let expected_len = payload.len();
    let reader = thread::spawn(move || {
        recv_stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("set_read_timeout");
        let mut received: Vec<u8> = Vec::with_capacity(expected_len);
        let mut scratch = [0u8; 32 * 1024];
        while received.len() < expected_len {
            match recv_stream.read(&mut scratch) {
                Ok(0) => break,
                Ok(n) => received.extend_from_slice(&scratch[..n]),
                Err(e) => panic!("receive failed: {e}"),
            }
        }
        received
    });

    let mut sender = match ZeroCopySender::new(send_fd.as_raw_fd()) {
        Ok(s) => s,
        Err(e) => {
            assert_setup_refused_by_memlock(TEST, "ZeroCopySender::new", &e);
            deliver_through_plain_send(TEST, send_fd.into(), &payload, reader);
            return;
        }
    };

    let before = fast_io::send_zc_stats();
    send_all(&mut sender, &payload);
    let after = fast_io::send_zc_stats();

    // Drop the send endpoint so the read thread observes EOF after
    // draining the last byte; this collapses the receive loop cleanly.
    drop(sender);
    drop(send_fd);

    let received = reader.join().expect("reader thread did not panic");
    assert_eq!(
        received.len(),
        payload.len(),
        "received byte count must match sent count"
    );
    assert_eq!(received, payload, "round-tripped bytes must match");
    assert_bytes_went_through_send_zc(TEST, before, after, payload.len() as u64, 0);
}

/// The caller's answer to a `ZeroCopySender::new` the locked-memory budget
/// refused: send the payload through the plain socket, as the socket-writer
/// factory does when it cannot build its ring, and assert the peer received
/// it byte for byte.
fn deliver_through_plain_send(
    test: &str,
    mut stream: std::net::TcpStream,
    payload: &[u8],
    reader: thread::JoinHandle<Vec<u8>>,
) {
    stream.write_all(payload).expect("plain send");
    drop(stream);
    assert_eq!(
        reader.join().expect("reader thread"),
        payload,
        "{test}: the plain-send fallback must deliver the payload byte for byte"
    );
    eprintln!("[send_zc env] {test}: asserted the plain-send fallback delivered the payload");
}

/// Returns `true` when this thread holds `CAP_IPC_LOCK` in its effective set.
fn holds_ipc_lock() -> bool {
    rustix::thread::capabilities(None)
        .expect("read this thread's capabilities")
        .effective
        .contains(rustix::thread::CapabilitySet::IPC_LOCK)
}

/// Asserts how the `len` payload bytes were sent, given the counter snapshots
/// taken around the send. `plain_tail` is the number of trailing bytes the
/// sender may put on a plain send by design (a writer's final flush below the
/// SEND_ZC dispatch floor); zero for a caller that only sends through SEND_ZC.
///
/// A thread holding `CAP_IPC_LOCK` creates rings the kernel never charges for
/// pinned pages (Linux `io_uring/io_uring.c` `io_uring_create` sets
/// `ctx->user` only without the capability), so its SEND_ZC cannot be refused
/// for memory: every byte must go zero-copy and none may be copied. That is
/// the root CI job, and it is what catches a regression that always falls
/// back. Without the capability the pages are charged to the user's
/// `RLIMIT_MEMLOCK` budget, which other processes share, so a refused SEND_ZC
/// is legitimately resent as a copying send and only the sum is fixed. The
/// branch taken is printed so a run states which assertion it made.
fn assert_bytes_went_through_send_zc(
    test: &str,
    before: fast_io::SendZcStats,
    after: fast_io::SendZcStats,
    len: u64,
    plain_tail: u64,
) {
    let zero_copy = after.bytes - before.bytes;
    let copied = after.copied_bytes - before.copied_bytes;
    if holds_ipc_lock() {
        eprintln!(
            "[send_zc env] {test}: CAP_IPC_LOCK held - pinned pages are never charged, \
             asserting every byte went zero-copy"
        );
        assert_eq!(
            copied, 0,
            "with CAP_IPC_LOCK no SEND_ZC can be refused for memory, so nothing may be \
             copied: {before:?} -> {after:?}"
        );
        assert!(
            zero_copy <= len && zero_copy + plain_tail >= len,
            "with CAP_IPC_LOCK the payload must travel through SEND_ZC: {before:?} -> {after:?}"
        );
    } else {
        eprintln!(
            "[send_zc env] {test}: no CAP_IPC_LOCK - pinned pages are charged to \
             RLIMIT_MEMLOCK, asserting zero-copy + copied bytes cover the payload"
        );
        let counted = zero_copy + copied;
        assert!(
            counted <= len && counted + plain_tail >= len,
            "every byte must be counted as a SEND_ZC or a copying send: {before:?} -> {after:?}"
        );
    }
}

/// Sends all of `payload` through `sender`. The environment probe said
/// SEND_ZC runs here, so any error is a real failure: running out of
/// pinnable memory is not one, because the sender answers it with a copying
/// send.
fn send_all(sender: &mut ZeroCopySender, payload: &[u8]) {
    let mut sent_total = 0usize;
    while sent_total < payload.len() {
        let n = sender
            .send_zc(&payload[sent_total..])
            .unwrap_or_else(|e| panic!("send failed on a SEND_ZC-capable host: {e}"));
        assert!(n > 0, "kernel reported zero bytes sent");
        sent_total += n;
    }
}

/// Makes the kernel charge this thread's rings for the pages SEND_ZC pins.
///
/// The charge applies only to rings created by a thread without
/// `CAP_IPC_LOCK` (Linux `io_uring/io_uring.c` `io_uring_create` sets
/// `ctx->user` only then), so the capability is dropped from this thread's
/// effective set - which matters in the root CI job. Call it before creating
/// any ring the test sends through.
fn charge_this_thread_for_pinned_pages() {
    use rustix::thread::{CapabilitySet, capabilities, set_capabilities};

    let mut caps = capabilities(None).expect("read this thread's capabilities");
    caps.effective.remove(CapabilitySet::IPC_LOCK);
    set_capabilities(None, caps).expect("clearing an effective capability is always permitted");
}

/// Raises the soft `RLIMIT_MEMLOCK` as far as this process may, for creating
/// rings. Returns `true` when the budget is now unlimited.
///
/// Some kernels charge a ring's own pages to the same budget when it is
/// created (v6.17 `io_uring/memmap.c` `io_create_region`; newer kernels pass
/// no ctx for the ring regions and do not). Creating the rings before the
/// budget is spent keeps setup out of the refusal the test is about. With
/// `CAP_SYS_RESOURCE` (root) the limit becomes unlimited and setup cannot be
/// refused for memory at all; without it the soft limit rises to the hard one.
fn open_pinned_page_budget() -> bool {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

    let unlimited = Rlimit {
        current: None,
        maximum: None,
    };
    if setrlimit(Resource::Memlock, unlimited).is_ok() {
        return true;
    }
    let hard = getrlimit(Resource::Memlock).maximum;
    setrlimit(
        Resource::Memlock,
        Rlimit {
            current: hard,
            maximum: hard,
        },
    )
    .expect("raising the soft RLIMIT_MEMLOCK to the hard limit is always permitted");
    hard.is_none()
}

/// Spends the budget: sets the soft `RLIMIT_MEMLOCK` to zero.
///
/// Rings created earlier keep working, but every SEND_ZC through a ring this
/// thread created after [`charge_this_thread_for_pinned_pages`] is now refused
/// with `ENOMEM` (`io_uring/net.c` `io_send_zc_import` ->
/// `io_uring/notif.h` `io_notif_account_mem` -> `io_uring/rsrc.c`
/// `__io_account_mem`), whatever the runner's real budget.
///
/// nextest runs each test in its own process, so the lowered limit cannot
/// reach another test.
fn exhaust_pinned_page_budget() {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

    let hard = getrlimit(Resource::Memlock).maximum;
    setrlimit(
        Resource::Memlock,
        Rlimit {
            current: Some(0),
            maximum: hard,
        },
    )
    .expect("lowering the soft RLIMIT_MEMLOCK is always permitted");
}

/// Handles a setup step that failed before the budget was spent.
///
/// With an unlimited budget nothing in setup can be refused for memory, so a
/// failure is a real failure and panics - that is the root CI job, which must
/// always take the full arm. Without one the user's budget is shared with
/// every other process of the user, so the ring may be refused with `ENOMEM`;
/// that refusal, and only that one, is asserted as its own arm.
fn setup_failed(test: &str, step: &str, err: &std::io::Error, unlimited: bool) {
    assert!(
        !unlimited,
        "{test}: {step} failed with an unlimited RLIMIT_MEMLOCK: {err}"
    );
    assert_setup_refused_by_memlock(test, step, err);
}

/// With no pinnable memory the kernel refuses every SEND_ZC with `ENOMEM`
/// (the nightly musl failure under a full parallel nextest run). The sender
/// must deliver the payload anyway, through copying sends, and say so in the
/// counters - zero copy is an optimisation, never a reason to fail a transfer.
#[test]
fn zero_copy_sender_copies_what_the_kernel_cannot_pin() {
    const TEST: &str = "zero_copy_sender_copies_what_the_kernel_cannot_pin";
    if !send_zc_runnable(TEST) {
        return;
    }
    charge_this_thread_for_pinned_pages();
    let unlimited = open_pinned_page_budget();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let send_stream = std::net::TcpStream::connect(listener.local_addr().expect("loopback addr"))
        .expect("connect");
    let (mut recv_stream, _) = listener.accept().expect("accept");
    let payload: Vec<u8> = (0..1024 * 1024).map(|i| (i % 253) as u8).collect();
    let expected_len = payload.len();
    let reader = thread::spawn(move || {
        recv_stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("set_read_timeout");
        let mut received = Vec::with_capacity(expected_len);
        recv_stream.read_to_end(&mut received).expect("read to end");
        received
    });

    let mut sender = match ZeroCopySender::new(send_stream.as_raw_fd()) {
        Ok(s) => s,
        Err(e) => {
            setup_failed(TEST, "ZeroCopySender::new", &e, unlimited);
            deliver_through_plain_send(TEST, send_stream, &payload, reader);
            return;
        }
    };
    eprintln!(
        "[send_zc env] {TEST}: ring built - asserting every SEND_ZC refused after the \
         budget is spent is resent as a copying send"
    );
    exhaust_pinned_page_budget();
    let before = fast_io::send_zc_stats();
    send_all(&mut sender, &payload);
    let after = fast_io::send_zc_stats();
    drop(sender);
    drop(send_stream);

    assert_eq!(
        reader.join().expect("reader thread"),
        payload,
        "the copying sends must deliver the payload byte for byte"
    );
    assert_eq!(
        after.bytes, before.bytes,
        "no byte can go zero-copy without pinnable memory: {before:?} -> {after:?}"
    );
    assert_eq!(
        after.copied_bytes - before.copied_bytes,
        payload.len() as u64,
        "every byte must be counted as a copying send: {before:?} -> {after:?}"
    );
}

#[test]
fn zero_copy_sender_rejects_empty_buffer() {
    const TEST: &str = "zero_copy_sender_rejects_empty_buffer";
    if !send_zc_runnable(TEST) {
        return;
    }
    let (send_fd, _recv_fd) = match socketpair_unix_stream() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("[send_zc env] SKIP {TEST}: socketpair() unavailable ({e})");
            return;
        }
    };
    let mut sender = match ZeroCopySender::new(send_fd.as_raw_fd()) {
        Ok(s) => s,
        Err(e) => {
            assert_setup_refused_by_memlock(TEST, "ZeroCopySender::new", &e);
            return;
        }
    };
    let err = sender.send_zc(&[]).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn zero_copy_sender_reports_raw_fd() {
    const TEST: &str = "zero_copy_sender_reports_raw_fd";
    if !send_zc_runnable(TEST) {
        return;
    }
    let (send_fd, _recv_fd) = match socketpair_unix_stream() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("[send_zc env] SKIP {TEST}: socketpair() unavailable ({e})");
            return;
        }
    };
    let raw = send_fd.as_raw_fd();
    let sender = match ZeroCopySender::new(raw) {
        Ok(s) => s,
        Err(e) => {
            assert_setup_refused_by_memlock(TEST, "ZeroCopySender::new", &e);
            return;
        }
    };
    assert_eq!(sender.raw_fd(), raw);
    assert!(sender.slot_bytes() >= 4 * 1024);
}

/// Drives `socket_writer_from_fd_zero_copy` under `policy` over a real TCP
/// loopback pair with a 2 MiB payload, written as `write_size`-byte calls.
/// Asserts the peer received the exact bytes and returns whether the writer
/// claimed SEND_ZC plus the counter snapshots taken around the write.
///
/// A `write_size` below the writer's 64 KiB buffer sends through the buffered
/// flush (the SEND_ZC pipeline); a larger one through the synchronous
/// `try_send_zc` on the caller's slice.
fn loopback_write(
    policy: fast_io::ZeroCopyPolicy,
    write_size: usize,
) -> (bool, fast_io::SendZcStats, fast_io::SendZcStats, usize) {
    use std::net::{TcpListener, TcpStream};

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("loopback addr");
    let payload: Vec<u8> = (0..2 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let expected_len = payload.len();
    let reader = thread::spawn(move || {
        let (mut peer, _) = listener.accept().expect("accept");
        peer.set_read_timeout(Some(Duration::from_secs(30)))
            .expect("read timeout");
        let mut received = Vec::with_capacity(expected_len);
        peer.read_to_end(&mut received).expect("read to end");
        received
    });

    let sender = TcpStream::connect(addr).expect("connect");
    let before = fast_io::send_zc_stats();
    let mut writer =
        fast_io::socket_writer_from_fd_zero_copy(sender.as_raw_fd(), 64 * 1024, policy)
            .expect("factory never errors");
    let engaged = matches!(
        writer,
        fast_io::IoUringOrStdSocketWriter::IoUring(ref w) if w.send_zc_active()
    );
    for chunk in payload.chunks(write_size) {
        writer.write_all(chunk).expect("write payload");
    }
    writer.flush().expect("flush");
    drop(writer);
    drop(sender);
    let after = fast_io::send_zc_stats();

    let received = reader.join().expect("reader thread");
    assert_eq!(
        received, payload,
        "loopback transcript must equal the payload"
    );
    (engaged, before, after, expected_len)
}

/// The default daemon-sender path: no `--zero-copy`, so the `Auto` policy.
///
/// SEND_ZC is compiled in but runtime opt-in, because default-on measured
/// +44% wall time on a 2 GiB daemon pull. Even on a kernel that advertises
/// the opcode the default writer must stay on plain sends: no SEND_ZC
/// completion and no fallback (none was requested) may be counted.
#[test]
fn default_auto_writer_never_sends_through_send_zc() {
    const TEST: &str = "default_auto_writer_never_sends_through_send_zc";
    send_zc_runnable(TEST);
    let (engaged, before, after, _) = loopback_write(fast_io::ZeroCopyPolicy::Auto, usize::MAX);
    assert!(!engaged, "the default Auto policy must not engage SEND_ZC");
    assert_eq!(
        after, before,
        "the default path must leave every SEND_ZC counter untouched"
    );
}

/// An explicit `--zero-copy` (`Enabled`) over a real TCP loopback pair.
///
/// Where SEND_ZC can run, the writer must engage it and the process counters
/// must show the payload went through `IORING_OP_SEND_ZC` - a writer that
/// silently fell back to plain sends would pass a byte comparison alone. A
/// SEND_ZC the kernel refused for lack of pinnable memory is resent as a
/// counted copying send, so those count too: they exist only because SEND_ZC
/// was submitted.
/// Where a ring was refused under the user's locked-memory budget, the writer
/// runs on plain sends with the fallback counted. Where SEND_ZC cannot run
/// (no io_uring, pre-6.0 kernel), the bytes must still arrive and exactly one
/// fallback must be counted.
#[test]
fn explicit_zero_copy_writer_sends_through_send_zc_over_tcp_loopback() {
    const TEST: &str = "explicit_zero_copy_writer_sends_through_send_zc_over_tcp_loopback";
    let runnable = send_zc_runnable(TEST);
    let (engaged, before, after, len) =
        loopback_write(fast_io::ZeroCopyPolicy::Enabled, usize::MAX);

    if runnable && engaged {
        let submitted = (after.sends - before.sends) + (after.copied_sends - before.copied_sends);
        assert!(
            submitted > 0,
            "no IORING_OP_SEND_ZC submission was counted: {before:?} -> {after:?}"
        );
        // A final buffered tail shorter than the dispatch floor goes out on a
        // plain IORING_OP_SEND by design and is counted nowhere.
        let plain_tail = fast_io::SEND_ZC_DISPATCH_MIN_BYTES as u64 - 1;
        assert_bytes_went_through_send_zc(TEST, before, after, len as u64, plain_tail);
    } else if runnable {
        assert_writer_ring_refused(TEST);
        assert_eq!(after.sends, before.sends, "no SEND_ZC may be counted here");
        assert!(
            after.fallbacks > before.fallbacks,
            "a writer that dropped SEND_ZC must count the fallback: {before:?} -> {after:?}"
        );
    } else {
        assert!(
            !engaged,
            "SEND_ZC cannot run here, so the writer must not claim it"
        );
        assert_eq!(after.sends, before.sends, "no SEND_ZC may be counted here");
        assert_eq!(
            after.fallbacks - before.fallbacks,
            1,
            "the requested-but-unavailable SEND_ZC must be counted as one fallback"
        );
    }
}

/// The production writer with no pinnable memory, on both of its SEND_ZC
/// paths: buffered 16 KiB writes reach the pipeline's flush and 256 KiB writes
/// go straight through `try_send_zc`. Every SEND_ZC is refused with `ENOMEM`,
/// and the transfer must still complete byte for byte on counted copying sends.
///
/// The writer's rings - the per-thread ring, built with the writer, and the
/// pipeline's ring, built on its first zero-copy flush - are created while the
/// budget is open: a warm-up write and flush builds the pipeline before
/// [`exhaust_pinned_page_budget`]. On a kernel that charges ring creation,
/// spending the budget first leaves the writer no ring at all, and it falls
/// back to a plain socket writer before SEND_ZC is ever submitted.
#[test]
fn zero_copy_writer_copies_what_the_kernel_cannot_pin() {
    const TEST: &str = "zero_copy_writer_copies_what_the_kernel_cannot_pin";
    if !send_zc_runnable(TEST) {
        return;
    }
    charge_this_thread_for_pinned_pages();

    for write_size in [16 * 1024, 256 * 1024] {
        let unlimited = open_pinned_page_budget();
        let WriterArm::ZeroCopy { before, after, len } =
            exhausted_loopback_write(TEST, write_size, unlimited)
        else {
            continue;
        };
        assert_eq!(
            after.bytes, before.bytes,
            "{write_size}-byte writes: nothing can go zero-copy: {before:?} -> {after:?}"
        );
        // A final buffered tail shorter than the dispatch floor goes out on a
        // plain IORING_OP_SEND by design and is counted nowhere.
        let copied = after.copied_bytes - before.copied_bytes;
        let plain_tail = fast_io::SEND_ZC_DISPATCH_MIN_BYTES as u64 - 1;
        assert!(
            copied <= len && copied + plain_tail >= len,
            "{write_size}-byte writes: every byte must be a counted copying send: \
             {before:?} -> {after:?}"
        );
    }
}

/// Which path [`exhausted_loopback_write`] took.
enum WriterArm {
    /// The writer kept SEND_ZC through its warm-up; the counters are the
    /// snapshots around the writes made after the budget was spent, and `len`
    /// is their byte count.
    ZeroCopy {
        before: fast_io::SendZcStats,
        after: fast_io::SendZcStats,
        len: u64,
    },
    /// The locked-memory budget refused a ring at setup, so the writer runs
    /// on plain sends; the fallback was counted and the payload still arrived
    /// byte for byte.
    Refused,
}

/// Writes 2 MiB through an `Enabled` socket writer in `write_size` calls,
/// spending the pinned-page budget after a warm-up that builds the writer's
/// rings, and asserts the peer received every byte.
///
/// Returns [`WriterArm::ZeroCopy`] when the writer kept SEND_ZC through the
/// warm-up. When a ring was refused instead, asserts that the refusal can
/// only be the user's shared budget (never with an unlimited budget or
/// `CAP_IPC_LOCK`) and that the writer counted its fallback, then returns
/// [`WriterArm::Refused`] - the production path a budget-limited user gets.
fn exhausted_loopback_write(test: &str, write_size: usize, unlimited: bool) -> WriterArm {
    use std::net::{TcpListener, TcpStream};

    const WARM_UP: usize = 16 * 1024;
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("loopback addr");
    let payload: Vec<u8> = (0..2 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let expected_len = payload.len();
    let reader = thread::spawn(move || {
        let (mut peer, _) = listener.accept().expect("accept");
        peer.set_read_timeout(Some(Duration::from_secs(30)))
            .expect("read timeout");
        let mut received = Vec::with_capacity(expected_len);
        peer.read_to_end(&mut received).expect("read to end");
        received
    });

    let sender = TcpStream::connect(addr).expect("connect");
    let at_setup = fast_io::send_zc_stats();
    let mut writer = fast_io::socket_writer_from_fd_zero_copy(
        sender.as_raw_fd(),
        64 * 1024,
        fast_io::ZeroCopyPolicy::Enabled,
    )
    .expect("factory never errors");
    writer
        .write_all(&payload[..WARM_UP])
        .expect("warm-up write");
    writer.flush().expect("warm-up flush");
    let engaged = matches!(
        writer,
        fast_io::IoUringOrStdSocketWriter::IoUring(ref w) if w.send_zc_active()
    );
    if !engaged {
        assert!(
            !unlimited && !holds_ipc_lock(),
            "{test}: {write_size}-byte writes: the writer dropped SEND_ZC at setup although \
             no ring of this thread can be refused for memory"
        );
        assert!(
            fast_io::send_zc_stats().fallbacks > at_setup.fallbacks,
            "{test}: {write_size}-byte writes: a writer that dropped SEND_ZC must count the fallback"
        );
        eprintln!(
            "[send_zc env] {test}: {write_size}-byte writes: a ring was refused by the \
             locked-memory budget - asserting the plain-send fallback delivers the payload"
        );
    } else {
        eprintln!(
            "[send_zc env] {test}: {write_size}-byte writes: rings built - asserting every \
             SEND_ZC refused after the budget is spent is resent as a copying send"
        );
        exhaust_pinned_page_budget();
    }
    let before = fast_io::send_zc_stats();
    for chunk in payload[WARM_UP..].chunks(write_size) {
        writer.write_all(chunk).expect("write payload");
    }
    writer.flush().expect("flush");
    drop(writer);
    drop(sender);
    let after = fast_io::send_zc_stats();

    let received = reader.join().expect("reader thread");
    assert_eq!(
        received, payload,
        "{write_size}-byte writes: loopback transcript must equal the payload"
    );
    if engaged {
        WriterArm::ZeroCopy {
            before,
            after,
            len: (payload.len() - WARM_UP) as u64,
        }
    } else {
        WriterArm::Refused
    }
}
