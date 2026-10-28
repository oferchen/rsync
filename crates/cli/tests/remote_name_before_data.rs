//! Oracle: a remote `-v` client prints each file's name before that file's
//! data moves, as upstream does.
//!
//! upstream: sender.c:763-777 logs the name (`log_before_transfer`) before
//! `match_sums()` sends the data on a push, and receiver.c:1254-1264 logs it
//! once the temp file opens, before `receive_data()`, on a pull.
//!
//! The run is throttled and killed while the destination temp file is part
//! written; the name must already be on stdout. A client that names files
//! after their data dies with the name unprinted.
#![cfg(unix)]

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync, upstream_compat_enabled};

const FILE: &str = "big.bin";
const SIZE: usize = 8 << 20;

#[derive(Clone, Copy, Debug)]
enum Direction {
    Push,
}

/// The destination temp file once it holds some, but not all, of the data.
fn partial_temp(dst: &Path) -> Option<PathBuf> {
    fs::read_dir(dst).ok()?.flatten().map(|entry| entry.path()).find(|path| {
        let named = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(&format!(".{FILE}.")));
        let len = fs::metadata(path).map_or(0, |meta| meta.len());
        named && len > 0 && len < SIZE as u64
    })
}

/// Runs a throttled `-v` transfer, kills it mid-file and returns its stdout.
fn stdout_when_killed_mid_file(binary: &Path, direction: Direction) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("dst")).unwrap();
    let data: Vec<u8> = (0..SIZE).map(|i| (i * 7 + i / 4093) as u8).collect();
    fs::write(root.join("src").join(FILE), data).unwrap();
    let rsh = root.join("rsh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").unwrap();
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).unwrap();
    let (src, dst) = (format!("{}/src/", root.display()), format!("{}/dst/", root.display()));
    let (from, to) = match direction {
        Direction::Push => (src, format!("lh:{dst}")),
    };
    let mut child = Command::new(binary)
        .args(["-rv", "--bwlimit=2000", "-e"])
        .arg(&rsh)
        .arg(format!("--rsync-path={}", binary.display()))
        .args([&from, &to])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // The 2000 KiB/s limit keeps the 8 MiB file in flight for about four
    // seconds; the deadline only bounds a run that never starts writing.
    let deadline = Instant::now() + Duration::from_secs(60);
    while partial_temp(&root.join("dst")).is_none() {
        assert!(child.try_wait().unwrap().is_none(), "{direction:?} ended before a partial write");
        assert!(Instant::now() < deadline, "{direction:?} never wrote part of {FILE}");
        std::thread::sleep(Duration::from_millis(5));
    }
    child.kill().unwrap();
    let mut out = String::new();
    child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    child.wait().unwrap();
    out
}

fn assert_named_before_data(binary: &Path, direction: Direction) {
    let out = stdout_when_killed_mid_file(binary, direction);
    assert!(
        out.lines().any(|line| line == FILE),
        "{} {direction:?}: {FILE} was not named before its data:\n{out}",
        binary.display()
    );
}

#[test]
fn a_push_names_each_file_before_its_data() {
    assert_named_before_data(&oc_rsync_bin(), Direction::Push);
}

/// The same cell against upstream proves it can pass: 3.5.1 names first.
#[test]
fn upstream_names_each_file_before_its_data() {
    if !upstream_compat_enabled() {
        return;
    }
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    assert_named_before_data(upstream.binary(), Direction::Push);
}
