//! A daemon push under `--only-write-batch` records the batch and leaves the
//! module untouched, as rsync 3.5.2 does.
//!
//! upstream:
//! - options.c:3091-3092 server_options() forwards `--only-write-batch=X` to
//!   the receiver on a push; clientserver.c:1199 turns it into `dry_run = 1`.
//! - sender.c:510 `f_xfer = write_batch < 0 ? batch_fd : f_out` sends each
//!   file's sum head and tokens into the batch instead of onto the wire.
//! - receiver.c:1003-1009 the server receiver logs the item and reads no data.
//!
//! Every cell pushes into an oc daemon and, when the 3.5.2 oracle resolves
//! (`OC_RSYNC_UPSTREAM_BIN_3_5_2`, required under `OC_RSYNC_UPSTREAM_COMPAT=1`),
//! into a 3.5.2 daemon too, with an oc client and with a 3.5.2 client. Each
//! must exit 0, leave the module empty, and write a batch that replays into
//! a copy of the source.

#![cfg(unix)]

use std::fs;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use test_support::inetd_server::{InetdCommand, InetdServer};
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// Serves an empty writable module `m` from `root/mod` with `binary`.
fn daemon(binary: &Path, root: &Path) -> InetdServer {
    let module = root.join("mod");
    fs::create_dir(&module).expect("mkdir module");
    let config = root.join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "use chroot = no\npid file = {0}/rsyncd.pid\n[m]\npath = {1}\nread only = no\n",
            root.display(),
            module.display()
        ),
    )
    .expect("write rsyncd.conf");
    let command = InetdCommand::discarding_stderr(binary)
        .expect("open /dev/null")
        .arg(format!("--config={}", config.display()));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind daemon listener");
    InetdServer::start(listener, command).expect("start daemon")
}

/// A source tree with a small file and one large enough to need several
/// delta blocks.
fn source(root: &Path) -> PathBuf {
    let src = root.join("src");
    fs::create_dir_all(src.join("sub")).expect("mkdir src/sub");
    fs::write(src.join("a"), b"hello\n").expect("write a");
    let body: Vec<u8> = (0..70_000u32).map(|i| (i * 7 % 251) as u8).collect();
    fs::write(src.join("sub/b"), body).expect("write b");
    src
}

/// Every entry under `root`, relative, sorted.
fn entries(root: &Path) -> Vec<PathBuf> {
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

/// Pushes `src/` with `client` into `daemon`, recording batch `B`, then checks
/// the exit code, the module and the batch.
fn push_cell(cell: &str, client: &Path, daemon_binary: &Path, extra: &[&str]) {
    let dir = TempDir::new().expect("tempdir");
    let src = source(dir.path());
    let server = daemon(daemon_binary, dir.path());
    let batch = dir.path().join("B");
    let output = Command::new(client)
        .arg("-a")
        .args(extra)
        .arg(format!("--only-write-batch={}", batch.display()))
        .arg(format!("{}/", src.display()))
        .arg(format!("rsync://127.0.0.1:{}/m/", server.port()))
        .output()
        .expect("run client");
    server.finish().expect("daemon sessions end");
    assert_eq!(output.status.code(), Some(0), "{cell}: {output:?}");
    assert!(
        entries(&dir.path().join("mod")).is_empty(),
        "{cell}: the push wrote into the module: {:?}",
        entries(&dir.path().join("mod"))
    );

    let replay = dir.path().join("replay");
    let replayed = Command::new(oc_rsync_bin())
        .arg("-a")
        .arg(format!("--read-batch={}", batch.display()))
        .arg(format!("{}/", replay.display()))
        .output()
        .expect("replay batch");
    assert_eq!(replayed.status.code(), Some(0), "{cell}: {replayed:?}");
    assert_eq!(entries(&replay), entries(&src), "{cell}: replayed tree");
    assert_eq!(
        fs::read(replay.join("sub/b")).expect("read replayed b"),
        fs::read(src.join("sub/b")).expect("read source b"),
        "{cell}: replayed content"
    );
}

/// Runs one push for every client and daemon pairing available.
fn every_pairing(extra: &[&str]) {
    let oc = oc_rsync_bin();
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    let mut binaries = vec![("oc", oc)];
    if let Some(upstream) = &upstream {
        binaries.push(("3.5.2", upstream.binary().to_path_buf()));
    }
    for (client_name, client) in &binaries {
        for (daemon_name, daemon_binary) in &binaries {
            let cell = format!("{client_name} client, {daemon_name} daemon, {extra:?}");
            push_cell(&cell, client, daemon_binary, extra);
        }
    }
}

#[test]
fn daemon_push_records_the_batch_without_touching_the_module() {
    every_pairing(&[]);
}

/// Under `-s` the options travel in the secluded phase-2 list
/// (clientserver.c:1164-1169), which must carry the placeholder too.
#[test]
fn secluded_args_daemon_push_records_the_batch_without_touching_the_module() {
    every_pairing(&["-s"]);
}
