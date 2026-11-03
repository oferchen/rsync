//! A source directory the lazy sender fails to read mid-transfer reaches the
//! receiver in the trailer of the sub-list being sent when the error
//! happened, so `--delete` stops in exactly the directories walked after it.
//!
//! upstream: flist.c:2641 takes `save_io_error` per `send_extra_file_list()`
//! call and flist.c:2686-2693 writes the moved `io_error` into the sub-list's
//! end-of-list trailer; flist.c:3190-3211 folds it into the receiver's
//! `io_error`, which generator.c:304-311 tests as each `delete_in_dir()`
//! starts. With `a`, `b/locked` (unreadable) and `c`, the sub-list for `a` is
//! walked before the error is known and `c`'s after: `a/x` is deleted and
//! `c/x` survives.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use test_support::upstream_compat::{UpstreamVersion, require_upstream_rsync};

/// What `--delete` left of the two extraneous files: `(a/x, c/x)` present.
type Survivors = (bool, bool);

/// The fixture: `src/{a/k, b/locked/, c/k}` with `b/locked` unreadable, and
/// `dst/{a/x, c/x}` to delete. `None` when the user can read `b/locked`
/// anyway (root), so there is no I/O error to observe.
fn fixture(root: &Path) -> Option<(PathBuf, PathBuf)> {
    let (src, dst) = (root.join("src"), root.join("dst"));
    for dir in ["a", "b/locked", "c"] {
        fs::create_dir_all(src.join(dir)).expect("mkdir src");
    }
    for dir in ["a", "c"] {
        fs::create_dir_all(dst.join(dir)).expect("mkdir dst");
        fs::write(src.join(dir).join("k"), b"k").expect("write k");
        fs::write(dst.join(dir).join("x"), b"x").expect("write x");
    }
    let locked = src.join("b/locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
    if fs::read_dir(&locked).is_ok() {
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("chmod");
        return None;
    }
    Some((src, dst))
}

/// A remote shell that runs the server command locally.
fn local_rsh(dir: &Path) -> PathBuf {
    let rsh = dir.join("lsh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh");
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
    rsh
}

/// Pulls `src/` into `dst/` with `client`, running `server` as the sender
/// with the lazy producer on, and reports the survivors.
fn delete_pull(client: &Path, server: &Path) -> Survivors {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let Some((src, dst)) = fixture(tmp.path()) else {
        return (true, true);
    };
    let output = Command::new(client)
        .args(["-rt", "--delete", "-e"])
        .arg(local_rsh(tmp.path()))
        .arg("--rsync-path")
        .arg(server)
        .arg(format!("localhost:{}/", src.display()))
        .arg(format!("{}/", dst.display()))
        .env("OC_RSYNC_LAZY_FLIST", "1")
        .output()
        .expect("run client");
    fs::set_permissions(src.join("b/locked"), fs::Permissions::from_mode(0o755))
        .expect("chmod back");
    assert_eq!(
        output.status.code(),
        Some(23),
        "an unreadable source directory is a partial transfer: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (dst.join("a/x").exists(), dst.join("c/x").exists())
}

#[test]
fn oc_lazy_sender_stops_deleting_where_the_error_is_found() {
    if fixture(tempfile::TempDir::new().expect("tempdir").path()).is_none() {
        return;
    }
    let oc = test_support::oc_rsync_bin();
    assert_eq!(
        delete_pull(&oc, &oc),
        (false, true),
        "a/x is walked before b/locked fails, c/x after"
    );
}

#[test]
fn oc_lazy_sender_matches_upstream_into_an_upstream_receiver() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let oc = test_support::oc_rsync_bin();
    let upstream = upstream.binary().to_path_buf();
    assert_eq!(
        delete_pull(&upstream, &oc),
        delete_pull(&upstream, &upstream),
        "an upstream receiver must delete exactly what it deletes with an upstream sender"
    );
}
