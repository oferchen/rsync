//! Integration tests for the `ZeroCopySender` zero-copy socket sender.
//!
//! Sends 1 MiB of pseudo-random bytes over a `socketpair(AF_UNIX,
//! SOCK_STREAM, 0)` and asserts that the receive end observes the exact
//! same bytes. The test runs only on Linux with both the `io_uring` and
//! `iouring-send-zc` cargo features enabled and skips gracefully when the
//! running kernel does not advertise `IORING_OP_SEND_ZC` (kernel < 6.0,
//! seccomp-restricted environments, container runtimes that block the
//! opcode).

#![cfg(all(target_os = "linux", feature = "iouring-send-zc"))]

use std::io::Read;
use std::os::fd::FromRawFd;
use std::os::unix::io::{AsRawFd, OwnedFd, RawFd};
use std::thread;
use std::time::Duration;

// `FromRawFd` is still required by the OwnedFd reconstruction inside
// `socketpair_unix_stream`; the test-level `UnixStream::from(OwnedFd)`
// conversion is a separate path that does not need the raw fd import.

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
fn zero_copy_sender_roundtrips_1mib_over_socketpair() {
    const TEST: &str = "zero_copy_sender_roundtrips_1mib_over_socketpair";
    if !send_zc_runnable(TEST) {
        return;
    }

    let (send_fd, recv_fd) = match socketpair_unix_stream() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("[send_zc env] SKIP {TEST}: socketpair() unavailable ({e})");
            return;
        }
    };

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
    // socket buffer cannot fill and block the sender. `OwnedFd` ->
    // `UnixStream` is a safe `From` conversion that hands the fd's
    // ownership across without going through raw fds.
    let expected_len = payload.len();
    let reader = thread::spawn(move || {
        let mut stream = std::os::unix::net::UnixStream::from(recv_fd);
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("set_read_timeout");
        let mut received: Vec<u8> = Vec::with_capacity(expected_len);
        let mut scratch = [0u8; 32 * 1024];
        while received.len() < expected_len {
            match stream.read(&mut scratch) {
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
        // Some sandboxes advertise IORING_OP_SEND_ZC via the probe ring but
        // fail the actual submission (seccomp, container runtime policy,
        // unprivileged user namespace). Treat that as a runtime-skip rather
        // than a hard failure so CI environments without working SEND_ZC do
        // not block the queue.
        let n = match sender.send_zc(chunk) {
            Ok(n) => n,
            Err(e) => {
                report_setup_failure(TEST, "ZeroCopySender::send_zc", &e);
                drop(sender);
                drop(send_fd);
                let _ = reader.join();
                return;
            }
        };
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

/// The default build's daemon-sender path: `socket_writer_from_fd_zero_copy`
/// under the default `Auto` policy over a real TCP loopback pair.
///
/// Where SEND_ZC can run, the writer must engage it, the peer must receive the
/// exact bytes, and the process counters must show the payload went through
/// `IORING_OP_SEND_ZC` - a writer that silently fell back to plain sends would
/// pass a byte comparison alone. Where it cannot run, the factory must keep the
/// plain writer, still deliver the exact bytes, and count one fallback.
#[test]
fn default_auto_writer_sends_through_send_zc_over_tcp_loopback() {
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};

    const TEST: &str = "default_auto_writer_sends_through_send_zc_over_tcp_loopback";
    let runnable = send_zc_runnable(TEST);

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
    let mut writer = fast_io::socket_writer_from_fd_zero_copy(
        sender.as_raw_fd(),
        64 * 1024,
        fast_io::ZeroCopyPolicy::Auto,
    )
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

    if runnable {
        assert!(
            engaged,
            "the default Auto policy must engage SEND_ZC on this kernel"
        );
        assert!(
            after.sends > before.sends,
            "no IORING_OP_SEND_ZC completion was counted: {before:?} -> {after:?}"
        );
        assert!(
            after.bytes - before.bytes >= payload.len() as u64 / 2,
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
            "the permitted-but-unavailable SEND_ZC must be counted as one fallback"
        );
    }
}
