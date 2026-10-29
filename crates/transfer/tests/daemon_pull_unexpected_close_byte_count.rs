//! A daemon pull whose server stream is cut off must name the same byte count
//! as upstream in "connection unexpectedly closed (N bytes received so far)",
//! and every way the stream can end must exit by upstream's class: end of
//! stream 12, a reset socket 10, a silent peer 30, a destination that cannot
//! be created 11.
//!
//! upstream: io.c:938 adds to `stats.total_read` only inside `perform_io()`,
//! which runs once `client_run()` starts multiplexed or buffered input
//! (main.c:1378-1381).
//! `setup_protocol()` reads the compat flags and checksum seed before that,
//! through `safe_read()`, so those bytes are never counted. oc used to count
//! them, and reported 5 bytes more than upstream for every cut.
//!
//! The expected counts were measured with rsync 3.5.1 as the client against
//! the same replayed stream: 0, 10, 22 and 26 bytes for the cuts below.
//!
//! Who reports the cut depends on upstream's fork (main.c:1077 do_recv()).
//! Inside the file list the client is still the lone `Receiver`; once the
//! list is in, the receiver child whines first and the generator, which reads
//! nothing more from the peer, whines with the count it inherited
//! (rsync.c:987-995 who_am_i(), cleanup.c:240-256).
//!
//! Every expected report below was printed by rsync 3.5.1 as the client
//! against the same replayed stream.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::Path;
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
/// The client's empty filter list: one `MSG_DATA` frame carrying a zero int
/// (exclude.c send_filter_list()).
const CLIENT_FILTER_LIST: usize = 8;
/// The generator's request for `keep` once the list is in: one `MSG_DATA`
/// frame of ndx, iflags and an empty sum head (generator.c recv_generator()).
const CLIENT_FILE_REQUEST: usize = 23;

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

/// How the replayed daemon ends its stream after the cut.
#[derive(Clone, Copy)]
enum End {
    /// Half-close, then drain the client: a clean end of stream.
    Eof,
    /// Close with a zero linger, so the client's next read is ECONNRESET.
    Reset,
    /// Send nothing more and wait for the client to give up.
    Stall,
}

/// Answers the client's greeting, module and arguments, then sends the setup
/// bytes once it has read them; returns the reader for the rest.
fn accept_and_set_up(listener: TcpListener) -> (TcpStream, BufReader<TcpStream>) {
    let (mut sock, _) = listener.accept().expect("accept the client");
    sock.set_read_timeout(Some(Duration::from_secs(30)))
        .expect("read timeout");
    sock.write_all(b"@RSYNCD: 30.0\n").expect("greeting");
    let mut reader = BufReader::new(sock.try_clone().expect("clone socket"));
    let mut line = String::new();
    reader.read_line(&mut line).expect("client greeting");
    line.clear();
    reader.read_line(&mut line).expect("module line");

    sock.write_all(b"@RSYNCD: OK\n").expect("module accepted");
    // Protocol 30 arguments are NUL-terminated, closed by an empty one.
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
    (sock, reader)
}

/// Plays a daemon that sends the setup bytes plus `cut` bytes of the stream,
/// then ends it as `end` says.
///
/// Each step waits for what the client writes before it, so a reset never
/// lands on a client write: the client meets the end of the stream on a read,
/// at exactly `cut`, however the two processes are scheduled.
fn serve_cut_stream(listener: TcpListener, cut: usize, end: End) -> JoinHandle<()> {
    thread::spawn(move || {
        let (mut sock, mut reader) = accept_and_set_up(listener);
        sock.write_all(&post_setup_stream()[..cut])
            .expect("cut stream");
        match end {
            End::Eof => sock.shutdown(Shutdown::Write).expect("half-close"),
            End::Reset => {
                if cut >= FLIST.len() + 4 {
                    reader
                        .read_exact(&mut [0u8; CLIENT_FILE_REQUEST])
                        .expect("client file request");
                }
                socket2::SockRef::from(&sock)
                    .set_linger(Some(Duration::ZERO))
                    .expect("zero linger");
                drop(reader);
                drop(sock);
                return;
            }
            End::Stall => {}
        }

        let mut sink = [0u8; 4096];
        while matches!(reader.read(&mut sink), Ok(n) if n > 0) {}
    })
}

fn pull_against_cut_stream(cut: usize) -> (Option<i32>, String) {
    let tmp = TempDir::new().expect("tempdir");
    pull_into(cut, End::Eof, &[], &format!("{}/", tmp.path().display()))
}

/// Pulls `keep` into `dest` against a stream cut at `cut` and ended as `end`.
fn pull_into(cut: usize, end: End, extra: &[&str], dest: &str) -> (Option<i32>, String) {
    pull_from(|listener| serve_cut_stream(listener, cut, end), extra, dest)
}

/// Pulls `keep` into `dest` from the replayed daemon `serve` starts.
fn pull_from(
    serve: impl FnOnce(TcpListener) -> JoinHandle<()>,
    extra: &[&str],
    dest: &str,
) -> (Option<i32>, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = serve(listener);

    let mut child = Command::new(oc_rsync_bin())
        .args(["--protocol=30", "-r"])
        .args(extra)
        .arg(format!("rsync://127.0.0.1:{port}/mod/keep"))
        .arg(dest)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn oc-rsync");
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().expect("poll client").is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("the client hung against the replayed daemon");
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

/// The diagnostic lines a stream cut at `cut` bytes leaves, as rsync 3.5.1
/// prints them, with each trailer's source location dropped.
fn upstream_report(cut: usize) -> Vec<String> {
    let whine =
        |who: &str| format!("connection unexpectedly closed ({cut} bytes received so far) [{who}]");
    let stream = |who: &str| trailer("error in rsync protocol data stream", 12, who);
    if cut < FLIST.len() + 4 {
        vec![whine("Receiver"), stream("Receiver")]
    } else {
        vec![
            whine("receiver"),
            stream("receiver"),
            whine("generator"),
            stream("generator"),
        ]
    }
}

/// Reduces the client's stderr to what [`upstream_report`] compares: the text
/// after the program name, with ` at <file>(<line>)` and the version removed.
/// A line upstream prints without the program name is kept whole.
fn report_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| match line.split_once(": ") {
            Some((program, rest)) if program.contains("rsync") => rest,
            _ => line,
        })
        .map(|rest| match rest.split_once(" at ") {
            Some((head, tail)) => {
                let who = tail.split_once('[').map_or("", |(_, who)| who);
                let who = who.split_once('=').map_or(who, |(role, _)| role);
                format!("{head} [{who}=")
            }
            None => rest.to_owned(),
        })
        .collect()
}

/// Cuts at the start, inside the file-list frame, at its end, and after the
/// next frame header. The setup bytes must never be part of the count, and
/// each cut is reported by the process upstream would report it from.
#[test]
fn a_cut_off_daemon_stream_reports_upstreams_byte_count() {
    for cut in [0, 10, 22, 26] {
        let (code, text) = pull_against_cut_stream(cut);
        assert_eq!(code, Some(12), "cut at {cut}: {text}");
        assert_eq!(
            report_lines(&text),
            upstream_report(cut),
            "cut at {cut}: {text}"
        );
    }
}

/// The text a cut leaves for a report of `code` named `name`, as
/// [`report_lines`] reduces it.
fn trailer(name: &str, code: i32, who: &str) -> String {
    format!("{name} (code {code}) [{who}=")
}

/// upstream: io.c:930-946 perform_io() - a reset socket is the reading
/// process's `read error` and RERR_SOCKETIO. Inside the file list that is the
/// lone `Receiver`, and the run exits 10. After it the receiver child reports
/// the reset, and the generator, seeing its error pipe close, whines and
/// exits 12 (cleanup.c:240-256).
///
/// Linux delivers the bytes already queued before the reset, so the cut is
/// exact; other kernels may drop them, which would move the cut.
#[cfg(target_os = "linux")]
#[test]
fn a_reset_daemon_stream_exits_by_the_phase_it_struck() {
    let reset = "read error: Connection reset by peer (104)";
    let socket = "error in socket IO";
    let stream = "error in rsync protocol data stream";
    for (cut, expected) in [
        (
            10,
            vec![
                format!("[Receiver] {reset}"),
                trailer(socket, 10, "Receiver"),
            ],
        ),
        (
            26,
            vec![
                format!("[receiver] {reset}"),
                trailer(socket, 10, "receiver"),
                "connection unexpectedly closed (26 bytes received so far) [generator]".to_owned(),
                trailer(stream, 12, "generator"),
            ],
        ),
    ] {
        let tmp = TempDir::new().expect("tempdir");
        let dest = format!("{}/", tmp.path().display());
        let (code, text) = pull_into(cut, End::Reset, &[], &dest);
        let exit = if cut < FLIST.len() + 4 { 10 } else { 12 };
        assert_eq!(code, Some(exit), "cut at {cut}: {text}");
        assert_eq!(report_lines(&text), expected, "cut at {cut}: {text}");
    }
}

/// upstream: io.c:256-266 check_timeout() - a peer silent past --timeout is
/// RERR_TIMEOUT (30). Before the fork the lone `Receiver` times out. After it
/// the receiver never does: the generator times out, and its exit SIGUSR1s the
/// receiver, whose own trailer prints last (cleanup.c:202-203, main.c:1684).
#[test]
fn a_silent_daemon_times_out_from_the_process_upstream_names() {
    let timeout = "io timeout after 1 seconds -- exiting";
    let name = "timeout in data send/receive";
    for (cut, expected) in [
        (
            10,
            vec![
                format!("[Receiver] {timeout}"),
                trailer(name, 30, "Receiver"),
            ],
        ),
        (
            26,
            vec![
                format!("[generator] {timeout}"),
                trailer(name, 30, "generator"),
                trailer("received SIGUSR1", 19, "receiver"),
            ],
        ),
    ] {
        let tmp = TempDir::new().expect("tempdir");
        let dest = format!("{}/", tmp.path().display());
        let (code, text) = pull_into(cut, End::Stall, &["--timeout=1"], &dest);
        assert_eq!(code, Some(30), "cut at {cut}: {text}");
        assert_eq!(report_lines(&text), expected, "cut at {cut}: {text}");
    }
}

/// upstream: main.c:817-821 get_local_name() - once the list is in, a
/// destination root the client cannot create is the lone `Receiver`'s
/// `mkdir ... failed` and RERR_FILEIO (11), before any fork.
#[test]
fn an_uncreatable_destination_root_exits_with_file_io() {
    let tmp = TempDir::new().expect("tempdir");
    let parent = tmp.path().join("ro");
    fs::create_dir(&parent).expect("mkdir ro");
    set_mode(&parent, 0o555);
    if fs::write(parent.join("probe"), b"").is_ok() {
        set_mode(&parent, 0o755);
        eprintln!("skip: the 0555 parent stays writable here (running as root)");
        return;
    }
    let target = parent.join("inner");
    let (code, text) = pull_into(
        FLIST.len() + 4,
        End::Eof,
        &[],
        &format!("{}/", target.display()),
    );
    set_mode(&parent, 0o755);
    assert_eq!(code, Some(11), "{text}");
    assert_eq!(
        report_lines(&text),
        [
            format!(
                "[Receiver] mkdir \"{}\" failed: Permission denied (13)",
                target.display()
            ),
            trailer("error in file IO", 11, "Receiver"),
        ],
        "{text}"
    );
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
}

/// Plays a daemon that sends the file list and, once asked, all of `keep`
/// (38 literal bytes and their MD5), then ends the stream.
fn serve_one_file(listener: TcpListener) -> JoinHandle<()> {
    thread::spawn(move || {
        let (mut sock, mut reader) = accept_and_set_up(listener);
        sock.write_all(&post_setup_stream()[..FLIST.len() + 4])
            .expect("file list");
        reader
            .read_exact(&mut [0u8; CLIENT_FILE_REQUEST])
            .expect("client file request");
        let data: Vec<u8> = (0..38u8).collect();
        let mut reply = vec![0x01, 0x00, 0xa0];
        reply.extend_from_slice(&[0u8; 16]);
        reply.extend_from_slice(&38i32.to_le_bytes());
        reply.extend_from_slice(&data);
        reply.extend_from_slice(&0i32.to_le_bytes());
        reply.extend_from_slice(&checksums::strong::Md5::digest(&data));
        let mut frame = ((MSG_DATA_TAG << 24) | reply.len() as u32)
            .to_le_bytes()
            .to_vec();
        frame.extend_from_slice(&reply);
        sock.write_all(&frame).expect("file data");
        sock.shutdown(Shutdown::Write).expect("half-close");
        let mut sink = [0u8; 4096];
        while matches!(reader.read(&mut sink), Ok(n) if n > 0) {}
    })
}

/// upstream: receiver.c:1341-1347 - a received file costs the generator one
/// 8-byte MSG_SUCCESS when --remove-source-files is on, and nothing
/// otherwise, so only the generator's count moves. Measured with rsync 3.5.1
/// against this stream: the receiver read 107 bytes either way, the generator
/// 22 without the option and 30 with it.
#[test]
fn a_received_file_costs_the_generator_one_notice_under_remove_source_files() {
    let stream = |who: &str| trailer("error in rsync protocol data stream", 12, who);
    let whine = |count: u64, who: &str| {
        format!("connection unexpectedly closed ({count} bytes received so far) [{who}]")
    };
    for (extra, generator) in [(&[][..], 22), (&["--remove-source-files"][..], 30)] {
        let tmp = TempDir::new().expect("tempdir");
        let dest = format!("{}/", tmp.path().display());
        let (code, text) = pull_from(serve_one_file, extra, &dest);
        assert_eq!(code, Some(12), "{extra:?}: {text}");
        assert_eq!(
            report_lines(&text),
            [
                whine(107, "receiver"),
                stream("receiver"),
                whine(generator, "generator"),
                stream("generator"),
            ],
            "{extra:?}: {text}"
        );
    }
}
