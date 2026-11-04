//! The lazy INC_RECURSE sender over a daemon connection, with an oc peer on
//! both ends, past the 10000-entry `MAX_FILECNT_LOOKAHEAD` ceiling
//! (rsync.h:152): a daemon sender (pull) and a client sender (push).
//!
//! A deadlock at the ceiling hangs the transfer; the test runner's
//! slow-timeout fails it. The pushing client's FLIST3 trace proves the lazy
//! producer ran; the daemon takes no `--debug`, so its pull sender is lazy
//! through the same environment flag the rsh matrix proves engages.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use test_support::DaemonProcess;

const LAZY_ENV: [(&str, &str); 1] = [("OC_RSYNC_LAZY_FLIST", "1")];

/// The lazy producer's FLIST3 trace line (`inc_recurse.rs`).
const LAZY_TRACE: &str = "inc_recurse producer: lazy";

/// 120 sub-directories of 100 files: 12,000+ entries.
fn build_tree(root: &Path) {
    for d in 0..120 {
        for f in 0..100 {
            let path = root.join(format!("d{d:03}/f{f:03}"));
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            fs::write(path, format!("{d}.{f}\n")).expect("write");
        }
    }
}

/// Every entry under `root`, relative, with its content (empty for a dir).
fn listing(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read_dir") {
            let path = entry.expect("dir entry").path();
            let rel = path.strip_prefix(root).expect("under root").to_path_buf();
            if path.is_dir() {
                out.push((rel, Vec::new()));
                stack.push(path);
            } else {
                out.push((rel, fs::read(&path).expect("read")));
            }
        }
    }
    out.sort();
    out
}

/// Runs one client transfer with the lazy flag on and returns its stdout,
/// where FINFO debug lines land.
fn transfer(extra: &[&str], src: &str, dest: &str) -> String {
    let output = Command::new(test_support::oc_rsync_bin())
        .arg("-r")
        .args(extra)
        .args([src, dest])
        .envs(LAZY_ENV)
        .output()
        .expect("run oc-rsync client");
    assert!(
        output.status.success(),
        "{src} -> {dest}: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn lazy_daemon_pull_and_push_past_the_max_lookahead_complete() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let src = dir.path().join("src");
    let pushed = dir.path().join("pushed");
    let pulled = dir.path().join("pulled");
    for d in [&src, &pushed, &pulled] {
        fs::create_dir(d).expect("mkdir");
    }
    build_tree(&src);
    let config = dir.path().join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "use chroot = false\n[src]\n    path = {}\n[dst]\n    path = {}\n    read only = false\n",
            src.display(),
            pushed.display()
        ),
    )
    .expect("write config");
    let daemon = DaemonProcess::spawn_with_env(&config, std::iter::empty::<&str>(), LAZY_ENV)
        .expect("daemon");
    let url = |module: &str| format!("rsync://127.0.0.1:{}/{module}/", daemon.port());

    transfer(&[], &url("src"), &format!("{}/", pulled.display()));
    let push_trace = transfer(
        &["--debug=flist3"],
        &format!("{}/", src.display()),
        &url("dst"),
    );

    let expected = listing(&src);
    assert_eq!(listing(&pulled), expected, "daemon sender (pull)");
    assert_eq!(listing(&pushed), expected, "client sender (push)");
    assert!(
        push_trace.contains(LAZY_TRACE),
        "the pushing client must run the lazy producer:\n{push_trace}"
    );
}
