//! A receiver that cannot create the staging directory a completed file is
//! moved into under `--delay-updates` drops that one file with upstream's plain
//! `FERROR` line and keeps going, exactly as rsync 3.5.1 does:
//!
//! ```c
//! /* receiver.c:1318-1325 - recv_files() */
//!         } else if (keep_partial && partialptr && (!one_inplace || delay_updates)) {
//!                 if (!handle_partial_dir(partialptr, PDIR_CREATE)) {
//!                         rprintf(FERROR,
//!                                 "Unable to create partial-dir for %s -- discarding %s.\n",
//!                                 local_name ? local_name : f_name(file, NULL),
//!                                 recv_ok ? "completed file" : "partial file");
//!                         do_unlink_at(fnametmp);
//!                         recv_ok = -1;
//! ```
//!
//! A plain `FERROR` never sets `got_xfer_error`, so 3.5.1 exits 0 with neither
//! file in place and the temp files removed. oc used to abort the whole run on
//! the first file instead: exit 23 with its own wording on a local copy, exit 3
//! on a push.
//!
//! Every cell copies `a` then `b` with a writable `--temp-dir`, so the temp
//! create succeeds and only the staging-dir create fails. It runs over each
//! receiver: a push to a `--server` receiver through a local rsh shim, a pull
//! received by the client, and a local copy. The oracle lines are copied from
//! rsync 3.5.1 run against the identical fixture.
//!
//! Skip conditions (the test passes with a printed reason): running as root,
//! which bypasses the 0555 mode the fixture relies on.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn running_as_root() -> bool {
    std::env::var("USER").is_ok_and(|u| u == "root")
}

/// An SSH-shaped remote-shell shim: drop the options and the host
/// placeholder, then exec the server command locally.
fn write_rsh_shim(dir: &Path) -> PathBuf {
    let script = dir.join("fake_rsh.sh");
    fs::write(
        &script,
        "#!/bin/sh\n\
         while [ $# -gt 0 ]; do case \"$1\" in -*) shift ;; *) break ;; esac; done\n\
         shift || true\n\
         exec \"$@\"\n",
    )
    .expect("write rsh shim");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod shim");
    script
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod fixture");
}

/// Which receiver a cell drives.
#[derive(Clone, Copy, Debug)]
enum Transport {
    /// A push to a `--server` receiver through the rsh shim.
    Push,
    /// A pull, received by the client.
    Pull,
    /// A local copy, received by the engine in-process.
    Local,
}

/// Where the completed file is staged.
#[derive(Clone, Copy, Debug)]
enum Staging {
    /// `--delay-updates` alone: upstream's implicit `.~tmp~` beside the file.
    Implicit,
    /// `--partial-dir=.pd`, relative to the destination directory.
    Relative,
    /// `--partial-dir=<root>/apd/x`, an absolute directory to create.
    Absolute,
}

/// Copies `a` and `b` into `<root>/dst` with `--delay-updates` staged per
/// `staging`, the staging parent sealed 0555 for the run, and asserts 3.5.1's
/// outcome.
fn check_cell(transport: Transport, staging: Staging) {
    let cell = format!("{staging:?} ({transport:?})");
    if running_as_root() {
        eprintln!("skip {cell}: root bypasses the 0555 fixture");
        return;
    }
    let dir = test_support::create_tempdir();
    let root = dir.path();
    for dir in ["src", "dst", "tmp", "apd"] {
        fs::create_dir_all(root.join(dir)).expect("create fixture dir");
    }
    fs::write(root.join("src/a"), b"payload-a\n").expect("seed a");
    fs::write(root.join("src/b"), b"payload-b\n").expect("seed b");

    let binary = oc_binary();
    let mut command = Command::new(&binary);
    command
        .arg(format!("--temp-dir={}", root.join("tmp").display()))
        .arg("--delay-updates");
    let sealed = match staging {
        Staging::Implicit => "dst",
        Staging::Relative => {
            command.arg("--partial-dir=.pd");
            "dst"
        }
        Staging::Absolute => {
            command.arg(format!("--partial-dir={}", root.join("apd/x").display()));
            "apd"
        }
    };
    let src = |name: &str| root.join("src").join(name).display().to_string();
    let dst = format!("{}/", root.join("dst").display());
    let (sources, dst) = match transport {
        Transport::Local => ([src("a"), src("b")], dst),
        Transport::Push => ([src("a"), src("b")], format!("localhost:{dst}")),
        Transport::Pull => (
            [
                format!("localhost:{}", src("a")),
                format!("localhost:{}", src("b")),
            ],
            dst,
        ),
    };
    if !matches!(transport, Transport::Local) {
        command
            .arg("--rsh")
            .arg(write_rsh_shim(root))
            .arg("--rsync-path")
            .arg(&binary);
    }
    set_mode(&root.join(sealed), 0o555);
    let output = command.args(sources).arg(dst).output().expect("run copy");
    set_mode(&root.join(sealed), 0o755);

    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines,
        [
            "Unable to create partial-dir for a -- discarding completed file.",
            "Unable to create partial-dir for b -- discarding completed file.",
        ],
        "{cell}: each file reports upstream's line, in file-list order, and nothing else"
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{cell}: a plain FERROR is not a transfer error"
    );
    for gone in ["dst/a", "dst/b"] {
        assert!(!root.join(gone).exists(), "{cell}: {gone} is discarded");
    }
    let leftover: Vec<_> = fs::read_dir(root.join("tmp"))
        .expect("read temp dir")
        .map(|entry| entry.expect("temp entry").file_name())
        .collect();
    assert!(
        leftover.is_empty(),
        "{cell}: the temp files are unlinked, found {leftover:?}"
    );
}

#[test]
fn a_failed_delay_updates_staging_dir_discards_each_file() {
    for transport in [Transport::Push, Transport::Pull, Transport::Local] {
        check_cell(transport, Staging::Implicit);
    }
}

#[test]
fn a_failed_relative_partial_dir_discards_each_file() {
    for transport in [Transport::Push, Transport::Pull, Transport::Local] {
        check_cell(transport, Staging::Relative);
    }
}

#[test]
fn a_failed_absolute_partial_dir_discards_each_file() {
    for transport in [Transport::Push, Transport::Pull, Transport::Local] {
        check_cell(transport, Staging::Absolute);
    }
}
