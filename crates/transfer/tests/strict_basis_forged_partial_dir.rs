//! A sender that forges a partial-dir basis and then ends the phase must end
//! the transfer the way upstream's receiver does.
//!
//! The rsync 3.5.1 `strict-basis` cell ("Forged partial-dir token") plays a
//! malicious daemon sender. It answers the client's request for the one file
//! with an `FNAMECMP_PARTIAL_DIR` basis tag the client never asked for, a
//! literal payload, and an all-zero whole-file checksum, then sends four
//! `NDX_DONE`s and closes the socket without the end-of-transfer stats.
//!
//! Upstream's receiver discards the unverifiable data (the destination is left
//! as it was) and queues a redo. It then reads each `NDX_DONE` as the sender's
//! next phase boundary: `recv_files()` has no notion of a request still being
//! outstanding (receiver.c:852-876 - `if (++phase > max_phase) break;`). The
//! transfer then hits end-of-stream, which is `whine_about_eof()`
//! (io.c:282-304): "connection unexpectedly closed (N bytes received so far)",
//! exit RERR_STREAMIO (12).
//!
//! oc used to stop at the first `NDX_DONE`, because the redo it had already
//! requested was still unanswered, and reported a protocol violation with exit
//! 23 - a per-file-transfer code for what is a broken stream.
//!
//! The byte stream below is the one the cell's `rsync_proto` helper produces
//! for that sub-case (protocol 30, one regular file `keep`).

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use test_support::oc_rsync_bin;

/// The destination's content before the transfer; it must survive it.
const ORIGINAL_DATA: &[u8] = b"ORIGINAL_DEST_BYTES_KEEP_ME\n";
/// The forged literal payload the sender claims is the new content.
const MODIFIED_DATA: &[u8] = b"MODIFIED_INPLACE_DESPITE_BAD_CHECKSUM\n";

/// Compat flags (`CF_INPLACE_PARTIAL_DIR`, varint) and checksum seed.
const SETUP: [u8; 5] = [0x40, 0x78, 0x56, 0x34, 0x12];
/// One regular-file entry `keep` (mode 0644, 38 bytes, mtime 1700000000),
/// then the end-of-list byte, as `rsync_proto.FileEntry` encodes them.
const FLIST: [u8; 18] = [
    0x18, 0x04, 0x6b, 0x65, 0x65, 0x70, 0x00, 0x26, 0x00, 0x65, 0x00, 0xf1, 0x53, 0xa4, 0x81, 0x00,
    0x00, 0x00,
];

/// Frames `payload` as one `MSG_DATA` multiplex packet.
fn msg_data(payload: &[u8]) -> Vec<u8> {
    const MPLEX_BASE: u32 = 7;
    let header = (MPLEX_BASE << 24) | u32::try_from(payload.len()).expect("small payload");
    let mut out = header.to_le_bytes().to_vec();
    out.extend_from_slice(payload);
    out
}

/// The sender's reply to the request for index 0, then four `NDX_DONE`s.
fn forged_response() -> Vec<u8> {
    let mut out = vec![0x01]; // NDX 0 (diff 1 from -1)
    out.extend_from_slice(&0x8800u16.to_le_bytes()); // ITEM_TRANSFER | ITEM_BASIS_TYPE_FOLLOWS
    out.push(0x81); // FNAMECMP_PARTIAL_DIR
    out.extend_from_slice(&[0; 16]); // sum_head: count, blength, s2length, remainder = 0
    let len = u32::try_from(MODIFIED_DATA.len()).expect("small payload");
    out.extend_from_slice(&len.to_le_bytes()); // literal token
    out.extend_from_slice(MODIFIED_DATA);
    out.extend_from_slice(&0u32.to_le_bytes()); // end of tokens
    out.extend_from_slice(&[0; 16]); // an invalid whole-file MD5
    out.extend_from_slice(&[0; 4]); // four NDX_DONEs
    out
}

/// Plays the forged sender for one connection, then drains the client until it
/// closes, so the client meets end-of-stream only after it read everything.
fn serve_forged_sender(listener: TcpListener) -> JoinHandle<()> {
    thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept the client");
        sock.set_read_timeout(Some(Duration::from_secs(15)))
            .expect("read timeout");
        sock.write_all(b"@RSYNCD: 30.0\n").expect("greeting");
        let mut reader = BufReader::new(sock.try_clone().expect("clone socket"));
        let mut line = String::new();
        reader.read_line(&mut line).expect("client greeting");
        line.clear();
        reader.read_line(&mut line).expect("module line");

        let mut out = b"@RSYNCD: OK\n".to_vec();
        out.extend_from_slice(&SETUP);
        out.extend(msg_data(&FLIST));
        out.extend(msg_data(&forged_response()));
        sock.write_all(&out).expect("forged stream");

        // Drain until the client itself closes: closing first under load would
        // turn its end-of-stream into a reset on its next write.
        sock.set_read_timeout(Some(Duration::from_secs(30)))
            .expect("drain timeout");
        let mut sink = [0u8; 4096];
        while matches!(reader.read(&mut sink), Ok(n) if n > 0) {}
        drop(reader);
        drop(sock);
    })
}

#[test]
fn a_forged_partial_dir_basis_ends_on_the_broken_stream_like_upstream() {
    let tmp = TempDir::new().expect("tempdir");
    let dest = tmp.path().join("dest");
    std::fs::create_dir(&dest).expect("mkdir dest");
    std::fs::write(dest.join("keep"), ORIGINAL_DATA).expect("seed dest");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = serve_forged_sender(listener);

    let mut child = Command::new(oc_rsync_bin())
        .args(["--protocol=30", "-r", "--no-whole-file"])
        .arg(format!("rsync://127.0.0.1:{port}/mod/keep"))
        .arg(format!("{}/", dest.display()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn oc-rsync");
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().expect("poll client").is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("the client hung against the forged sender");
        }
        thread::sleep(Duration::from_millis(50));
    }
    let output = child.wait_with_output().expect("client output");
    server.join().expect("forged sender thread");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // upstream: io.c:298-303 - the whine, then exit_cleanup(RERR_STREAMIO).
    assert_eq!(output.status.code(), Some(12), "got: {text}");
    assert!(
        text.contains("connection unexpectedly closed (")
            && text.contains("bytes received so far) [receiver]"),
        "expected upstream's end-of-stream whine, got: {text}"
    );
    assert!(
        text.contains("error in rsync protocol data stream"),
        "expected the RERR_STREAMIO line, got: {text}"
    );
    assert_eq!(
        std::fs::read(dest.join("keep")).expect("read dest"),
        ORIGINAL_DATA,
        "the forged, unverifiable data must not reach the destination"
    );
}
