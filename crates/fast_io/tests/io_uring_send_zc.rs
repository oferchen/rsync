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

#![cfg(all(target_os = "linux", feature = "iouring-send-zc"))]

use std::io::Read;
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

use send_zc_test_env::{SendZcEnv, classify, report, report_setup_failure};

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
            report_setup_failure(TEST, "ZeroCopySender::new", &e);
            return;
        }
    };

    let mut sent_total = 0usize;
    while sent_total < payload.len() {
        let chunk = &payload[sent_total..];
        // The environment probe said SEND_ZC runs here, so a rejected
        // submission is a real failure, not a skip. (Over a socketpair this
        // used to be EOPNOTSUPP on every kernel and the test never ran.)
        let n = sender
            .send_zc(chunk)
            .unwrap_or_else(|e| panic!("SEND_ZC rejected on a SEND_ZC-capable host: {e}"));
        assert!(n > 0, "kernel reported zero bytes sent");
        sent_total += n;
    }

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
            report_setup_failure(TEST, "ZeroCopySender::new", &e);
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
            report_setup_failure(TEST, "ZeroCopySender::new", &e);
            return;
        }
    };
    assert_eq!(sender.raw_fd(), raw);
    assert!(sender.slot_bytes() >= 4 * 1024);
}

/// Drives `socket_writer_from_fd_zero_copy` under `policy` over a real TCP
/// loopback pair with a 2 MiB payload. Asserts the peer received the exact
/// bytes and returns whether the writer claimed SEND_ZC plus the counter
/// snapshots taken around the write.
fn loopback_write(
    policy: fast_io::ZeroCopyPolicy,
) -> (bool, fast_io::SendZcStats, fast_io::SendZcStats, usize) {
    use std::io::Write;
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
    writer.write_all(&payload).expect("write payload");
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
    let (engaged, before, after, _) = loopback_write(fast_io::ZeroCopyPolicy::Auto);
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
/// silently fell back to plain sends would pass a byte comparison alone.
/// Where it cannot (no io_uring, pre-6.0 kernel), the bytes must still arrive
/// and exactly one fallback must be counted.
#[test]
fn explicit_zero_copy_writer_sends_through_send_zc_over_tcp_loopback() {
    const TEST: &str = "explicit_zero_copy_writer_sends_through_send_zc_over_tcp_loopback";
    let runnable = send_zc_runnable(TEST);
    let (engaged, before, after, len) = loopback_write(fast_io::ZeroCopyPolicy::Enabled);

    if runnable {
        assert!(engaged, "--zero-copy must engage SEND_ZC on this kernel");
        assert!(
            after.sends > before.sends,
            "no IORING_OP_SEND_ZC completion was counted: {before:?} -> {after:?}"
        );
        assert!(
            after.bytes - before.bytes >= len as u64 / 2,
            "most of the payload must travel through SEND_ZC: {before:?} -> {after:?}"
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
