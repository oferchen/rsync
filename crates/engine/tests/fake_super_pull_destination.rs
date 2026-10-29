//! `--fake-super` pulls read the local destination through its `%stat`
//! record, as upstream's generator does.
//!
//! The local-copy executor and the network receiver each stat the
//! destination on their own; this pins the receiver's side over a real
//! remote-shell pull, oc to oc, so it runs without an upstream binary.
//!
//! # Upstream Reference
//!
//! - `generator.c:1393-1399` - under `--fake-super` the generator's
//!   `link_stat()` goes through `x_lstat()`.
//! - `xattrs.c:1253-1258` - a record that does not parse is fatal, exit 11.

#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use filetime::FileTime;
use metadata::FAKE_SUPER_XATTR;
use tempfile::tempdir;

/// The oc-rsync binary and a remote shell that runs the server locally, or
/// `None` when the binary is not built or user xattrs are refused.
fn setup(root: &Path) -> Option<(PathBuf, PathBuf)> {
    let oc = test_support::oc_rsync_bin();
    if !oc.is_file() {
        eprintln!("skipping: oc-rsync binary not built at {}", oc.display());
        return None;
    }
    fs::create_dir_all(root.join("src")).expect("create src");
    if xattr::set(root.join("src"), "user.probe", b"1").is_err() {
        eprintln!("skipping: the filesystem rejects user xattrs");
        return None;
    }
    let shell = root.join("lsh");
    fs::write(&shell, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write lsh");
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o755)).expect("chmod lsh");
    Some((oc, shell))
}

fn pull(oc: &Path, shell: &Path, root: &Path, flags: &str) -> Output {
    Command::new(oc)
        .current_dir(root)
        .arg(flags)
        .args(["--fake-super", "-M--fake-super", "-e"])
        .arg(shell)
        .arg("--rsync-path")
        .arg(oc)
        .arg(format!("localhost:{}/src/", root.display()))
        .arg("dst/")
        .output()
        .expect("run oc-rsync")
}

/// A pull rerun itemizes nothing for files whose mode only the `%stat`
/// record can carry.
#[test]
fn a_fake_super_pull_rerun_itemizes_nothing() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    let Some((oc, shell)) = setup(root) else {
        return;
    };
    for (name, mode) in [("ro", 0o444), ("setuid", 0o4755), ("setgid", 0o2755)] {
        let path = root.join("src").join(name);
        fs::write(&path, name).expect("write source");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("chmod source");
    }
    let first = pull(&oc, &shell, root, "-ai");
    assert!(first.status.success(), "first pull: {first:?}");
    let rerun = pull(&oc, &shell, root, "-ai");
    assert!(rerun.status.success(), "rerun: {rerun:?}");
    assert_eq!(
        String::from_utf8_lossy(&rerun.stdout),
        "",
        "an unchanged rerun itemizes nothing"
    );
}

/// A destination record that does not parse ends the pull with the cause on
/// its own line and the generator's `error in file IO` trailer, exit 11.
#[test]
fn a_fake_super_pull_onto_a_corrupt_destination_exits_file_io() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    let Some((oc, shell)) = setup(root) else {
        return;
    };
    fs::write(root.join("src/f"), b"data").expect("write source");
    fs::create_dir_all(root.join("dst")).expect("create dst");
    let bad = root.join("dst/f");
    fs::write(&bad, b"old").expect("write destination");
    filetime::set_file_mtime(&bad, FileTime::from_unix_time(1_546_300_800, 0)).expect("mtime");
    xattr::set(&bad, FAKE_SUPER_XATTR, b"garbage").expect("plant %stat");

    let output = pull(&oc, &shell, root, "-a");
    assert_eq!(output.status.code(), Some(11), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some(
            format!(
                "Corrupt {FAKE_SUPER_XATTR} xattr attached to \"{}\": \"garbage\"",
                bad.display()
            )
            .as_str()
        ),
        "{stderr}"
    );
    assert_eq!(lines.len(), 2, "the cause and one trailer: {stderr}");
    assert!(
        lines[1].contains("error in file IO (code 11)") && lines[1].contains("[generator="),
        "{stderr}"
    );
    assert_eq!(fs::read(&bad).expect("read destination"), b"old");
}
