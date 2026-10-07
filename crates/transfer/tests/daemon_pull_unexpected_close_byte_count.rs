//! A daemon pull whose server stream is cut off must name the same byte count
//! as upstream in "connection unexpectedly closed (N bytes received so far)".
//!
//! upstream: io.c:938 adds to `stats.total_read` only inside `perform_io()`,
//! which runs once `io_start_buffering_in()` is active (main.c:1307-1308).
//! `setup_protocol()` reads the compat flags and checksum seed before that,
//! through `safe_read()`, so those bytes are never counted. oc used to count
//! them, and reported 5 bytes more than upstream for every cut.
//!
//! The expected counts were measured with rsync 3.5.1 as the client against
//! the same replayed stream: 0, 10, 22 and 26 bytes for the cuts below.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener};
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use test_support::oc_rsync_bin;

/// Compat flags (`CF_INPLACE_PARTIAL_DIR`, varint) and checksum seed.
const SETUP: [u8; 5] = [0x40, 0x78, 0x56, 0x34, 0x12];
/// One regular-file entry `keep` (mode 0644, 38 bytes) and the end-of-list byte.
const FLIST: [u8; 18] = [
    0x18, 0x04, 0x6b, 0x65, 0x65, 0x70, 0x00, 0x26, 0x00, 0x65, 0x00, 0xf1, 0x53, 0xa4, 0x81, 0x00,
    0x00, 0x00,
];
const MSG_DATA_TAG: u32 = 7;

/// The server stream after the setup bytes: the file list in one `MSG_DATA`
/// frame, then the header of a 100-byte `MSG_DATA` frame and its payload.
fn post_setup_stream() -> Vec<u8> {
    let mut out = ((MSG_DATA_TAG << 24) | FLIST.len() as u32)
        .to_le_bytes()
        .to_vec();
    out.extend_from_slice(&FLIST);
    out.extend_from_slice(&((MSG_DATA_TAG << 24) | 100).to_le_bytes());
    out.extend(0..100u8);
    out
}

/// Plays a daemon that sends the setup bytes plus `cut` bytes of the stream,
/// then half-closes and drains the client until it disconnects.
fn serve_cut_stream(listener: TcpListener, cut: usize) -> JoinHandle<()> {
    thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept the client");
        sock.set_read_timeout(Some(Duration::from_secs(30)))
            .expect("read timeout");
        sock.write_all(b"@RSYNCD: 30.0\n").expect("greeting");
        let mut reader = BufReader::new(sock.try_clone().expect("clone socket"));
        let mut line = String::new();
        reader.read_line(&mut line).expect("client greeting");
        line.clear();
        reader.read_line(&mut line).expect("module line");

        let mut out = b"@RSYNCD: OK\n".to_vec();
        out.extend_from_slice(&SETUP);
        out.extend_from_slice(&post_setup_stream()[..cut]);
        sock.write_all(&out).expect("cut stream");
        sock.shutdown(Shutdown::Write).expect("half-close");

        let mut sink = [0u8; 4096];
        while matches!(reader.read(&mut sink), Ok(n) if n > 0) {}
    })
}

fn pull_against_cut_stream(cut: usize) -> (Option<i32>, String) {
    let tmp = TempDir::new().expect("tempdir");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = serve_cut_stream(listener, cut);

    let mut child = Command::new(oc_rsync_bin())
        .args(["--protocol=30", "-r"])
        .arg(format!("rsync://127.0.0.1:{port}/mod/keep"))
        .arg(format!("{}/", tmp.path().display()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn oc-rsync");
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().expect("poll client").is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("the client hung against a stream cut at {cut} bytes");
        }
        thread::sleep(Duration::from_millis(50));
    }
    let output = child.wait_with_output().expect("client output");
    server.join().expect("cut-stream daemon thread");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code(), text)
}

/// Cuts at the start, inside the file-list frame, at its end, and after the
/// next frame header. The setup bytes must never be part of the count.
#[test]
fn a_cut_off_daemon_stream_reports_upstreams_byte_count() {
    for cut in [0, 10, 22, 26] {
        let (code, text) = pull_against_cut_stream(cut);
        assert_eq!(code, Some(12), "cut at {cut}: {text}");
        let expected =
            format!("connection unexpectedly closed ({cut} bytes received so far) [receiver]");
        assert!(
            text.contains(&expected),
            "cut at {cut}: expected `{expected}`, got: {text}"
        );
    }
}
