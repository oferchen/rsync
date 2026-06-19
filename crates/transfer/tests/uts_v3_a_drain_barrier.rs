//! UTS-V3.A regression: the daemon-sender exit path must flush every
//! user-space byte through the writer graph and then half-close the kernel
//! send side BEFORE the connection drops.
//!
//! Audit (`docs/design/uts-v3-a-goodbye-flush-audit.md`) traced the cluster-A
//! wire-cutoffs (~2.25 MB on `batch-mode`, `alt-dest`, and
//! `daemon-refuse-compress`; ~615 KB on `daemon-gzip-download`) to user-space
//! bytes still sitting in the multiplex `BufWriter` / codec trailer when the
//! daemon's SO_LINGER + shutdown(SHUT_WR) teardown fired. The fix is a
//! two-stage barrier:
//!
//! 1. [`ServerWriter::flush_all_pending`] drains user-space buffers.
//! 2. [`shutdown_send_side`] drains the kernel send buffer with bounded
//!    SO_LINGER and issues an explicit `shutdown(SHUT_WR)`.
//!
//! This integration test exercises both methods end-to-end over a real
//! `TcpListener` bound to port 0, pushes >= 3 MiB of payload (past the A2
//! cutoff at ~2.25 MB), and asserts the receiver observed every byte before
//! FIN. The listener is read-to-EOF: any byte still queued in the sender's
//! user-space buffers when the half-close fires would show up as a short
//! read on the receive side.
//!
//! upstream: `cleanup.c::handle_cleanup()` brackets the sender's final
//! `io_flush(FULL_FLUSH)` with the process exit so every queued byte hits
//! the wire before the kernel queues FIN.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use transfer::writer::{ServerWriter, shutdown_send_side};

/// Mirrors the cluster-A boundary: roughly 2.25 MiB of payload pushed through
/// the multiplex writer, with a comfortable margin so any flush-shortfall is
/// observable. The receiver thread expects every byte before observing EOF.
const PAYLOAD_BYTES: usize = 3 * 1024 * 1024;

/// Drain-barrier timeout. Matches the daemon teardown's 5 s SO_LINGER window
/// in `crates/daemon/src/daemon/sections/module_access/transfer.rs`.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Drives the cluster-A payload through `ServerWriter::flush_all_pending`
/// followed by `shutdown_send_side` over a real `TcpStream`. Asserts the
/// receiver sees the entire payload before observing FIN.
///
/// Port 0 + `local_addr()` keeps the test isolated from any concurrent
/// runner that holds well-known ports.
#[test]
fn daemon_sender_emits_final_byte_before_fin() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");

    // Receiver: read to EOF. If the sender drops before flushing user-space
    // buffers or before half-closing the kernel send queue, the receiver
    // observes a short read instead of the full payload.
    let server_thread = thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        sock.set_read_timeout(Some(DRAIN_TIMEOUT * 2))
            .expect("set_read_timeout");
        let mut buf = Vec::with_capacity(PAYLOAD_BYTES);
        sock.read_to_end(&mut buf).expect("read_to_end");
        buf
    });

    let stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_write_timeout(Some(DRAIN_TIMEOUT))
        .expect("set_write_timeout");

    // The actual daemon-sender writer graph wraps a `CountingWriter`
    // around the stream and a `ServerWriter::Plain` around that. For the
    // drain-barrier contract we only need the outermost `ServerWriter`
    // wired against the socket, since the barrier traverses through
    // `Write::flush` and `finalize_compression`. Plain mode is the
    // minimum surface where `flush_all_pending` and the subsequent
    // `shutdown_send_side` produce an observable wire effect.
    let mut writer: ServerWriter<TcpStream> =
        ServerWriter::new_plain(stream.try_clone().expect("clone TcpStream for writer"));

    // Write the full payload. Deterministic pattern lets the receiver
    // assert byte-for-byte equality, not just length.
    let payload: Vec<u8> = (0..PAYLOAD_BYTES).map(|i| (i % 251) as u8).collect();
    writer.write_all(&payload).expect("write payload");

    // Stage 1: drain user-space buffers. Must precede any socket-level
    // half-close so the multiplex/codec trailer reaches the kernel send
    // queue before SO_LINGER + shutdown(SHUT_WR) fire.
    writer.flush_all_pending().expect("flush_all_pending");

    // Stage 2: kernel-level drain + explicit half-close. SO_LINGER
    // ensures the queued payload + trailer are ACKed before FIN; the
    // shutdown(SHUT_WR) makes the half-close observable and
    // error-surfacing.
    shutdown_send_side(&stream, DRAIN_TIMEOUT).expect("shutdown_send_side");

    // Drop the writer chain so any remaining clone closes. The receive
    // side has already drained to EOF because shutdown(SHUT_WR) emitted
    // FIN above; this drop is housekeeping.
    drop(writer);
    drop(stream);

    let received = server_thread.join().expect("server thread");
    assert_eq!(
        received.len(),
        PAYLOAD_BYTES,
        "receiver observed short read: {} of {} bytes (drain barrier missing?)",
        received.len(),
        PAYLOAD_BYTES,
    );
    assert_eq!(received, payload, "byte content diverged after drain");
}

/// `flush_all_pending` must be idempotent: calling it twice in a row is the
/// orchestrator's defense-in-depth pattern when the goodbye finaliser already
/// ran `finalize_compression` once during `handle_goodbye_with_finalizer`.
///
/// A second call against a plain `Vec` sink should not double-flush, double-
/// finalise, or panic, and must report `Ok(())`.
#[test]
fn flush_all_pending_is_idempotent() {
    let buf: Vec<u8> = Vec::new();
    let mut writer = ServerWriter::new_plain(buf);
    writer.write_all(b"hello").expect("write");

    writer.flush_all_pending().expect("first flush_all_pending");
    writer
        .flush_all_pending()
        .expect("second flush_all_pending must be idempotent");
}
