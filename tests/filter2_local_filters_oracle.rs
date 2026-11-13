//! The sender announces each per-directory filter scope as 3.5.1 does.
//!
//! upstream: exclude.c:858-867 `push_local_filters()` prints
//! `[%s] pushing local filters for %s` at FILTER2 for every directory the
//! sender enters, and exclude.c:925-933 `pop_local_filters()` prints
//! `[%s] popping local filters` for every scope that saved merge state. The
//! sequence shows how often each scope is entered, which no transfer outcome
//! can: a scope pushed twice holds the same rules twice and filters the same.
//!
//! Each cell runs one transfer with 3.5.1 and once with oc in the sender's
//! seat and requires the two push/pop sequences to match line for line. The
//! oc sender runs its lazy INC_RECURSE producer, except in the `--no-i-r`
//! cell.

#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// The `src/{a/b, c, d/e}` tree, with a root and a nested per-dir merge file.
fn build_tree(src: &Path) {
    for dir in ["a/b", "c", "d/e"] {
        fs::create_dir_all(src.join(dir)).expect("create dir");
    }
    fs::write(src.join(".rsync-filter"), "- *.bak\n").expect("root merge file");
    fs::write(src.join("a/.rsync-filter"), "- x\n").expect("nested merge file");
    for file in [
        "a/x", "a/b/x", "a/keep", "c/y.bak", "c/keep", "top", "d/e/f",
    ] {
        fs::write(src.join(file), "d").expect("write file");
    }
}

/// A remote shell that runs the remote command locally.
fn local_rsh(dir: &Path) -> PathBuf {
    let path = dir.join("rsh.sh");
    fs::write(&path, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
    path
}

/// Runs `client` with `args`, the other side being `server`, and returns the
/// push/pop lines with `root` written as `T`.
fn scope_trace(client: &Path, server: &Path, root: &Path, args: &[&str]) -> Vec<String> {
    let output = Command::new(client)
        .current_dir(root)
        .env("OC_RSYNC_LAZY_FLIST", "1")
        .arg("-e")
        .arg(local_rsh(root))
        .arg("--rsync-path")
        .arg(server)
        .args(args)
        .output()
        .expect("run transfer");
    assert!(output.status.success(), "{client:?} {args:?}: {output:?}");
    let root = root.display().to_string();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .chain(String::from_utf8_lossy(&output.stderr).lines())
        .filter(|line| line.contains("local filters"))
        .map(|line| line.replace(&root, "T"))
        .collect()
}

/// Runs the same transfer with `upstream` and with oc in the sender's seat,
/// each into its own destination, and requires identical push/pop lines.
fn assert_same_trace(upstream: &Path, pull: bool, args: &[&str]) {
    let oc = oc_rsync_bin();
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    build_tree(&root.join("src"));
    let run = |sender: &Path, dest: &str| {
        let mut full: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let (client, server) = if pull {
            full.push(format!("x:{}/src/", root.display()));
            full.push(format!("{dest}/"));
            (upstream, sender)
        } else {
            full.push("src/".to_owned());
            full.push(format!("x:{}/{dest}/", root.display()));
            (sender, upstream)
        };
        let full: Vec<&str> = full.iter().map(String::as_str).collect();
        scope_trace(client, server, &root, &full)
    };
    let expected = run(upstream, "dst-upstream");
    let actual = run(&oc, "dst-oc");
    assert!(
        expected
            .iter()
            .any(|line| line.contains("pushing local filters")),
        "3.5.1 printed no push lines for {args:?}: the cell cannot discriminate"
    );
    assert_eq!(actual, expected, "{args:?}");
}

#[test]
fn a_lazy_push_announces_each_scope_like_upstream() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    assert_same_trace(upstream.binary(), false, &["-r", "-F", "--debug=FILTER2"]);
}

#[test]
fn a_lazy_push_without_merge_rules_announces_pushes_only() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    assert_same_trace(upstream.binary(), false, &["-r", "--debug=FILTER2"]);
}

#[test]
fn a_whole_list_push_announces_each_scope_like_upstream() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    assert_same_trace(
        upstream.binary(),
        false,
        &["-r", "-F", "--no-i-r", "--debug=FILTER2"],
    );
}

/// The server's sender takes FILTER2 from `-vvv`; its lines reach the client
/// over the multiplexed stream.
#[test]
fn a_lazy_pull_from_an_oc_server_announces_each_scope_like_upstream() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    assert_same_trace(upstream.binary(), true, &["-r", "-F", "-vvv"]);
}
