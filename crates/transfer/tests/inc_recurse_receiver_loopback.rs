//! End-to-end loopback of the INC_RECURSE streaming receiver.
//!
//! The live client never negotiates INC_RECURSE on its receive path (it drops
//! the `i` capability letter), so the streaming driver
//! `run_pipelined_incremental_streaming` is otherwise reached only by hermetic
//! unit tests with scripted wire bytes. This test wires a real server sender
//! to a real client receiver over a socket pair, both through
//! `run_server_with_handshake` - the same entry the SSH pull drives - and
//! forces INC_RECURSE the way an upstream client would: the sender's compact
//! flag string carries `-e.i...`, so the sender's `setup_protocol` sets
//! `CF_INC_RECURSE` and the receiver honours the flag verbatim
//! (upstream: compat.c:745-746). No production hook is involved.
//!
//! The tree holds 3 directories of 5,000 files plus a nested directory, well
//! above upstream's `MIN_FILECNT_LOOKAHEAD` (1,000) and `MAX_FILECNT_LOOKAHEAD`
//! (10,000), so the sender parks on its lookahead window and the transfer can
//! only finish if the receiver frees that window with mid-walk `NDX_DONE`s.
//! A 60-second watchdog turns a deadlock into a test failure.
//!
//! # Upstream Reference
//!
//! - `generator.c:2219-2239` - `check_for_finished_files` writes `NDX_DONE`
//!   for each completed flist during the walk.
//! - `sender.c:246-261` - the sender echoes it and frees the flist.
//! - `rsync.h:151-152` - `MIN_FILECNT_LOOKAHEAD` / `MAX_FILECNT_LOOKAHEAD`.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::io::BufReader;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use protocol::ProtocolVersion;
use transfer::{
    ServerConfig, ServerRole, ServerStats, perform_handshake_with_max, run_server_with_handshake,
};

const DIRS: usize = 3;
const FILES_PER_DIR: usize = 5_000;
const NESTED_FILES: usize = 40;
const WATCHDOG: Duration = Duration::from_secs(60);

/// Deterministic content of varied length; every 997th file is large enough
/// to span several delta blocks.
fn file_content(dir: usize, idx: usize) -> Vec<u8> {
    let len = if idx % 997 == 0 {
        96 * 1024 + idx
    } else {
        (idx * 131 + dir * 17) % 3_000
    };
    let mut state = (dir as u32 + 1).wrapping_mul(2_654_435_761) ^ idx as u32;
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (state >> 16) as u8
        })
        .collect()
}

/// Builds the source tree and returns its regular-file count.
fn build_tree(src: &Path) -> usize {
    let mut files = 0;
    for dir in 0..DIRS {
        let d = src.join(format!("dir{dir}"));
        fs::create_dir_all(&d).expect("create dir");
        for idx in 0..FILES_PER_DIR {
            fs::write(d.join(format!("f{idx:05}")), file_content(dir, idx)).expect("write file");
            files += 1;
        }
    }
    let nested = src.join("dir1").join("nested").join("deeper");
    fs::create_dir_all(&nested).expect("create nested dir");
    for idx in 0..NESTED_FILES {
        fs::write(nested.join(format!("n{idx:02}")), file_content(DIRS, idx))
            .expect("write nested file");
        files += 1;
    }
    files
}

/// Recursively lists every entry under `root` as a sorted relative path.
fn list_tree(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read_dir") {
            let path = entry.expect("dir entry").path();
            out.push(path.strip_prefix(root).expect("under root").to_path_buf());
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Runs one side of the transfer: handshake, then the server entry point.
fn run_side(config: ServerConfig, stream: UnixStream) -> std::io::Result<ServerStats> {
    let mut reader = BufReader::with_capacity(32 * 1024, stream.try_clone()?);
    let mut writer = stream;
    let handshake = perform_handshake_with_max(&mut reader, &mut writer, ProtocolVersion::V32)?;
    run_server_with_handshake(config, handshake, &mut reader, writer, None, None, None)
}

/// A forced-INC_RECURSE pull of a multi-segment tree completes through the
/// streaming receiver, reproduces the source byte for byte, and releases
/// sub-list segments mid-walk - the release is what keeps the sender's
/// lookahead window open, so a zero count means the streaming path never ran.
#[test]
fn inc_recurse_receiver_loopback_pulls_multi_segment_tree() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    fs::create_dir_all(&dst).expect("create dst");
    let expected_files = build_tree(&src);

    // Sender: `--server --sender -rte.iLsfxCIvu . src/`. The `i` letter is what
    // an upstream client sends to request INC_RECURSE (options.c:3035).
    let mut src_arg = src.clone().into_os_string();
    src_arg.push("/");
    let sender_cfg = ServerConfig::from_flag_string_and_args(
        ServerRole::Generator,
        "-rte.iLsfxCIvu".to_owned(),
        vec![src_arg],
    )
    .expect("sender config");

    // Receiver: the client half of an SSH pull (drive.rs `run_pull_transfer`).
    let mut receiver_cfg = ServerConfig::from_flag_string_and_args(
        ServerRole::Receiver,
        "-rte.LsfxCIvu".to_owned(),
        vec![OsString::from(dst.as_os_str())],
    )
    .expect("receiver config");
    receiver_cfg.connection.client_mode = true;

    let (sender_sock, receiver_sock) = UnixStream::pair().expect("socket pair");
    let kill_sender = sender_sock.try_clone().expect("clone sender socket");
    let kill_receiver = receiver_sock.try_clone().expect("clone receiver socket");

    let (tx, rx) = mpsc::channel();
    let sender_tx = tx.clone();
    thread::spawn(move || {
        let _ = sender_tx.send(("sender", run_side(sender_cfg, sender_sock)));
    });
    thread::spawn(move || {
        let _ = tx.send(("receiver", run_side(receiver_cfg, receiver_sock)));
    });

    let start = Instant::now();
    let mut receiver_stats = None;
    for _ in 0..2 {
        let remaining = WATCHDOG.saturating_sub(start.elapsed());
        let (side, result) = match rx.recv_timeout(remaining) {
            Ok(done) => done,
            Err(_) => {
                // Unblock both peers so the threads exit, then fail.
                let _ = kill_sender.shutdown(Shutdown::Both);
                let _ = kill_receiver.shutdown(Shutdown::Both);
                panic!(
                    "watchdog: transfer did not finish within {WATCHDOG:?} - \
                     sender and receiver deadlocked"
                );
            }
        };
        let stats = result.unwrap_or_else(|e| panic!("{side} failed: {e}"));
        if let ServerStats::Receiver(stats) = stats {
            receiver_stats = Some(stats);
        }
    }
    let stats = receiver_stats.expect("receiver returned receiver stats");

    assert!(
        stats.segments_released_mid_walk > 0,
        "no sub-list segment was released mid-walk: the streaming INC_RECURSE \
         driver did not run (stats: {stats:?})"
    );
    assert_eq!(
        stats.files_transferred, expected_files,
        "every regular file must be transferred exactly once"
    );

    let src_entries = list_tree(&src);
    assert_eq!(
        src_entries,
        list_tree(&dst),
        "destination tree shape differs"
    );
    for rel in &src_entries {
        let s = src.join(rel);
        if s.is_file() {
            let d = dst.join(rel);
            assert!(
                fs::read(&s).expect("read src") == fs::read(&d).expect("read dst"),
                "content mismatch for {}",
                rel.display()
            );
        }
    }
}
