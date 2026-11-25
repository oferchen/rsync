//! Shared harness for the ISI INC_RECURSE sender interop cells.
//!
//! A real rsync 3.5.2 client pulls a tree through a remote shell from a
//! sender started with `--rsync-path`. Each cell runs the pull twice on the
//! same source, once with oc-rsync as the sender and once with rsync 3.5.2,
//! so the only thing that differs between the two runs is the sender. The
//! client negotiates protocol 32 with INC_RECURSE on its own (compat.c), so
//! no capability string is hand-written here.
//!
//! The remote shell runs the server command locally and copies everything
//! the sender writes to its stdout into a file, which gives the wire-parity
//! cell the sender's outbound byte stream.
//!
//! Each cell resolves the oracle itself, calling `require_upstream_rsync`
//! with `UpstreamVersion::V3_5_2` in its top-level file, where
//! tools/ci/upstream_oracle_tests.py finds the call: with
//! `OC_RSYNC_UPSTREAM_COMPAT=1`, as the CI oracle step sets, a missing 3.5.2
//! fails the cell; without it the cell prints why it skipped.

#![allow(dead_code)] // Each ISI cell uses the part of the harness it needs.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use checksums::strong::Sha256;
use test_support::UpstreamRsync;

/// The two senders a cell compares, and the 3.5.2 client that drives both.
pub struct Peers {
    /// The oc-rsync binary under test.
    pub oc: PathBuf,
    /// The rsync 3.5.2 oracle, used as the client and as the reference sender.
    pub upstream: PathBuf,
}

/// Pairs oc-rsync with the resolved oracle, or `None` when the cell must
/// skip because the resolver found none.
pub fn peers(upstream: Option<UpstreamRsync>) -> Option<Peers> {
    let upstream = upstream?;
    Some(Peers {
        oc: PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync")),
        upstream: upstream.binary().to_path_buf(),
    })
}

/// What one pull produced.
pub struct Pull {
    /// The client's exit code.
    pub code: Option<i32>,
    /// The client's stdout.
    pub stdout: String,
    /// The client's stderr.
    pub stderr: String,
    /// Every byte the sender wrote to the client.
    pub wire: Vec<u8>,
}

/// Pulls `src/` into `dst/` with the 3.5.2 client, `sender` serving the files.
pub fn pull(peers: &Peers, sender: &Path, src: &Path, dst: &Path, args: &[&str]) -> Pull {
    let scratch = tempfile::tempdir().expect("scratch dir");
    let rsh = scratch.path().join("rsh");
    let wire = scratch.path().join("wire");
    fs::write(
        &rsh,
        "#!/bin/bash\nset -o pipefail\nshift\nsh -c \"$*\" | tee \"$ISI_WIRE\"\n",
    )
    .expect("write rsh");
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
    let output = Command::new(&peers.upstream)
        .args(args)
        .arg(format!("--rsh={}", rsh.display()))
        .arg(format!("--rsync-path={}", sender.display()))
        .arg(format!("localhost:{}/", src.display()))
        .arg(format!("{}/", dst.display()))
        .env("ISI_WIRE", &wire)
        .output()
        .expect("run the rsync 3.5.2 client");
    Pull {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        wire: fs::read(&wire).unwrap_or_default(),
    }
}

/// One entry of a tree snapshot.
#[derive(Debug, PartialEq, Eq)]
pub enum Entry {
    /// A directory.
    Dir,
    /// A directory whose entries cannot be listed (for example mode 0000).
    UnreadableDir,
    /// A regular file and the SHA-256 of its content.
    File([u8; 32]),
}

/// Every entry below `root`, keyed by its path relative to `root`.
pub type Snapshot = BTreeMap<PathBuf, Entry>;

/// Snapshots the tree below `root`.
pub fn snapshot(root: &Path) -> Snapshot {
    let mut out = Snapshot::new();
    walk(root, root, &mut out);
    out
}

fn walk(base: &Path, current: &Path, out: &mut Snapshot) {
    let listing = match fs::read_dir(current) {
        Ok(listing) => listing,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && current != base => {
            let rel = current
                .strip_prefix(base)
                .expect("below base")
                .to_path_buf();
            out.insert(rel, Entry::UnreadableDir);
            return;
        }
        Err(e) => panic!("read {}: {e}", current.display()),
    };
    let mut entries: Vec<_> = listing
        .map(|entry| entry.expect("directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let rel = path.strip_prefix(base).expect("below base").to_path_buf();
        let meta = fs::symlink_metadata(&path).expect("lstat");
        if meta.is_dir() {
            out.insert(rel, Entry::Dir);
            walk(base, &path, out);
        } else {
            let bytes = fs::read(&path).expect("read file");
            out.insert(rel, Entry::File(Sha256::digest(&bytes)));
        }
    }
}

/// Writes a file whose content depends on `seed` and every byte offset.
pub fn write_fixture_file(path: &Path, seed: u32, size: usize) {
    let bytes: Vec<u8> = (0..size)
        .map(|offset| seed.wrapping_mul(31).wrapping_add(offset as u32) as u8)
        .collect();
    fs::write(path, bytes).expect("write fixture file");
}

/// Sets every entry below `root`, and `root` itself, to one fixed mtime so
/// the file list's encoded times are the same on every run.
pub fn pin_mtimes(root: &Path) {
    let mtime = filetime::FileTime::from_unix_time(1_700_000_000, 0);
    for (rel, _) in snapshot(root) {
        test_support::file_times::set_file_mtime(root.join(rel), mtime).expect("pin mtime");
    }
    test_support::file_times::set_file_mtime(root, mtime).expect("pin root mtime");
}

/// Runs the same pull with the oc sender and the 3.5.2 sender into fresh
/// destinations below `work`, returning `(oc, upstream)` with each
/// destination's snapshot.
pub fn pull_both(
    peers: &Peers,
    work: &Path,
    src: &Path,
    args: &[&str],
) -> ((Pull, Snapshot), (Pull, Snapshot)) {
    let run = |sender: &Path, name: &str| {
        let dst = work.join(name);
        fs::create_dir_all(&dst).expect("create destination");
        let pull = pull(peers, sender, src, &dst, args);
        let tree = snapshot(&dst);
        (pull, tree)
    };
    (
        run(&peers.oc, "dst-oc"),
        run(&peers.upstream, "dst-upstream"),
    )
}

/// A multiplex header is `((MPLEX_BASE + tag) << 24) + len`, little-endian
/// (io.c:1180); tag 0 is MSG_DATA.
pub const MPLEX_BASE: u32 = 7;

/// The stats record is five `write_varlong30(f, x, 3)` values; each takes
/// exactly three bytes while it is below 2^23 (io.c write_varlong).
const STATS_LEN: usize = 15;

/// Index of total_size among the stats values.
const TOTAL_SIZE: usize = 2;

/// A sender stream with its multiplex framing removed.
#[derive(Debug)]
struct Demuxed {
    /// The bytes written before multiplexing starts.
    preamble: Vec<u8>,
    /// Every MSG_DATA payload, concatenated.
    data: Vec<u8>,
    /// Every other message, as (tag, payload), in stream order.
    messages: Vec<(u32, Vec<u8>)>,
}

/// Splits `wire` at the first offset from which whole frames run exactly to
/// the end of the stream.
fn demultiplex(wire: &[u8]) -> Demuxed {
    (0..wire.len().min(64))
        .find_map(|start| frames_from(wire, start))
        .unwrap_or_else(|| {
            panic!(
                "no multiplexed frame chain ends the {}-byte stream",
                wire.len()
            )
        })
}

fn frames_from(wire: &[u8], start: usize) -> Option<Demuxed> {
    let mut out = Demuxed {
        preamble: wire[..start].to_vec(),
        data: Vec::new(),
        messages: Vec::new(),
    };
    let mut pos = start;
    while pos < wire.len() {
        let header = u32::from_le_bytes(wire.get(pos..pos + 4)?.try_into().ok()?);
        let tag = (header >> 24).checked_sub(MPLEX_BASE)?;
        let len = (header & 0x00FF_FFFF) as usize;
        let payload = wire.get(pos + 4..pos + 4 + len)?;
        if tag == 0 {
            out.data.extend_from_slice(payload);
        } else {
            out.messages.push((tag, payload.to_vec()));
        }
        pos += 4 + len;
    }
    (!out.data.is_empty()).then_some(out)
}

/// Splits the closing stats record off `data`: total_read, total_written,
/// total_size, flist_buildtime, flist_xfertime (main.c:353-358). The last
/// byte after it is the NDX_DONE the sender echoes in read_final_goodbye
/// (main.c:933-935); it stays with the returned records.
fn split_stats(data: &[u8]) -> (Vec<u8>, [u64; 5]) {
    assert!(
        data.len() > STATS_LEN,
        "stream too short for a stats record"
    );
    let at = data.len() - STATS_LEN - 1;
    let record = &data[at..at + STATS_LEN];
    let mut body = data[..at].to_vec();
    body.extend_from_slice(&data[at + STATS_LEN..]);
    let mut stats = [0; 5];
    for (value, bytes) in stats.iter_mut().zip(record.as_chunks::<3>().0) {
        // write_varlong puts the top byte first, then the low two bytes;
        // a first byte of 0x80 or more would mean a longer encoding.
        assert!(
            bytes[0] < 0x80,
            "stats value does not use the 3-byte varlong form: {bytes:02x?}"
        );
        *value = (u64::from(bytes[0]) << 16) | (u64::from(bytes[2]) << 8) | u64::from(bytes[1]);
    }
    (body, stats)
}

fn hex_window(bytes: &[u8], at: usize) -> String {
    let start = at.saturating_sub(16);
    let end = (at + 16).min(bytes.len());
    bytes[start..end]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Asserts that two sender streams carry the same records.
///
/// rsync 3.5.2's own stream varies from run to run in three places, so they
/// are compared in canonical form:
///
/// - Multiplex frame boundaries. A MSG_DATA header closes when the output
///   buffer is flushed, and the sender flushes when it must wait for input,
///   so how many records share a frame depends on when the client's next
///   NDX_DONE arrives. Only the concatenated MSG_DATA payload and the other
///   messages are compared.
/// - total_read and total_written in the stats record count raw bytes
///   including frame headers (io.c:938, io.c:977).
/// - flist_buildtime and flist_xfertime are wall-clock milliseconds
///   (flist.c:3028-3031, flist.c:3054).
///
/// Everything else, including total_size, must match exactly.
pub fn assert_same_records(oc: &[u8], up: &[u8]) {
    let (oc, up) = (demultiplex(oc), demultiplex(up));
    assert_eq!(oc.preamble, up.preamble, "pre-multiplex bytes differ");
    assert_eq!(oc.messages, up.messages, "non-data messages differ");
    let ((oc_body, oc_stats), (up_body, up_stats)) = (split_stats(&oc.data), split_stats(&up.data));
    if let Some(at) = oc_body.iter().zip(&up_body).position(|(a, b)| a != b) {
        panic!(
            "sender data differs at payload byte {at} (oc {} bytes, 3.5.2 {} bytes)\noc:    {}\n3.5.2: {}",
            oc_body.len(),
            up_body.len(),
            hex_window(&oc_body, at),
            hex_window(&up_body, at),
        );
    }
    assert_eq!(oc_body.len(), up_body.len(), "sender data lengths differ");
    assert_eq!(
        oc_stats[TOTAL_SIZE], up_stats[TOTAL_SIZE],
        "stats total_size differs (oc {oc_stats:?}, 3.5.2 {up_stats:?})"
    );
}
