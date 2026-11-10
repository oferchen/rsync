//! An OS error reaches the user in upstream's shape, `strerror (errno)`, and
//! never as Rust's `io::Error` rendering `... (os error N)`.
//!
//! upstream: log.c:499-500 `rsyserr()` - every failed syscall is reported as
//! `": %s (%d)"` with `strerror(errno)` and `errno`.
//!
//! Each failure is driven through every transport: a local copy, a push and a
//! pull over a remote shell, and a push and a pull against a daemon reached
//! through `RSYNC_CONNECT_PROG`. On the remote legs the failing side is the
//! server, so the text also crosses the wire as `MSG_ERROR`. Every cell must
//! name the errno, so a cell that reports no OS error at all fails instead of
//! passing vacuously. When a 3.5.1 binary is available, the errno text of each
//! cell must equal upstream's.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use test_support::{UpstreamVersion, require_upstream_rsync};

/// How the client reaches the side that fails.
#[derive(Clone, Copy, Debug)]
enum Leg {
    Local,
    ShellPush,
    ShellPull,
    DaemonPush,
    DaemonPull,
}

const ALL_LEGS: [Leg; 5] = [
    Leg::Local,
    Leg::ShellPush,
    Leg::ShellPull,
    Leg::DaemonPush,
    Leg::DaemonPull,
];

/// One failing transfer: the source and destination relative to the fixture
/// root (or absolute), extra client flags, and the errno text it must report.
struct Failure {
    source: &'static str,
    dest: &'static str,
    flags: &'static [&'static str],
    errno_text: &'static str,
}

const MISSING_SOURCE: Failure = Failure {
    source: "missing",
    dest: "dst/",
    flags: &["-r"],
    errno_text: "No such file or directory (2)",
};

const UNREADABLE_DIR: Failure = Failure {
    source: "src/",
    dest: "dst/",
    flags: &["-r"],
    errno_text: "Permission denied (13)",
};

#[cfg(target_os = "linux")]
const FULL_DEVICE: Failure = Failure {
    source: "src/a",
    dest: "/dev/full",
    flags: &["--inplace", "--write-devices"],
    errno_text: "No space left on device (28)",
};

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// A source tree with a readable file and a directory nobody can open.
fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("temp dir");
    let src = tmp.path().join("src");
    fs::create_dir_all(src.join("locked")).expect("src dirs");
    fs::write(src.join("a"), b"payload\n").expect("write a");
    fs::write(src.join("locked/b"), b"hidden\n").expect("write b");
    fs::set_permissions(src.join("locked"), fs::Permissions::from_mode(0o000))
        .expect("lock dir");
    fs::create_dir(tmp.path().join("dst")).expect("dst dir");
    tmp
}

/// Restores the locked directory so the tempdir can be removed.
fn unlock(root: &Path) {
    let _ = fs::set_permissions(
        root.join("src/locked"),
        fs::Permissions::from_mode(0o755),
    );
}

/// Root opens a 0000 directory anyway, so `EACCES` cannot be provoked.
fn permission_checks_apply(root: &Path) -> bool {
    fs::read_dir(root.join("src/locked")).is_err()
}

/// A remote shell that drops the host argument and runs the server locally.
fn local_shell(root: &Path) -> PathBuf {
    let shell = root.join("lsh");
    fs::write(&shell, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write shell");
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o755)).expect("chmod shell");
    shell
}

fn daemon_config(root: &Path) -> PathBuf {
    let conf = root.join("rsyncd.conf");
    fs::write(
        &conf,
        format!(
            "use chroot = no\n[m]\npath = {}\nread only = no\n",
            root.display()
        ),
    )
    .expect("write config");
    conf
}

/// Spells an operand for the side of the leg that it lives on.
fn remote(leg: Leg, root: &Path, operand: &str) -> String {
    let absolute = if operand.starts_with('/') {
        operand.to_owned()
    } else {
        format!("{}/{operand}", root.display())
    };
    match leg {
        Leg::ShellPush | Leg::ShellPull => format!("lh:{absolute}"),
        _ => format!("rsync://localhost/m/{operand}"),
    }
}

fn run(binary: &Path, root: &Path, leg: Leg, failure: &Failure) -> Output {
    let mut command = Command::new(binary);
    command.current_dir(root).args(failure.flags);
    let (source, dest) = match leg {
        Leg::Local => (failure.source.to_owned(), failure.dest.to_owned()),
        Leg::ShellPush | Leg::DaemonPush => (
            failure.source.to_owned(),
            remote(leg, root, failure.dest),
        ),
        Leg::ShellPull | Leg::DaemonPull => (
            remote(leg, root, failure.source),
            failure.dest.to_owned(),
        ),
    };
    match leg {
        Leg::ShellPush | Leg::ShellPull => {
            command
                .arg("-e")
                .arg(local_shell(root))
                .arg(format!("--rsync-path={}", binary.display()));
        }
        Leg::DaemonPush | Leg::DaemonPull => {
            command.env(
                "RSYNC_CONNECT_PROG",
                format!(
                    "{} --daemon --config={}",
                    binary.display(),
                    daemon_config(root).display()
                ),
            );
        }
        Leg::Local => {}
    }
    command.arg(source).arg(dest);
    command.output().expect("run client")
}

/// The `strerror (errno)` tails of every stderr line, sorted.
fn errno_tails(stderr: &str) -> Vec<String> {
    let mut tails: Vec<String> = stderr
        .lines()
        .filter_map(|line| {
            let (_, tail) = line.rsplit_once(": ")?;
            let (text, number) = tail.strip_suffix(')')?.rsplit_once(" (")?;
            (!text.is_empty() && number.parse::<i32>().is_ok()).then(|| tail.to_owned())
        })
        .collect();
    tails.sort();
    tails.dedup();
    tails
}

fn assert_upstream_errno_text(root: &Path, leg: Leg, failure: &Failure) {
    let output = run(&oc_binary(), root, leg, failure);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "{leg:?} {}: the transfer must fail\n{stderr}",
        failure.source
    );
    assert!(
        !stderr.contains("os error"),
        "{leg:?} {}: Rust's io::Error text reached the user\n{stderr}",
        failure.source
    );
    assert!(
        stderr.contains(failure.errno_text),
        "{leg:?} {}: expected upstream's {:?}\n{stderr}",
        failure.source,
        failure.errno_text
    );
}

fn assert_matches_upstream(root: &Path, leg: Leg, failure: &Failure) {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let oc = run(&oc_binary(), root, leg, failure);
    let up = run(upstream.binary(), root, leg, failure);
    assert_eq!(
        errno_tails(&String::from_utf8_lossy(&oc.stderr)),
        errno_tails(&String::from_utf8_lossy(&up.stderr)),
        "{leg:?} {}: errno text differs from 3.5.1\noc:\n{}\n3.5.1:\n{}",
        failure.source,
        String::from_utf8_lossy(&oc.stderr),
        String::from_utf8_lossy(&up.stderr)
    );
}

#[test]
fn a_missing_source_reports_enoent_like_upstream() {
    for leg in ALL_LEGS {
        let tmp = fixture();
        assert_upstream_errno_text(tmp.path(), leg, &MISSING_SOURCE);
        assert_matches_upstream(tmp.path(), leg, &MISSING_SOURCE);
        unlock(tmp.path());
    }
}

#[test]
fn an_unreadable_directory_reports_eacces_like_upstream() {
    for leg in ALL_LEGS {
        let tmp = fixture();
        if !permission_checks_apply(tmp.path()) {
            println!("SKIP: running as root, a 0000 directory stays readable");
            unlock(tmp.path());
            return;
        }
        assert_upstream_errno_text(tmp.path(), leg, &UNREADABLE_DIR);
        assert_matches_upstream(tmp.path(), leg, &UNREADABLE_DIR);
        unlock(tmp.path());
    }
}

/// `/dev/full` fails every write with `ENOSPC`. Only the shell legs and the
/// local copy can name it as a destination; a daemon module cannot.
#[cfg(target_os = "linux")]
#[test]
fn a_full_device_reports_enospc_like_upstream() {
    for leg in [Leg::Local, Leg::ShellPush, Leg::ShellPull] {
        let tmp = fixture();
        assert_upstream_errno_text(tmp.path(), leg, &FULL_DEVICE);
        assert_matches_upstream(tmp.path(), leg, &FULL_DEVICE);
        unlock(tmp.path());
    }
}

#[test]
fn errno_tails_pick_only_strerror_and_number() {
    let stderr = "rsync: [sender] link_stat \"/x\" failed: No such file or directory (2)\n\
                  rsync error: some files/attrs were not transferred (see previous errors) (code 23) at main.c(1338) [sender=3.5.1]\n";
    assert_eq!(errno_tails(stderr), ["No such file or directory (2)"]);
}
