//! The `--server --sender` output is framed exactly as rsync 3.5.1 frames it.
//!
//! Upstream buffers data in `iobuf.out` (io.c:1567) and messages in
//! `iobuf.msg` (io.c:2688), writes the messages first (io.c:628-636), and
//! writes anything only when a write does not fit (io.c:2487-2488), when
//! perform_io() is about to wait for input (io.c:766-768), or at an explicit
//! io_flush() (main.c:1007, main.c:1011). Where the frame boundaries fall is
//! therefore a function of the client's input alone - provided that input is
//! all available up front and never ends early. This test replays a recorded
//! client stream that way and compares the server's frame lengths with the
//! ones the 3.5.1 tarball build produced for the same input and tree.
//!
//! Recording (3.5.1 tarball build on both ends, the fixture below):
//! `rsync -rtc --no-inc-recursive --checksum-seed=1 -e <capturing rsh> h:src/ dst/`.
//! Recording again with `-vv` added gives the same client stream.
//! Replaying the captured client stream into the 3.5.1 server, stdin held
//! open, gives the same frames on every run, with and without `-vv`.

#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use filetime::{FileTime, set_file_times};

/// The 3.5.1 client's half of the recorded session.
const CLIENT_STREAM: &str = concat!(
    "210000001e7878683132382078786833207878683634206d6435206d64342073686131",
    "040000070000000003000007010060500000070102a00000000000000000000000000000",
    "00000102a0000000000000000000000000000000000102a0000000000000000000000000",
    "000000000100600102a00000000000000000000000000000000000030000070000000100",
    "000700",
);

/// The 3.5.1 server's reply to [`CLIENT_STREAM`]: `(tag, payload length)`
/// per frame, `0` being `MSG_DATA`.
///
/// Upstream `send_file_list()` streams `flist` entries in `readdir()` order
/// before `flist_sort_and_clean()`, so `XMIT_SAME_MODE` / `XMIT_SAME_NAME`
/// compression on the 4-entry `src/` fixture yields a first frame of `63056`
/// (when `d` precedes `f1..f3`), `63064` (when `d` follows `f1..f3`, as in the
/// original 3.5.1 capture), or `63065` (when `d` splits `f1..f3`). The
/// subsequent frames are invariant across all `readdir()` orders.
const VALID_FIRST_FRAME_LENS: [usize; 3] = [63056, 63064, 63065];
const UPSTREAM_TAIL_FRAMES: [(u32, usize); 2] = [(0, 37259), (0, 16)];

/// The same reply from a `-vv` server, which queues the `match_report()`
/// line (match.c:480-488) as `MSG_INFO` (tag 2) after the last file. 3.5.1
/// writes it ahead of the data it buffered before the line, because the
/// message queue leaves first (io.c:628-636), and the client stream is
/// byte-for-byte the same as without `-vv`.
const UPSTREAM_TAIL_FRAMES_VV: [(u32, usize); 3] = [(2, 58), (0, 37259), (0, 16)];

/// The recorded tree: four files whose sizes put two literal chunks of
/// `d/big` across the first frame boundary.
const FILES: [(&str, usize); 4] = [("f1", 5000), ("f2", 10000), ("f3", 15000), ("d/big", 70000)];

/// 2026-01-01T00:00:00Z, the mtime of every fixture entry.
const MTIME: i64 = 1_767_225_600;

/// One multiplex frame header: `(MPLEX_BASE + tag) << 24 | len`, little-endian
/// (upstream: io.c:1173 send_msg()).
const MPLEX_BASE: u32 = 7;

fn decode_hex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
        .collect()
}

fn build_fixture(root: &Path) {
    fs::create_dir_all(root.join("d")).expect("mkdir");
    for (i, (name, size)) in FILES.iter().enumerate() {
        let content: Vec<u8> = (0..*size).map(|j| (j * 31 + i) as u8).collect();
        let path = root.join(name);
        fs::write(&path, content).expect("write fixture file");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod file");
    }
    let mtime = FileTime::from_unix_time(MTIME, 0);
    for name in FILES.iter().map(|(name, _)| *name).chain(["d", ""]) {
        let path = root.join(name);
        if path.is_dir() {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod dir");
        }
        set_file_times(&path, mtime, mtime).expect("set mtime");
    }
}

/// The `(tag, len)` of each frame in `stream` after its unframed handshake.
///
/// The handshake has no framing, so the frames start at the smallest offset
/// from which whole frames parse exactly to the end of the stream.
fn frames(stream: &[u8]) -> Vec<(u32, usize)> {
    (0..stream.len())
        .find_map(|start| parse_frames(&stream[start..]))
        .expect("no multiplex frame boundary in the server output")
}

fn parse_frames(mut rest: &[u8]) -> Option<Vec<(u32, usize)>> {
    let mut frames = Vec::new();
    while !rest.is_empty() {
        let word = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?);
        let tag = (word >> 24).checked_sub(MPLEX_BASE)?;
        let len = (word & 0x00FF_FFFF) as usize;
        rest = rest.get(4 + len..)?;
        frames.push((tag, len));
    }
    (frames.len() > 1).then_some(frames)
}

/// Replays [`CLIENT_STREAM`] into `oc-rsync --server --sender` with `flags`
/// and returns the `(tag, length)` of each frame it wrote.
fn replay_frames(flags: &str) -> Vec<(u32, usize)> {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    build_fixture(&src);

    let mut child = Command::new(env!("CARGO_BIN_EXE_oc-rsync"))
        .args(["--server", "--sender", flags, "--checksum-seed=1", "."])
        .arg(format!("{}/", src.display()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn oc-rsync --server --sender");

    // The whole client stream is available before the server reads any of
    // it, and stdin stays open until the server has exited, so no read ever
    // waits or ends early and the flush points depend on the input alone.
    let mut stdin = child.stdin.take().expect("stdin");
    stdin
        .write_all(&decode_hex(CLIENT_STREAM))
        .expect("write client stream");
    let mut reply = Vec::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_end(&mut reply)
        .expect("read server output");
    let status = child.wait().expect("wait for server");
    drop(stdin);
    assert!(status.success(), "server exited with {status}");
    frames(&reply)
}

fn assert_frames_match_upstream(flags: &str, expected_tail: &[(u32, usize)]) {
    let actual = replay_frames(flags);
    let (first, tail) = actual
        .split_first()
        .expect("server must emit at least one frame");
    assert_eq!(
        first.0, 0,
        "first frame must be MSG_DATA; got frames {actual:?}"
    );
    assert!(
        VALID_FIRST_FRAME_LENS.contains(&first.1),
        "first frame length {} differs from rsync 3.5.1's valid readdir-order lengths {VALID_FIRST_FRAME_LENS:?}; frames={actual:?}",
        first.1
    );
    assert_eq!(
        tail, expected_tail,
        "the server's tail frames differ from rsync 3.5.1's"
    );
}

#[test]
fn server_sender_frames_match_rsync_3_5_1() {
    assert_frames_match_upstream("-trce.LsfxCIvu", &UPSTREAM_TAIL_FRAMES);
}

#[test]
fn a_queued_message_leaves_ahead_of_buffered_data_as_in_rsync_3_5_1() {
    assert_frames_match_upstream("-vvtrce.LsfxCIvu", &UPSTREAM_TAIL_FRAMES_VV);
}
