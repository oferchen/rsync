//! An rsh `--server` (receiver or sender) must forward a peer's `MSG_INFO`,
//! framed, to the client - never write it to its own stdout, which IS the protocol
//! stream.
//!
//! upstream: main.c:1085 `send_msgs_to_gen = am_server` - the server
//! receiver hands the frame to its generator (log.c:292-301), whose
//! `rwrite()` sends it to the client as `MSG_INFO` (log.c:357-366).
//!
//! The scripted peer is a recorded oc transfer with one `MSG_INFO` frame
//! spliced in ahead of its first multiplexed frame, replayed into a fresh
//! `oc-rsync --server`. The transcript is deterministic under the
//! `TranscriptRecorder` normalization, so the replayed server's output
//! must equal the recorded one plus exactly the forwarded frame: anything
//! else (the raw text on stdout, a missing frame, a desynchronised tail)
//! fails the cell.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

use test_support::transcript::{FIXED_CHECKSUM_LIST, TranscriptRecorder};

/// The peer text the scripted sender injects.
const PEER_TEXT: &[u8] = b"PEER-INFO-0195\n";

/// upstream: rsync.h:210 `MPLEX_BASE`; rsync.h:288 `MSG_INFO = FINFO = 2`.
const MPLEX_BASE: u8 = 7;
const MSG_INFO: u8 = 2;
const MSG_DATA: u8 = 0;

/// A multiplex frame: the 4-byte little-endian `(MPLEX_BASE + tag) << 24 |
/// len` header, then the payload (upstream: io.c:1173 send_msg()).
fn frame(tag: u8, payload: &[u8]) -> Vec<u8> {
    let len = u32::try_from(payload.len()).expect("payload fits a frame");
    let header = (u32::from(MPLEX_BASE + tag) << 24) | len;
    let mut out = header.to_le_bytes().to_vec();
    out.extend_from_slice(payload);
    out
}

/// A remote shell that records its argv (NUL-separated) and then execs the
/// teeing `capture-rsh` trampoline with it.
fn write_argv_rsh(dir: &Path, capture_rsh: &Path, argv_file: &Path) -> std::path::PathBuf {
    let script = dir.join("argv-rsh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > '{}'\nexec '{}' \"$@\"\n",
            argv_file.display(),
            capture_rsh.display()
        ),
    )
    .expect("write argv-rsh");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod argv-rsh");
    script
}

/// The server command the client asked the remote shell to run: leading
/// `-*` options and the host token dropped, as capture-rsh does.
fn server_command(argv_file: &Path) -> Vec<OsString> {
    let raw = fs::read(argv_file).expect("read recorded argv");
    let argv: Vec<OsString> = raw
        .split(|&b| b == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| OsString::from_vec(arg.to_vec()))
        .collect();
    let first = argv
        .iter()
        .position(|arg| !arg.to_string_lossy().starts_with('-'))
        .expect("host token");
    argv[first + 1..].to_vec()
}

/// Splices a `MSG_INFO` frame in front of the client's first multiplexed
/// frame.
///
/// The client->server stream opens with the raw 4-byte protocol version and
/// the checksum-name list (upstream: compat.c negotiate_the_strings(), one
/// vstring under the pinned `RSYNC_CHECKSUM_LIST`); every byte after that is
/// framed. The layout is asserted, not assumed, so a change fails loudly.
fn inject_info(c2s: &[u8]) -> Vec<u8> {
    let mut names = vec![u8::try_from(FIXED_CHECKSUM_LIST.len()).unwrap()];
    names.extend_from_slice(FIXED_CHECKSUM_LIST.as_bytes());
    let mux_start = 4 + names.len();
    assert_eq!(
        &c2s[4..mux_start],
        names.as_slice(),
        "client->server stream no longer opens version + checksum list"
    );
    assert_eq!(
        c2s[mux_start + 3],
        MPLEX_BASE + MSG_DATA,
        "the first client->server frame after negotiation must be MSG_DATA"
    );
    let mut out = c2s[..mux_start].to_vec();
    out.extend_from_slice(&frame(MSG_INFO, PEER_TEXT));
    out.extend_from_slice(&c2s[mux_start..]);
    out
}

fn occurrences(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    haystack
        .windows(needle.len())
        .enumerate()
        .filter_map(|(i, w)| (w == needle).then_some(i))
        .collect()
}

/// Which side the rsh server plays in the replayed transfer.
#[derive(Clone, Copy, Debug)]
enum ServerSide {
    /// A push: the server is the receiver.
    Receiver,
    /// A pull: the server is the sender.
    Sender,
}

/// Zeroes a pull server's end-of-transfer stats frame: the `MSG_DATA` frame
/// just before the trailing 5-byte `NDX_DONE` goodbye frame.
///
/// It carries `total_read`, `total_written`, `total_size` and the flist
/// timings (upstream: main.c:345-358 handle_stats()). The timings are wall
/// clock, `total_written` grows by the forwarded frame, and `total_read`
/// counts bytes read ahead from the socket, which a replay that delivers the
/// whole script at once inflates - so the frame is excluded from the
/// byte-identity check rather than predicted.
fn mask_sender_stats(stream: &mut [u8]) {
    const GOODBYE: [u8; 5] = [1, 0, 0, MPLEX_BASE + MSG_DATA, 0];
    let end = stream.len() - GOODBYE.len();
    assert_eq!(
        stream[end..],
        GOODBYE,
        "a pull must end with the NDX_DONE goodbye"
    );
    let start = (0..end.saturating_sub(4))
        .rev()
        .find(|&i| {
            let header = u32::from_le_bytes(stream[i..i + 4].try_into().unwrap());
            header >> 24 == u32::from(MPLEX_BASE + MSG_DATA)
                && (header & 0x00FF_FFFF) as usize == end - i - 4
        })
        .expect("stats frame before the goodbye");
    stream[start + 4..end].fill(0);
}

/// Records one rsh transfer, replays its client->server stream into a fresh
/// `--server` with a peer `MSG_INFO` spliced in, and asserts the server
/// framed that text to the client and changed nothing else.
fn assert_server_frames_peer_info(side: ServerSide) {
    let work = test_support::create_tempdir();
    let src = work.path().join("src");
    let dest = work.path().join("dest");
    let replay_dest = work.path().join("replay-dest");
    for dir in [&src, &dest, &replay_dest] {
        fs::create_dir(dir).expect("mkdir");
    }
    let content: Vec<u8> = (0..4096u32).map(|i| (i * 7 % 251) as u8).collect();
    fs::write(src.join("a.bin"), &content).expect("write source");

    let argv_file = work.path().join("server-argv");
    let rsh = write_argv_rsh(
        work.path(),
        Path::new(env!("CARGO_BIN_EXE_capture-rsh")),
        &argv_file,
    );
    let binary = test_support::oc_rsync_bin();
    let recorder = TranscriptRecorder::new(work.path());
    let mut cmd = recorder.command(&binary, &binary, &rsh);
    cmd.arg("-r");
    match side {
        ServerSide::Receiver => cmd
            .arg(format!("{}/", src.display()))
            .arg(format!("replayhost:{}", dest.display())),
        ServerSide::Sender => cmd.arg(format!("replayhost:{}/", src.display())).arg(&dest),
    };
    let output = cmd.output().expect("run recording transfer");
    assert!(
        output.status.success(),
        "recording {side:?} transfer failed: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let recorded = recorder.finish().expect("read captures");

    let dest_arg = OsString::from(dest.as_os_str());
    let server: Vec<OsString> = server_command(&argv_file)
        .into_iter()
        .map(|arg| {
            if arg == dest_arg {
                replay_dest.clone().into_os_string()
            } else {
                arg
            }
        })
        .collect();
    let mut child = Command::new(&server[0])
        .args(&server[1..])
        .env("RSYNC_CHECKSUM_LIST", FIXED_CHECKSUM_LIST)
        .env_remove("RSYNC_COMPRESS_LIST")
        .env_remove("RSYNC_PROTECT_ARGS")
        .env_remove("OC_CONSECUTIVE_MATCH")
        .env_remove("OC_RSYNC_LAZY_FLIST")
        .env_remove("OC_RSYNC_PULL_INC_RECURSE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn replay server");
    let mut stdin = child.stdin.take().expect("piped stdin");
    let script = inject_info(&recorded.client_to_server);
    // A server that aborts early stops reading; the write error is then the
    // expected consequence, and the status below reports the real failure.
    let writer = thread::spawn(move || {
        let _ = stdin.write_all(&script);
    });
    let mut replayed = Vec::new();
    child
        .stdout
        .take()
        .expect("piped stdout")
        .read_to_end(&mut replayed)
        .expect("read server stdout");
    let result = child.wait_with_output().expect("wait replay server");
    writer.join().expect("stdin writer");
    assert!(
        result.status.success(),
        "replay {side:?} server failed: {}\nstderr: {}",
        result.status,
        String::from_utf8_lossy(&result.stderr)
    );
    if let ServerSide::Receiver = side {
        assert_eq!(
            fs::read(replay_dest.join("a.bin")).expect("replayed file"),
            content
        );
    }

    let forwarded = frame(MSG_INFO, PEER_TEXT);
    let framed_at = occurrences(&replayed, &forwarded);
    assert_eq!(
        framed_at.len(),
        1,
        "{side:?}: the peer MSG_INFO must reach the client as exactly one MSG_INFO frame"
    );
    assert_eq!(
        occurrences(&replayed, PEER_TEXT),
        vec![framed_at[0] + 4],
        "{side:?}: the peer text must appear only inside that frame, never raw on stdout"
    );
    let mut left = replayed;
    left.drain(framed_at[0]..framed_at[0] + forwarded.len());
    let mut right = recorded.server_to_client;
    if let ServerSide::Sender = side {
        mask_sender_stats(&mut left);
        mask_sender_stats(&mut right);
    }
    assert!(
        left == right,
        "{side:?}: removing the forwarded frame must leave the recorded \
         server->client stream ({} vs {} bytes)",
        left.len(),
        right.len()
    );
}

/// WHY: on an rsh server, stdout is the stream. A peer's `MSG_INFO` written
/// there raw corrupts the client's demultiplexer; upstream's receiver hands
/// it to its generator, which frames it to the client (main.c:1085,
/// log.c:292-301, 357-366).
#[test]
fn rsh_server_receiver_frames_a_peer_msg_info_to_the_client() {
    assert_server_frames_peer_info(ServerSide::Receiver);
}

/// WHY: the same holds for an rsh server sender: its `rwrite()` is
/// `am_server`, so a peer `MSG_INFO` goes back to the client as a frame
/// (log.c:357-366), not to the stream raw.
#[test]
fn rsh_server_sender_frames_a_peer_msg_info_to_the_client() {
    assert_server_frames_peer_info(ServerSide::Sender);
}
