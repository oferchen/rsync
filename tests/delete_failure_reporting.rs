//! A `--delete` removal that fails is reported the way rsync 3.5.2's
//! `delete_item()` reports it, and the rest of the directory is still deleted.
//!
//! The shape is the one that made `daemon-delete-msg-overflow` fail: the
//! `--backup-dir` is itself an extraneous, non-empty directory of the
//! destination. 3.5.2 walks each directory's listing in reverse sorted order
//! (generator.c:364-368), so `runs` goes before `history`. Deleting `history`
//! backs its own contents up into `history/history`, and the final `rmdir`
//! fails with ENOTEMPTY. That is a `FINFO` notice, not an error
//! (delete.c:260-263), and the run exits 0.
//!
//! Each cell runs the same tree through oc and, when the oracle resolves,
//! through rsync 3.5.2, over a local copy, a remote-shell push and a daemon
//! push. It compares the exit code, the delete lines in order, stderr and the
//! final tree. Every child runs under the harness runner's timeout.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use filetime::FileTime;
use test_support::{OcRsyncCliRunner, UpstreamVersion, require_upstream_rsync};

const KEEP_MTIME: i64 = 1_000_000_000;

/// What 3.5.2 prints on stdout for the shape, measured on every leg.
const EXPECTED_LINES: [&str; 6] = [
    "deleting runs/f",
    "deleting runs/",
    "deleting history/runs/f",
    "deleting history/runs/",
    "deleting history/old",
    "cannot delete non-empty directory: history",
];

/// What the destination holds afterwards on every leg, relative to it.
const EXPECTED_TREE: [&str; 7] = [
    ".",
    "./history",
    "./history/history",
    "./history/history/old",
    "./history/history/runs",
    "./history/history/runs/f",
    "./keep",
];

#[derive(Clone, Copy, Debug)]
enum Leg {
    Local,
    RemoteShellPush,
    DaemonPush,
}

/// The observable result of one run.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    status: Option<i32>,
    lines: Vec<String>,
    stderr: String,
    tree: Vec<String>,
}

fn oc_rsync() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn stamp(path: &Path) {
    test_support::file_times::set_file_mtime(path, FileTime::from_unix_time(KEEP_MTIME, 0))
        .expect("stamp");
}

/// Source `s` holds one up-to-date file; destination `d` also holds the
/// extraneous `runs/f` and a `history` backup dir with an older backup in it.
fn build(root: &Path) {
    let src = root.join("s");
    let dst = root.join("d");
    fs::create_dir_all(&src).expect("create s");
    fs::create_dir_all(dst.join("history")).expect("create history");
    fs::create_dir_all(dst.join("runs")).expect("create runs");
    for dir in [&src, &dst] {
        fs::write(dir.join("keep"), "k\n").expect("write keep");
        stamp(&dir.join("keep"));
    }
    fs::write(dst.join("history/old"), "o\n").expect("write old");
    fs::write(dst.join("runs/f"), "r\n").expect("write f");
}

/// A remote shell that runs the server command on this host.
fn write_rsh(root: &Path) -> PathBuf {
    let rsh = root.join("rsh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec sh -c \"$*\"\n").expect("write rsh");
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
    rsh
}

fn tree(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).expect("read_dir") {
            let path = entry.expect("entry").path();
            let rel = path.strip_prefix(base).expect("under base");
            out.push(format!("./{}", rel.display()));
            if fs::symlink_metadata(&path).expect("lstat").is_dir() {
                walk(base, &path, out);
            }
        }
    }
    let mut out = vec![".".to_owned()];
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// Runs the shape with `binary` as both client and server over `leg`.
fn run(binary: &Path, leg: Leg) -> Outcome {
    let root = tempfile::TempDir::new().expect("temp dir");
    let root = root.path();
    build(root);
    let src = format!("{}/", root.join("s").display());
    let mut runner = OcRsyncCliRunner::new()
        .binary(binary)
        .env("OC_RSYNC_FALLBACK", "0")
        .args(["-rtv", "--delete", "--backup", "--backup-dir=history"]);
    let dest = match leg {
        Leg::Local => format!("{}/", root.join("d").display()),
        Leg::RemoteShellPush => {
            runner = runner
                .arg(format!("--rsh={}", write_rsh(root).display()))
                .arg(format!("--rsync-path={}", binary.display()));
            format!("localhost:{}/", root.join("d").display())
        }
        Leg::DaemonPush => {
            let config = root.join("rsyncd.conf");
            fs::write(
                &config,
                format!(
                    "use chroot = no\n[m]\n    path = {}\n    read only = no\n",
                    root.join("d").display()
                ),
            )
            .expect("write rsyncd.conf");
            // The daemon end runs over a pipe, the way the upstream
            // testsuite drives it, so both binaries get the same transport.
            runner = runner.env(
                "RSYNC_CONNECT_PROG",
                format!(
                    "{} --config={} --daemon",
                    binary.display(),
                    config.display()
                ),
            );
            "rsync://localhost/m/".to_owned()
        }
    };
    let out = runner.arg(src).arg(dest).run().expect("child completes");
    let lines = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| line.starts_with("deleting ") || line.starts_with("cannot delete "))
        .map(str::to_owned)
        .collect();
    Outcome {
        status: out.status,
        lines,
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        tree: tree(&root.join("d")),
    }
}

fn check(leg: Leg) {
    let ours = run(&oc_rsync(), leg);
    assert_eq!(ours.status, Some(0), "{leg:?}: {}", ours.stderr);
    assert_eq!(ours.lines, EXPECTED_LINES, "{leg:?}: delete lines");
    assert_eq!(
        ours.stderr, "",
        "{leg:?}: a non-empty directory is FINFO, not an error"
    );
    assert_eq!(ours.tree, EXPECTED_TREE, "{leg:?}: final tree");

    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let theirs = run(upstream.binary(), leg);
    assert_eq!(ours, theirs, "{leg:?}: oc and rsync 3.5.2 differ");
}

#[test]
fn a_non_empty_backup_dir_victim_is_named_and_the_rest_deleted_on_a_local_copy() {
    check(Leg::Local);
}

#[test]
fn a_non_empty_backup_dir_victim_is_named_and_the_rest_deleted_over_a_remote_shell() {
    check(Leg::RemoteShellPush);
}

#[test]
fn a_non_empty_backup_dir_victim_is_named_and_the_rest_deleted_on_a_daemon_push() {
    check(Leg::DaemonPush);
}
