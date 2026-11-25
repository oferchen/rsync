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
        filetime::set_file_mtime(root.join(rel), mtime).expect("pin mtime");
    }
    filetime::set_file_mtime(root, mtime).expect("pin root mtime");
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
