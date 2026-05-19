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
use fast_io::io_uring::send_zc;

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
    if !send_zc::is_supported() {
        eprintln!("IORING_OP_SEND_ZC unsupported on this kernel; skipping");
        return;
    }

    let (send_fd, recv_fd) = match socketpair_unix_stream() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("socketpair() unavailable ({e}); skipping");
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
            eprintln!("ZeroCopySender::new failed ({e}); skipping");
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
                eprintln!("ZeroCopySender::send_zc rejected at runtime ({e}); skipping");
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
    if !send_zc::is_supported() {
        eprintln!("IORING_OP_SEND_ZC unsupported on this kernel; skipping");
        return;
    }
    let (send_fd, _recv_fd) = match socketpair_unix_stream() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("socketpair() unavailable ({e}); skipping");
            return;
        }
    };
    let mut sender = match ZeroCopySender::new(send_fd.as_raw_fd()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ZeroCopySender::new failed ({e}); skipping");
            return;
        }
    };
    let err = sender.send_zc(&[]).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn zero_copy_sender_reports_raw_fd() {
    if !send_zc::is_supported() {
        eprintln!("IORING_OP_SEND_ZC unsupported on this kernel; skipping");
        return;
    }
    let (send_fd, _recv_fd) = match socketpair_unix_stream() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("socketpair() unavailable ({e}); skipping");
            return;
        }
    };
    let raw = send_fd.as_raw_fd();
    let sender = match ZeroCopySender::new(raw) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ZeroCopySender::new failed ({e}); skipping");
            return;
        }
    };
    assert_eq!(sender.raw_fd(), raw);
    assert!(sender.slot_bytes() >= 4 * 1024);
}
