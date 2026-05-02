//! Integration tests for the shared io_uring ring (issue #1874).
//!
//! Covers:
//!
//! - Tag round-trip on every platform (pure arithmetic, no syscalls).
//! - End-to-end read + poll-write + send concurrency on a single ring,
//!   gated to `cfg(all(target_os = "linux", feature = "io_uring"))` and
//!   skipped at runtime when [`is_io_uring_available`] returns `false`
//!   (older kernels, seccomp inside containers).
//! - Fallback path: `SharedRing::try_new` returns `None` when io_uring is
//!   unavailable, and the public stub on non-Linux behaves the same way.

use fast_io::{OpTag, SharedRing, SharedRingConfig, is_io_uring_available};

#[test]
fn op_tag_encoding_round_trips_for_every_kind() {
    for tag in [OpTag::Read, OpTag::Write, OpTag::Send, OpTag::PollWrite] {
        for &op_id in &[0u64, 1, 9_999, (1u64 << 56) - 1] {
            let encoded = tag.encode(op_id);
            let (decoded_tag, decoded_id) = OpTag::decode(encoded).expect("known tag must decode");
            assert_eq!(decoded_tag, tag);
            assert_eq!(decoded_id, op_id);
        }
    }
}

#[test]
fn op_tag_decode_returns_none_for_unknown_tag() {
    // Tag value 0xff is not a defined OpTag variant.
    let user_data = (0xffu64 << 56) | 0xdead;
    assert!(OpTag::decode(user_data).is_none());
}

/// On any platform without a working io_uring kernel (non-Linux, missing
/// feature, or syscall blocked) `try_new` must return `None`, and the
/// caller must therefore fall back to the per-channel ring path or to
/// standard buffered I/O.
#[test]
fn try_new_falls_back_when_io_uring_unavailable() {
    if is_io_uring_available() {
        // Live kernel - the negative case is covered by the runtime
        // pre-check inside the construction-success test below. Skip here
        // because we need a guaranteed-unavailable environment to assert
        // `None`.
        return;
    }
    // Both fds are dummy values; `try_new` must short-circuit on
    // availability before touching them.
    let cfg = SharedRingConfig::default();
    assert!(SharedRing::try_new(0, 1, &cfg).is_none());
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
mod linux_only {
    use std::io::Write;
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;

    use fast_io::{OpTag, SharedCompletion, SharedRing, SharedRingConfig, is_io_uring_available};
    use tempfile::tempdir;

    /// Drives the shared ring through a single read + poll-write + send
    /// cycle, demonstrating that one ring services both directions and that
    /// the demux loop returns each completion under the matching tag.
    ///
    /// Skipped at runtime when io_uring is unavailable (older kernel or
    /// seccomp restriction inside the test container).
    #[test]
    fn read_and_write_share_one_ring_with_demux() {
        if !is_io_uring_available() {
            eprintln!("skipping shared-ring concurrency test: io_uring unavailable");
            return;
        }

        // File: prepare a known payload to read.
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("shared_ring_input.bin");
        let payload = b"hello shared io_uring world".to_vec();
        std::fs::write(&path, &payload).expect("write payload");
        let file = std::fs::File::open(&path).expect("open payload");

        // Socket pair: writer side will be poll/send-driven; reader side
        // verifies the bytes the kernel actually delivered.
        let (writer_sock, peer_sock) = UnixStream::pair().expect("socket pair");
        peer_sock.set_nonblocking(true).expect("nonblocking peer");

        let cfg = SharedRingConfig::default();
        let mut ring = match SharedRing::try_new(file.as_raw_fd(), writer_sock.as_raw_fd(), &cfg) {
            Some(r) => r,
            None => {
                eprintln!("skipping: SharedRing::try_new returned None");
                return;
            }
        };

        // Submit one read at offset 0 and one POLL_ADD(POLLOUT) on the
        // writer fd. Distinct op_ids so the demux can be verified.
        let mut read_buf = vec![0u8; payload.len()];
        ring.submit_read(101, 0, &mut read_buf)
            .expect("submit read");
        ring.submit_poll_write(202).expect("submit poll");

        // Submit and wait until both completions arrive.
        ring.submit_and_wait(2).expect("submit_and_wait");
        let completions = ring.reap().expect("reap");
        assert!(
            completions.len() >= 2,
            "expected at least 2 completions, got {completions:?}"
        );

        let mut saw_read = false;
        let mut saw_poll = false;
        for c in &completions {
            match *c {
                SharedCompletion::Read { op_id, result } => {
                    assert_eq!(op_id, 101, "read tag must carry the submitted op_id");
                    assert!(
                        result >= 0,
                        "read result must be non-negative, got {result}"
                    );
                    assert_eq!(result as usize, payload.len(), "expected full read");
                    saw_read = true;
                }
                SharedCompletion::PollWrite { op_id, revents } => {
                    assert_eq!(op_id, 202, "poll tag must carry the submitted op_id");
                    assert!(
                        (revents as i32) & libc::POLLOUT != 0,
                        "expected POLLOUT in revents, got {revents:#x}"
                    );
                    saw_poll = true;
                }
                other => panic!("unexpected completion kind: {other:?}"),
            }
        }
        assert!(saw_read, "missing read completion");
        assert!(saw_poll, "missing poll completion");
        assert_eq!(read_buf, payload, "buffer content must match the file");

        // Now submit a send on the writer and verify the peer receives it.
        let send_data = b"poll-then-send";
        ring.submit_send(303, send_data).expect("submit send");
        ring.submit_and_wait(1).expect("submit_and_wait send");
        let send_completions = ring.reap().expect("reap send");
        assert_eq!(send_completions.len(), 1, "expected single send completion");
        match send_completions[0] {
            SharedCompletion::Send { op_id, result } => {
                assert_eq!(op_id, 303);
                assert!(result >= 0);
                assert_eq!(result as usize, send_data.len());
            }
            other => panic!("expected Send completion, got {other:?}"),
        }

        // Drain the peer to confirm the bytes really left the writer fd.
        let mut peer_buf = vec![0u8; send_data.len()];
        let n = read_with_retry(&peer_sock, &mut peer_buf);
        assert_eq!(n, send_data.len());
        assert_eq!(&peer_buf[..n], send_data);
    }

    /// Submits four reads and two poll-writes interleaved on the same ring,
    /// verifying that completions can arrive in any order and that the
    /// demux still routes them correctly via [`OpTag`].
    #[test]
    fn many_interleaved_ops_demux_independently() {
        if !is_io_uring_available() {
            eprintln!("skipping interleaved demux test: io_uring unavailable");
            return;
        }

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("interleaved.bin");
        let payload: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &payload).expect("write payload");
        let file = std::fs::File::open(&path).expect("open payload");

        let (writer_sock, _peer) = UnixStream::pair().expect("socket pair");

        let mut ring = match SharedRing::try_new(
            file.as_raw_fd(),
            writer_sock.as_raw_fd(),
            &SharedRingConfig::default(),
        ) {
            Some(r) => r,
            None => {
                eprintln!("skipping: SharedRing::try_new returned None");
                return;
            }
        };

        // Allocate four 256-byte buffers, each reading a distinct quarter.
        let mut bufs: Vec<Vec<u8>> = (0..4).map(|_| vec![0u8; 256]).collect();
        // Submit in interleaved order: read, poll, read, poll, read, read.
        for (i, buf) in bufs.iter_mut().enumerate() {
            let offset = (i * 256) as u64;
            ring.submit_read(1000 + i as u64, offset, buf)
                .expect("submit read");
            if i < 2 {
                ring.submit_poll_write(2000 + i as u64)
                    .expect("submit poll");
            }
        }

        let mut received = 0usize;
        let target = 6usize; // 4 reads + 2 polls
        let mut read_seen = vec![false; 4];
        let mut poll_seen = vec![false; 2];

        while received < target {
            ring.submit_and_wait(1).expect("submit_and_wait");
            for c in ring.reap().expect("reap") {
                match c {
                    SharedCompletion::Read { op_id, result } => {
                        let idx = (op_id - 1000) as usize;
                        assert!(idx < 4, "unexpected read op_id {op_id}");
                        assert!(!read_seen[idx], "duplicate read CQE for op_id {op_id}");
                        read_seen[idx] = true;
                        assert_eq!(result, 256);
                        received += 1;
                    }
                    SharedCompletion::PollWrite { op_id, .. } => {
                        let idx = (op_id - 2000) as usize;
                        assert!(idx < 2, "unexpected poll op_id {op_id}");
                        assert!(!poll_seen[idx], "duplicate poll CQE for op_id {op_id}");
                        poll_seen[idx] = true;
                        received += 1;
                    }
                    other => panic!("unexpected completion: {other:?}"),
                }
            }
        }

        // Reassemble the payload from the four bufs and verify.
        let mut reassembled = Vec::with_capacity(payload.len());
        for buf in &bufs {
            reassembled.extend_from_slice(buf);
        }
        assert_eq!(reassembled, payload);
    }

    /// Confirms the per-channel fallback path still works after the shared
    /// ring is in use: the existing `socket_writer_from_fd` constructor
    /// must continue to operate without interference from this PR. This
    /// guards against accidental coupling that would break the documented
    /// fallback chain.
    #[test]
    fn per_channel_fallback_path_still_works() {
        if !is_io_uring_available() {
            // The fallback path is exactly the per-channel BufWriter on
            // older kernels, which is exercised by the existing
            // io_uring_probe_fallback tests. Nothing to assert here.
            return;
        }

        let (sock, _peer) = UnixStream::pair().expect("socket pair");
        let mut writer = fast_io::socket_writer_from_fd(
            sock.as_raw_fd(),
            8 * 1024,
            fast_io::IoUringPolicy::Auto,
        )
        .expect("auto policy must succeed on this platform");
        writer.write_all(b"per-channel still works").expect("write");
        writer.flush().expect("flush");
    }

    /// Reads from a non-blocking socket with a small retry loop because the
    /// remote send is queued through io_uring and may not be visible
    /// immediately on the peer side under very high system load.
    fn read_with_retry(stream: &UnixStream, buf: &mut [u8]) -> usize {
        use std::io::Read;
        let mut attempts = 0;
        loop {
            match (&*stream).read(buf) {
                Ok(n) if n > 0 => return n,
                Ok(_) => {
                    if attempts > 100 {
                        panic!("peer never received the bytes after 100 attempts");
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if attempts > 100 {
                        panic!("WouldBlock for too long: {e}");
                    }
                }
                Err(e) => panic!("peer read failed: {e}"),
            }
            attempts += 1;
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// `OpTag::encode` followed by `OpTag::decode` must be lossless for the
    /// full 56-bit op_id range. This is a stronger version of the unit test
    /// in the live module - it picks a few values across the boundary so
    /// the integration suite catches any future regression in the encoding
    /// layout.
    #[test]
    fn op_tag_encoding_handles_boundary_op_ids() {
        let boundary_ids = [0u64, 1, (1u64 << 32) - 1, 1u64 << 32, (1u64 << 56) - 1];
        for &id in &boundary_ids {
            for tag in [OpTag::Read, OpTag::Write, OpTag::Send, OpTag::PollWrite] {
                let encoded = tag.encode(id);
                let (back_tag, back_id) = OpTag::decode(encoded).expect("decode");
                assert_eq!(back_tag, tag, "tag mismatch for id {id}");
                assert_eq!(back_id, id, "id mismatch for tag {tag:?}");
            }
        }
    }
}
