//! A receiving server sends a deleted name to the client only when the
//! client asked for per-item output.
//!
//! upstream: log.c:997-1006 `log_delete()` - a server queues `MSG_DELETED`
//! only under `INFO_GTE(DEL, 1) || stdout_format`. `stdout_format` comes from
//! a forwarded `--log-format` (options.c:3009-3021) or from `INFO_GTE(NAME, 1)`
//! (options.c:2576-2579), and a daemon fixes it before `max verbosity` clamps
//! the levels (clientserver.c:1226-1232). A pull is not covered here: its
//! receiver is the client, which prints the row itself and sends nothing.
//!
//! Each cell deletes one extraneous file twice, named `e` and then a name 19
//! bytes longer, and compares the `Total bytes received` of the two runs. The
//! name is the only difference, so the totals differ by exactly 19 when the
//! name is sent and are equal when it is not, whatever the frame layout.
//!
//! Measured against rsync 3.5.2 on the build host with the same fixture, both
//! ends 3.5.2 and mixed with oc in either direction: every cell below gives
//! the cost asserted here (the deleted name is sent for `-v`, `-i`,
//! `--info=del`, `--info=name`, `--out-format=%n` and `--out-format=%o`, and
//! not with none of them), also for a module with `max verbosity = 0`.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use filetime::FileTime;
use tempfile::TempDir;
use test_support::file_times::set_file_mtime;
use test_support::oc_daemon::OcDaemon;
use test_support::oc_rsync_bin;

const SHORT: &str = "e";
const LONG: &str = "eeeeeeeeeeeeeeeeeeee";

/// How the client reaches the receiving server.
#[derive(Clone, Copy, Debug)]
enum Leg {
    /// A local copy, whose forked receiving half is a server (pipe.c:131-132).
    Local,
    /// A push over a remote shell to `oc-rsync --server`.
    RemoteShell,
    /// A push to a daemon module with this `max verbosity`.
    Daemon(u8),
}

/// The client options of each cell and whether they ask for deleted names.
const CELLS: &[(&[&str], bool)] = &[
    (&[], false),
    (&["-v"], true),
    (&["-i"], true),
    (&["--info=del"], true),
    (&["--info=name"], true),
    (&["--out-format=%n"], true),
    (&["--out-format=%o"], true),
];

/// Builds `src/alpha` and `dst/{alpha,<extraneous>}` with pinned mtimes, so
/// `alpha` is up to date and the deletion is the only change.
fn fixture(root: &Path, extraneous: &str) {
    let (src, dst) = (root.join("src"), root.join("dst"));
    fs::create_dir_all(&src).expect("create src");
    fs::create_dir_all(&dst).expect("create dst");
    fs::write(src.join("alpha"), b"alpha\n").expect("write src alpha");
    fs::write(dst.join("alpha"), b"alpha\n").expect("write dst alpha");
    fs::write(dst.join(extraneous), b"x\n").expect("write extraneous");
    let mtime = FileTime::from_unix_time(1_700_000_000, 0);
    for path in [src.join("alpha"), dst.join("alpha"), src.clone()] {
        set_file_mtime(&path, mtime).expect("pin mtime");
    }
}

/// A remote shell that runs the server command locally: `rsh HOST CMD...`.
fn write_rsh(root: &Path) -> std::path::PathBuf {
    let rsh = root.join("rsh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec sh -c \"$*\"\n").expect("write rsh");
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
    rsh
}

/// `Total bytes received` of a `--delete` push that removes `extraneous`.
fn received(leg: Leg, args: &[&str], extraneous: &str) -> u64 {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path();
    fixture(root, extraneous);
    let src = format!("{}/", root.join("src").display());
    let dst = root.join("dst");
    let mut command = Command::new(oc_rsync_bin());
    command.args(["-rt", "--delete", "--stats"]).args(args);
    let daemon = match leg {
        Leg::Local => {
            command.arg(&src).arg(&dst);
            None
        }
        Leg::RemoteShell => {
            command
                .arg(format!("--rsh={}", write_rsh(root).display()))
                .arg(format!("--rsync-path={}", oc_rsync_bin().display()))
                .arg(&src)
                .arg(format!("localhost:{}/", dst.display()));
            None
        }
        Leg::Daemon(max_verbosity) => {
            let config = root.join("rsyncd.conf");
            fs::write(
                &config,
                format!(
                    "[m]\n    path = {}\n    use chroot = false\n    read only = false\n    \
                     max verbosity = {max_verbosity}\n",
                    dst.display()
                ),
            )
            .expect("write rsyncd.conf");
            let daemon = OcDaemon::builder(&config).spawn().expect("spawn daemon");
            command
                .arg(&src)
                .arg(format!("rsync://127.0.0.1:{}/m/", daemon.port()))
                .env("OC_RSYNC_FALLBACK", "0");
            Some(daemon)
        }
    };
    let output = command.output().expect("run oc-rsync");
    drop(daemon);
    assert!(
        output.status.success(),
        "{leg:?} {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !dst.join(extraneous).exists(),
        "{leg:?} {args:?}: {extraneous} was not deleted"
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stats");
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("Total bytes received:"))
        .map(|value| value.trim().replace(',', ""))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("no received total in:\n{stdout}"))
}

/// Asserts, for every cell on `leg`, that the deleted name crosses the wire
/// exactly when the options ask for it.
fn check_leg(leg: Leg, cells: &[(&[&str], bool)]) {
    let name_cost = (LONG.len() - SHORT.len()) as u64;
    for &(args, reported) in cells {
        let short = received(leg, args, SHORT);
        let long = received(leg, args, LONG);
        let expected = if reported { short + name_cost } else { short };
        assert_eq!(
            long,
            expected,
            "{leg:?} {args:?}: received {short} deleting {SHORT:?} and {long} deleting \
             {LONG:?}; the name should {}be sent",
            if reported { "" } else { "not " }
        );
    }
}

#[test]
fn local_copy_reports_deletions_only_on_request() {
    check_leg(Leg::Local, CELLS);
}

#[test]
fn remote_shell_push_reports_deletions_only_on_request() {
    check_leg(Leg::RemoteShell, CELLS);
}

#[test]
fn daemon_push_reports_deletions_only_on_request() {
    check_leg(Leg::Daemon(1), CELLS);
}

/// `max verbosity = 0` clamps the levels after `stdout_format` is fixed
/// (clientserver.c:1226-1232), so `-v` and `--info=name` still ask for names.
#[test]
fn daemon_max_verbosity_does_not_drop_requested_deletions() {
    check_leg(
        Leg::Daemon(0),
        &[(&[], false), (&["-v"], true), (&["--info=name"], true)],
    );
}
