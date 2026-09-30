//! A protocol-30 client pulling with `--files-from` from an oc server sender.
//!
//! Below protocol 31 a pulling client forwards its local `--files-from` names
//! to the sender WITHOUT multiplex framing, even though the rest of its output
//! (the filter list included) is multiplexed. The sender must read those names
//! raw and only then resume demultiplexing. Parsing the names as frame headers
//! makes the sender wait for a frame that never comes, and the pull hangs.
//!
//! The peer below replays, byte for byte, what an rsync 3.0.9 client sends for
//! `rsync -a --files-from=LIST host:src/ dst/` (captured with strace against an
//! upstream 3.5.1 server). No real 3.0.9 binary is needed. A read timeout on
//! the peer socket is the watchdog that turns a hang into a failure.
//!
//! # Upstream Reference
//!
//! - 3.0.9 `main.c:978` start_server() - input multiplexed for protocol >= 30.
//! - 3.0.9 `io.c:685-741` read_timeout() - the client forwards the names raw,
//!   newlines rewritten to NULs, then a lone NUL as the end marker.
//! - 3.0.9 `io.c:797` read_line() - the sender reads them with a raw `read()`.
//! - 3.5.1 `io.c:1400-1405` start_filesfrom_forwarding() and
//!   `flist.c:2792-2798` send_file_list() - `MPLX_TO_BUFFERED` below 31.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::io::{BufReader, ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::{Duration, Instant};

use protocol::ProtocolVersion;
use transfer::{ServerConfig, ServerRole, perform_handshake_with_max, run_server_with_handshake};

const WATCHDOG: Duration = Duration::from_secs(20);
/// Silence after the last expected name that marks the end of the file list:
/// with no INC_RECURSE the sender next waits for the receiver's first NDX.
const QUIET: Duration = Duration::from_millis(750);
const MSG_DATA_TAG: u8 = 7;

/// 3.0.9 client bytes after the handshake: the filter-list terminator in a
/// `MSG_DATA` frame, then the raw NUL-separated names and the lone-NUL end.
fn client_script(names: &[&str]) -> Vec<u8> {
    let mut bytes = vec![0x04, 0x00, 0x00, MSG_DATA_TAG, 0, 0, 0, 0];
    for name in names {
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
    }
    bytes.push(0);
    bytes
}

/// Reads `MSG_DATA` payload from the sender until every `expected` name is
/// present and the stream then stays quiet, or the watchdog expires.
fn read_file_list(peer: &mut UnixStream, expected: &[&str]) -> Result<Vec<u8>, String> {
    let start = Instant::now();
    let mut payload = Vec::new();
    loop {
        let all_seen = expected.iter().all(|n| contains(&payload, n.as_bytes()));
        let wait = if all_seen {
            QUIET
        } else {
            WATCHDOG.saturating_sub(start.elapsed())
        };
        if wait.is_zero() {
            return Err(format!(
                "watchdog: no complete file list within {WATCHDOG:?} (got {} payload bytes)",
                payload.len()
            ));
        }
        peer.set_read_timeout(Some(wait)).expect("set read timeout");
        let mut header = [0u8; 4];
        match peer.read_exact(&mut header) {
            Ok(()) => {}
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                if all_seen {
                    return Ok(payload);
                }
                continue;
            }
            Err(e) => return Err(format!("sender closed before the file list: {e}")),
        }
        let len = u32::from_le_bytes([header[0], header[1], header[2], 0]) as usize;
        let mut frame = vec![0u8; len];
        peer.set_read_timeout(Some(WATCHDOG))
            .expect("set read timeout");
        peer.read_exact(&mut frame)
            .map_err(|e| format!("truncated frame: {e}"))?;
        if header[3] == MSG_DATA_TAG {
            payload.extend_from_slice(&frame);
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// The names forwarded raw at protocol 30 must reach the sender as a file
/// list: every listed name is sent, and the bait file that only a walk
/// ignoring the list would find is not. Before the fix the sender parsed
/// `small.txt\0...` as a multiplex header and never produced a file list.
#[test]
fn proto30_pull_reads_forwarded_files_from_names_unframed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    fs::create_dir_all(src.join("sub/deep")).expect("mkdir");
    fs::write(src.join("small.txt"), b"small\n").expect("write");
    fs::write(src.join("sub/deep/b.txt"), b"b\n").expect("write");
    fs::write(src.join("with space.txt"), b"space\n").expect("write");
    fs::write(src.join("bait-unlisted.txt"), b"bait\n").expect("write");

    // The server argv a 3.0.9 client sends: `--server --sender -logDtpRe.Lsf
    // --files-from=- --from0 . src/`.
    let mut src_arg = OsString::from(src.as_os_str());
    src_arg.push("/");
    let mut config = ServerConfig::from_flag_string_and_args(
        ServerRole::Generator,
        "-logDtpRe.Lsf".to_owned(),
        vec![src_arg],
    )
    .expect("sender config");
    config.file_selection.files_from_path = Some("-".to_owned());
    config.file_selection.from0 = true;

    let (server_sock, mut peer) = UnixStream::pair().expect("socket pair");
    let server = thread::spawn(move || {
        let mut reader = BufReader::new(server_sock.try_clone().expect("clone"));
        let handshake =
            perform_handshake_with_max(&mut reader, &mut &server_sock, ProtocolVersion::NEWEST)?;
        run_server_with_handshake(
            config,
            handshake,
            &mut reader,
            &server_sock,
            None,
            None,
            None,
        )
        .map(|_| ())
    });

    // Version exchange, then the sender's compat flags varint and seed.
    peer.set_read_timeout(Some(WATCHDOG))
        .expect("set read timeout");
    peer.write_all(&30u32.to_le_bytes()).expect("send version");
    let mut server_version = [0u8; 4];
    peer.read_exact(&mut server_version).expect("read version");
    let mut compat_and_seed = [0u8; 5];
    peer.read_exact(&mut compat_and_seed)
        .expect("read compat+seed");
    assert_eq!(
        compat_and_seed[0] & 0x80,
        0,
        "compat flags must fit one varint byte"
    );

    let names = ["small.txt", "sub/deep/b.txt", "with space.txt"];
    peer.write_all(&client_script(&names)).expect("send script");

    let outcome = read_file_list(&mut peer, &["small.txt", "b.txt", "with space.txt"]);
    let _ = peer.shutdown(Shutdown::Both);
    let _ = server.join();

    let payload = outcome.unwrap_or_else(|e| panic!("{e}"));
    assert!(
        !contains(&payload, b"bait-unlisted.txt"),
        "the sender walked the source instead of honouring the forwarded list"
    );
}
