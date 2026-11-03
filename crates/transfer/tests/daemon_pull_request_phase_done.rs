//! Without INC_RECURSE, a pulling client must end its request phase with
//! `NDX_DONE` as soon as the last request is out, before the sender has sent
//! any file data.
//!
//! upstream: generator.c:2831-2833 writes `NDX_DONE` right after the
//! generate loop, while the receiver is still waiting for the first reply.
//! rsync 3.5.1 as the client against the replayed daemon below sends the
//! request for `keep`, then `NDX_DONE`, with no file data sent yet. Once the
//! file arrives, `--remove-source-files` adds its `MSG_SUCCESS`. oc used to
//! hold the `NDX_DONE` until every reply had been read, so its `MSG_SUCCESS`
//! went out first.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tempfile::TempDir;
use test_support::oc_rsync_bin;

/// Compat flags (`CF_INPLACE_PARTIAL_DIR`, no INC_RECURSE) and checksum seed.
const SETUP: [u8; 5] = [0x40, 0x78, 0x56, 0x34, 0x12];
/// One regular-file entry `keep` (mode 0644, 38 bytes) and the end-of-list byte.
const FLIST: [u8; 18] = [
    0x18, 0x04, 0x6b, 0x65, 0x65, 0x70, 0x00, 0x26, 0x00, 0x65, 0x00, 0xf1, 0x53, 0xa4, 0x81, 0x00,
    0x00, 0x00,
];
/// The client's empty filter list: one `MSG_DATA` frame carrying a zero int.
const CLIENT_FILTER_LIST: usize = 8;
/// `MPLEX_BASE` plus `MSG_DATA`.
const TAG_DATA: u8 = 7;
/// `MPLEX_BASE` plus `MSG_SUCCESS` (rsync.h).
const TAG_SUCCESS: u8 = 7 + 100;
/// The request for `keep`: ndx 0, ITEM_TRANSFER | ITEM_IS_NEW, and an empty
/// sum head (generator.c recv_generator()).
const REQUEST: [u8; 19] = [
    0x01, 0x00, 0xa0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
/// `NDX_DONE` on a protocol 30 wire.
const NDX_DONE: u8 = 0x00;
/// The bytes of `keep`.
const CONTENT: [u8; 38] = {
    let mut b = [0u8; 38];
    let mut i = 0;
    while i < 38 {
        b[i] = i as u8;
        i += 1;
    }
    b
};

/// One multiplexed frame from the client: its tag and payload.
type Frame = (u8, Vec<u8>);

fn frame(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = ((u32::from(tag) << 24) | payload.len() as u32)
        .to_le_bytes()
        .to_vec();
    out.extend_from_slice(payload);
    out
}

fn read_frame(reader: &mut impl Read) -> Frame {
    let mut head = [0u8; 4];
    reader.read_exact(&mut head).expect("client frame header");
    let head = u32::from_le_bytes(head);
    let mut payload = vec![0u8; (head & 0x00ff_ffff) as usize];
    reader
        .read_exact(&mut payload)
        .expect("client frame payload");
    ((head >> 24) as u8, payload)
}

/// The whole-file reply for `keep`: ndx, iflags, sum head, one literal run,
/// the end token and the MD5 file checksum (sender.c send_files()).
fn reply() -> Vec<u8> {
    let mut out = vec![0x01, 0x00, 0xa0];
    out.extend_from_slice(&[0u8; 16]);
    out.extend_from_slice(&(CONTENT.len() as i32).to_le_bytes());
    out.extend_from_slice(&CONTENT);
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&checksums::strong::Md5::digest(&CONTENT));
    out
}

/// Answers the greeting, module and arguments, sends the setup bytes and the
/// file list, and returns the reader once the client's filter list is in.
fn accept_and_send_list(listener: &TcpListener) -> (TcpStream, BufReader<TcpStream>) {
    let (mut sock, _) = listener.accept().expect("accept the client");
    // Only a regression waits this long: the client then never sends the
    // NDX_DONE this daemon waits for.
    sock.set_read_timeout(Some(Duration::from_secs(30)))
        .expect("read timeout");
    sock.write_all(b"@RSYNCD: 30.0\n").expect("greeting");
    let mut reader = BufReader::new(sock.try_clone().expect("clone socket"));
    let mut line = String::new();
    reader.read_line(&mut line).expect("client greeting");
    line.clear();
    reader.read_line(&mut line).expect("module line");
    sock.write_all(b"@RSYNCD: OK\n").expect("module accepted");
    let mut arg = Vec::new();
    loop {
        arg.clear();
        reader.read_until(0, &mut arg).expect("client argument");
        if arg == [0] {
            break;
        }
    }
    sock.write_all(&SETUP).expect("setup");
    reader
        .read_exact(&mut [0u8; CLIENT_FILTER_LIST])
        .expect("client filter list");
    sock.write_all(&frame(TAG_DATA, &FLIST)).expect("file list");
    (sock, reader)
}

/// Reads client frames until their data holds `len` bytes.
fn read_data(reader: &mut impl Read, len: usize) -> Vec<u8> {
    let mut data = Vec::new();
    while data.len() < len {
        let (tag, payload) = read_frame(reader);
        if tag == TAG_DATA {
            data.extend_from_slice(&payload);
        }
    }
    data
}

/// Pulls `keep` with `extra` options from a daemon that sends the file only
/// after the request phase has ended. Returns the client's data stream up to
/// that point and every frame it wrote afterwards.
fn pull_waiting_for_phase_end(extra: &[&str]) -> (Vec<u8>, Vec<Frame>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (tx, rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut sock, mut reader) = accept_and_send_list(&listener);
        let before_data = read_data(&mut reader, REQUEST.len() + 1);
        sock.write_all(&frame(TAG_DATA, &reply()))
            .expect("file data");
        sock.shutdown(Shutdown::Write).expect("half-close");
        let mut after = Vec::new();
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).expect("client output");
        let mut rest = rest.as_slice();
        while !rest.is_empty() {
            after.push(read_frame(&mut rest));
        }
        tx.send((before_data, after)).expect("report");
    });

    let tmp = TempDir::new().expect("tempdir");
    let output = Command::new(oc_rsync_bin())
        .args(["--protocol=30", "-r"])
        .args(extra)
        .arg(format!("rsync://127.0.0.1:{port}/mod/keep"))
        .arg(format!("{}/", tmp.path().display()))
        .stdin(Stdio::null())
        .output()
        .expect("run oc-rsync");
    server.join().expect("replayed daemon");
    let (before_data, after) = rx.recv().expect("daemon report");
    assert!(
        tmp.path().join("keep").exists(),
        "the client must still receive the file: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (before_data, after)
}

#[test]
fn request_phase_ends_before_any_file_data() {
    let (before_data, _) = pull_waiting_for_phase_end(&[]);
    let mut expected = REQUEST.to_vec();
    expected.push(NDX_DONE);
    assert_eq!(before_data, expected);
}

#[test]
fn source_removal_notice_follows_the_phase_end() {
    let (before_data, after) = pull_waiting_for_phase_end(&["--remove-source-files"]);
    let mut expected = REQUEST.to_vec();
    expected.push(NDX_DONE);
    assert_eq!(before_data, expected);
    assert_eq!(
        after.first(),
        Some(&(TAG_SUCCESS, 0i32.to_le_bytes().to_vec())),
        "MSG_SUCCESS for ndx 0 must be the client's next frame"
    );
}
