//! A push whose sender cannot open one source file skips that file and
//! finishes with exit 23, whatever the receiving server is waiting for.
//!
//! The sender answers a request it cannot serve with `MSG_NO_SEND` and moves
//! on (sender.c:725). rsync's generator retires the entry by index
//! (io.c:1854-1863 -> `got_flist_entry_status(FES_NO_SEND, ndx)`) and the run
//! ends with `RERR_PARTIAL`.
//!
//! An oc server receiver handled that message while it awaited a file's
//! reply, but not while it awaited the echo of an itemized entry that is not
//! transferred, such as the directory itself under `-i` with attributes to
//! set, or any entry under `-vv`. There the decline ended the run with exit 11
//! and nothing transferred. The cells differ only in whether such an echo is
//! outstanding when the decline arrives; `-r` alone has none and is the
//! control.
//!
//! MEASURED against rsync 3.5.2 on both ends: every cell exits 23 and the
//! readable file arrives.
//!
//! `--only-write-batch` reaches the same decline through a different loop. Its
//! sender keeps `do_xfers` set (main.c:1950 forces `dry_run` only after
//! `do_xfers` is computed), so it opens every file and declines the one it
//! cannot read, on a push and on a pull, while the receiver waits for that
//! file's echo. MEASURED against rsync 3.5.2 on both ends: exit 23, nothing
//! written to the destination, and the batch holds the readable file.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tempfile::TempDir;
use test_support::deadline::{Deadlined, run_deadlined};
use test_support::{UpstreamVersion, require_upstream_rsync};

/// Generous for a debug build; only a hang can reach it.
const BUDGET: Duration = Duration::from_secs(120);

/// upstream: errcode.h `RERR_PARTIAL`.
const RERR_PARTIAL: i32 = 23;

const READABLE: &str = "b_readable";
const UNREADABLE: &str = "a_unreadable";

/// Option sets. `-r` awaits no non-transfer echo when the sender declines the
/// unreadable file and is the control; the other two await one.
const CELLS: [&[&str]; 3] = [&["-r"], &["-rlptgoDi"], &["-rvv"]];

struct Fixture {
    temp: TempDir,
    rsh: PathBuf,
}

/// A source directory holding one readable and one mode-000 file, plus a
/// remote shell that runs the server locally.
///
/// Returns `None` when this user can read a mode-000 file (root, or
/// `CAP_DAC_OVERRIDE`): the sender would then send the file and no decline
/// would ever be produced.
fn fixture() -> Option<Fixture> {
    let temp = tempfile::tempdir().expect("tempdir");
    let src = temp.path().join("src");
    fs::create_dir(&src).expect("create src");
    fs::write(src.join(READABLE), b"readable\n").expect("write readable");
    let unreadable = src.join(UNREADABLE);
    fs::write(&unreadable, b"secret\n").expect("write unreadable");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).expect("chmod 000");
    if fs::File::open(&unreadable).is_ok() {
        eprintln!("skipping: this user can read a mode-000 file, so no sender declines it");
        return None;
    }
    let rsh = temp.path().join("rsh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh");
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
    Some(Fixture { temp, rsh })
}

/// Pushes `src/` with `client` to an oc server and returns the exit code, the
/// client's stderr and the destination directory.
fn push(fixture: &Fixture, client: &Path, flags: &[&str], tag: &str) -> (i32, String, PathBuf) {
    let root = fixture.temp.path();
    let dst = root.join(format!("dst-{tag}"));
    fs::create_dir(&dst).expect("create dst");
    let mut command = Command::new(client);
    command
        .args(flags)
        .arg("-e")
        .arg(&fixture.rsh)
        .arg(format!("--rsync-path={}", env!("CARGO_BIN_EXE_oc-rsync")))
        .arg("src/")
        .arg(format!("localhost:{}/", dst.display()))
        .current_dir(root);
    match run_deadlined(&mut command, BUDGET).expect("spawn client") {
        Deadlined::Finished { status, stderr, .. } => (
            status.code().expect("exit code"),
            String::from_utf8_lossy(&stderr).into_owned(),
            dst,
        ),
        Deadlined::Expired { budget, stderr, .. } => panic!(
            "push {flags:?} did not finish within {budget:?}: {}",
            String::from_utf8_lossy(&stderr)
        ),
    }
}

/// The contract every cell must meet: the run reports a partial transfer, the
/// readable file arrives and the unreadable one is not created.
fn assert_skipped_one_file(flags: &[&str], code: i32, stderr: &str, dst: &Path) {
    assert_eq!(code, RERR_PARTIAL, "{flags:?}: {stderr}");
    assert_eq!(
        fs::read(dst.join(READABLE)).unwrap_or_default(),
        b"readable\n",
        "{flags:?}: the readable file must still arrive: {stderr}"
    );
    assert!(
        !dst.join(UNREADABLE).exists(),
        "{flags:?}: the unreadable file must not be created: {stderr}"
    );
}

#[test]
fn oc_server_skips_an_unreadable_source_whatever_echo_it_awaits() {
    let Some(fixture) = fixture() else {
        return;
    };
    let client = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    for (index, flags) in CELLS.iter().enumerate() {
        let (code, stderr, dst) = push(&fixture, &client, flags, &format!("oc{index}"));
        assert_skipped_one_file(flags, code, &stderr, &dst);
    }
}

#[test]
fn an_upstream_client_pushing_to_an_oc_server_skips_the_unreadable_source() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let Some(fixture) = fixture() else {
        return;
    };
    for (index, flags) in CELLS.iter().enumerate() {
        let (code, stderr, dst) = push(&fixture, upstream.binary(), flags, &format!("up{index}"));
        assert_skipped_one_file(flags, code, &stderr, &dst);
    }
}

#[derive(Clone, Copy, Debug)]
enum Direction {
    Push,
    Pull,
}

/// Runs `client` against `server` with `--only-write-batch` and returns the
/// exit code, the client's stderr, the destination and the batch it wrote.
fn only_write_batch(
    fixture: &Fixture,
    client: &Path,
    server: &Path,
    direction: Direction,
    tag: &str,
) -> (i32, String, PathBuf, Vec<u8>) {
    let root = fixture.temp.path();
    let dst = root.join(format!("dst-{tag}"));
    fs::create_dir(&dst).expect("create dst");
    let batch = root.join(format!("batch-{tag}"));
    let mut command = Command::new(client);
    command
        .arg("-r")
        .arg(format!("--only-write-batch={}", batch.display()))
        .arg("-e")
        .arg(&fixture.rsh)
        .arg(format!("--rsync-path={}", server.display()))
        .current_dir(root);
    match direction {
        Direction::Push => command
            .arg("src/")
            .arg(format!("localhost:{}/", dst.display())),
        Direction::Pull => command
            .arg(format!("localhost:{}/src/", root.display()))
            .arg(format!("{}/", dst.display())),
    };
    match run_deadlined(&mut command, BUDGET).expect("spawn client") {
        Deadlined::Finished { status, stderr, .. } => (
            status.code().expect("exit code"),
            String::from_utf8_lossy(&stderr).into_owned(),
            dst,
            fs::read(&batch).unwrap_or_default(),
        ),
        Deadlined::Expired { budget, stderr, .. } => panic!(
            "--only-write-batch {direction:?} did not finish within {budget:?}: {}",
            String::from_utf8_lossy(&stderr)
        ),
    }
}

/// The run reports a partial transfer, writes nothing to the destination, and
/// still records the readable file, which follows the declined one.
fn assert_batch_skipped_one_file(
    label: &str,
    (code, stderr, dst, batch): &(i32, String, PathBuf, Vec<u8>),
) {
    assert_eq!(*code, RERR_PARTIAL, "{label}: {stderr}");
    assert_eq!(
        fs::read_dir(dst).expect("read dst").count(),
        0,
        "{label}: --only-write-batch must not touch the destination: {stderr}"
    );
    assert!(
        batch
            .windows(b"readable\n".len())
            .any(|window| window == b"readable\n"),
        "{label}: the batch must hold the readable file ({} bytes): {stderr}",
        batch.len()
    );
}

#[test]
fn oc_only_write_batch_skips_an_unreadable_source() {
    let Some(fixture) = fixture() else {
        return;
    };
    let oc = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    for direction in [Direction::Push, Direction::Pull] {
        let run = only_write_batch(&fixture, &oc, &oc, direction, &format!("oc-{direction:?}"));
        assert_batch_skipped_one_file(&format!("oc {direction:?}"), &run);
    }
}

/// The pairings that put oc on the receiving side of a 3.5.2 sender, next to
/// the 3.5.2-to-3.5.2 run that defines the expectation.
#[test]
fn oc_receiving_an_only_write_batch_from_upstream_skips_the_unreadable_source() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let Some(fixture) = fixture() else {
        return;
    };
    let oc = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    let up = upstream.binary();
    for direction in [Direction::Push, Direction::Pull] {
        let reference =
            only_write_batch(&fixture, up, up, direction, &format!("ref-{direction:?}"));
        assert_batch_skipped_one_file(&format!("3.5.2 {direction:?}"), &reference);
        let (client, server) = match direction {
            Direction::Push => (up, oc.as_path()),
            Direction::Pull => (oc.as_path(), up),
        };
        let run = only_write_batch(
            &fixture,
            client,
            server,
            direction,
            &format!("mix-{direction:?}"),
        );
        assert_batch_skipped_one_file(&format!("oc receiving, {direction:?}"), &run);
    }
}
