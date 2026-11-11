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
//!
//! upstream: generator.c:2853-2866 - once the sender has answered that
//! `NDX_DONE`, the generator ends the redo phase and, without
//! `--delay-updates`, the delay phase straight away. rsync 3.5.1 writes both
//! `NDX_DONE`s before the sender sends anything more; oc used to wait for the
//! sender's answer to each.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Command, Output, Stdio};
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
    reply_with_checksum(&checksums::strong::Md5::digest(&CONTENT))
}

/// [`reply`] with `checksum` as the file checksum.
fn reply_with_checksum(checksum: &[u8]) -> Vec<u8> {
    let mut out = vec![0x01, 0x00, 0xa0];
    out.extend_from_slice(&[0u8; 16]);
    out.extend_from_slice(&(CONTENT.len() as i32).to_le_bytes());
    out.extend_from_slice(&CONTENT);
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(checksum);
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

    let (tmp, output) = pull(port, extra);
    server.join().expect("replayed daemon");
    let (before_data, after) = rx.recv().expect("daemon report");
    assert!(
        tmp.path().join("keep").exists(),
        "the client must still receive the file: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (before_data, after)
}

/// Pulls `keep` with `extra` options from the daemon listening on `port`.
fn pull(port: u16, extra: &[&str]) -> (TempDir, Output) {
    let tmp = TempDir::new().expect("tempdir");
    let output = pull_into(&tmp, port, extra);
    (tmp, output)
}

/// Pulls `keep` into `dest` with `extra` options from the daemon on `port`.
fn pull_into(dest: &TempDir, port: u16, extra: &[&str]) -> Output {
    Command::new(oc_rsync_bin())
        .args(["--protocol=30", "-r"])
        .args(extra)
        .arg(format!("rsync://127.0.0.1:{port}/mod/keep"))
        .arg(format!("{}/", dest.path().display()))
        .stdin(Stdio::null())
        .output()
        .expect("run oc-rsync")
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

#[test]
fn redo_and_delay_phases_end_once_the_sender_ends_phase_zero() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = thread::spawn(move || {
        let (mut sock, mut reader) = accept_and_send_list(&listener);
        read_data(&mut reader, REQUEST.len() + 1);
        let mut answer = reply();
        answer.push(NDX_DONE);
        sock.write_all(&frame(TAG_DATA, &answer))
            .expect("file data and the sender's phase end");
        let phase_ends = read_data(&mut reader, 2);
        sock.shutdown(Shutdown::Both).expect("close");
        phase_ends
    });
    // The daemon hangs up mid-transfer, so only the bytes it read matter.
    pull(port, &[]);
    assert_eq!(
        server.join().expect("replayed daemon"),
        [NDX_DONE, NDX_DONE]
    );
}

/// Serves `keep` with a wrong file checksum after the client's request of
/// `request_len` bytes and its phase-0 `NDX_DONE`, then the sender's phase-0
/// `NDX_DONE`, and returns the next `redo_len` data bytes the client writes.
fn redo_after_failed_checksum(dest: &TempDir, request_len: usize, redo_len: usize) -> Vec<u8> {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = thread::spawn(move || {
        let (mut sock, mut reader) = accept_and_send_list(&listener);
        read_data(&mut reader, request_len + 1);
        let mut answer = reply_with_checksum(&[0u8; 16]);
        answer.push(NDX_DONE);
        sock.write_all(&frame(TAG_DATA, &answer))
            .expect("corrupt file data and the sender's phase end");
        let redo = read_data(&mut reader, redo_len);
        sock.shutdown(Shutdown::Both).expect("close");
        redo
    });
    // The daemon hangs up mid-transfer, so only the bytes it read matter.
    pull_into(dest, port, &[]);
    server.join().expect("replayed daemon")
}

/// upstream: generator.c:2853-2866 - the redo phase ends right after its last
/// request. rsync 3.5.1 answers a failed whole-file checksum by re-requesting
/// `keep` - the same index (`0xFE` and a zero diff), `ITEM_TRANSFER |
/// ITEM_IS_NEW` because the discarded temp leaves the destination absent
/// (generator.c:584), and an empty sum head - and writes the redo and delay
/// phases' `NDX_DONE`s in the same frame, before any redo reply.
#[test]
fn redo_phase_ends_right_after_its_request() {
    let dest = TempDir::new().expect("tempdir");
    let redo = redo_after_failed_checksum(&dest, REQUEST.len(), 3 + REQUEST.len() - 1 + 2);
    let mut expected = vec![0xfe, 0x00, 0x00];
    expected.extend_from_slice(&REQUEST[1..]);
    expected.extend_from_slice(&[NDX_DONE, NDX_DONE]);
    assert_eq!(redo, expected);
}

/// upstream: generator.c:2667-2705 - the redo re-enters recv_generator(),
/// which re-stats the destination and itemizes the request against it
/// (generator.c:2378-2384). An existing `keep` that differs in size and
/// mtime is not new: rsync 3.5.1 sends `ITEM_TRANSFER | ITEM_REPORT_SIZE |
/// ITEM_REPORT_TIME` (`0x800c`) on the redo, as on the first request.
#[test]
fn redo_request_itemizes_an_existing_destination() {
    let dest = TempDir::new().expect("tempdir");
    let existing = dest.path().join("keep");
    std::fs::write(&existing, b"hello").expect("existing destination");
    filetime::set_file_mtime(
        &existing,
        filetime::FileTime::from_unix_time(1_000_000_000, 0),
    )
    .expect("backdate destination");
    // The first request's sum head and one 5-byte block: a 4-byte rolling sum
    // and a 2-byte strong sum. The redo's block carries a full 16-byte one.
    let request_len = 1 + 2 + 16 + 4 + 2;
    let redo_len = 3 + 2 + 16 + 4 + 16 + 2;
    let redo = redo_after_failed_checksum(&dest, request_len, redo_len);
    assert_eq!(redo[..3], [0xfe, 0x00, 0x00], "the redo request for ndx 0");
    assert_eq!(
        redo[3..5],
        [0x0c, 0x80],
        "ITEM_TRANSFER | ITEM_REPORT_SIZE | ITEM_REPORT_TIME"
    );
    assert_eq!(
        redo[redo_len - 2..],
        [NDX_DONE, NDX_DONE],
        "the redo and delay phases end before any redo reply"
    );
}
