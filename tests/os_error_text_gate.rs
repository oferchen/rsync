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
    fs::set_permissions(src.join("locked"), fs::Permissions::from_mode(0o000)).expect("lock dir");
    fs::create_dir(tmp.path().join("dst")).expect("dst dir");
    tmp
}

/// Restores the locked directory so the tempdir can be removed.
fn unlock(root: &Path) {
    let _ = fs::set_permissions(root.join("src/locked"), fs::Permissions::from_mode(0o755));
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
        Leg::ShellPush | Leg::DaemonPush => {
            (failure.source.to_owned(), remote(leg, root, failure.dest))
        }
        Leg::ShellPull | Leg::DaemonPull => {
            (remote(leg, root, failure.source), failure.dest.to_owned())
        }
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

/// Every `strerror (errno)` that follows a `": "` on a stderr line, sorted.
///
/// The errno can sit before oc's trailer on the same line, so it is found
/// where it occurs rather than only at the end of the line.
fn errno_tails(stderr: &str) -> Vec<String> {
    let mut tails: Vec<String> = stderr
        .lines()
        .flat_map(|line| line.match_indices(": ").map(move |(at, _)| &line[at + 2..]))
        .filter_map(errno_tail)
        .collect();
    tails.sort();
    tails.dedup();
    tails
}

/// `rest` up to and including its first ` (N)`, if the text before it is one
/// strerror message.
fn errno_tail(rest: &str) -> Option<String> {
    let (text, after) = rest.split_once(" (")?;
    let (number, _) = after.split_once(')')?;
    let is_errno = !text.is_empty()
        && !text.contains(": ")
        && !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit());
    is_errno.then(|| format!("{text} ({number})"))
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
    let oc_tails = errno_tails(&String::from_utf8_lossy(&oc.stderr));
    let up_tails = errno_tails(&String::from_utf8_lossy(&up.stderr));
    // 3.5.1 can add a late `write error: Broken pipe (32)` after its trailer
    // depending on scheduling, so oc's errno set must be contained in
    // upstream's rather than equal to it.
    assert!(
        up_tails.iter().any(|tail| tail == failure.errno_text)
            && oc_tails.iter().all(|tail| up_tails.contains(tail)),
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

/// A failure reported at one of upstream's own `rsyserr()` call sites rather
/// than by the transfer: the client arguments, the cause text both
/// implementations must print, and whether the run still succeeds.
struct Site {
    args: Vec<String>,
    connect_prog: Option<PathBuf>,
    cause: String,
    succeeds: bool,
}

fn run_site(binary: &Path, root: &Path, site: &Site) -> Output {
    let mut command = Command::new(binary);
    command.current_dir(root).args(&site.args);
    if let Some(conf) = &site.connect_prog {
        command.env(
            "RSYNC_CONNECT_PROG",
            format!("{} --daemon --config={}", binary.display(), conf.display()),
        );
    }
    command.output().expect("run client")
}

/// oc prints the cause text without Rust's rendering and, when a 3.5.1
/// binary is available, upstream prints the very same text, so the expected
/// string is upstream's and not one this test made up.
fn assert_site(root: &Path, name: &str, site: &Site) {
    let oc = run_site(&oc_binary(), root, site);
    let stderr = String::from_utf8_lossy(&oc.stderr);
    assert_eq!(
        oc.status.success(),
        site.succeeds,
        "{name}: unexpected exit status\n{stderr}"
    );
    assert!(
        !stderr.contains("os error"),
        "{name}: Rust's io::Error text reached the user\n{stderr}"
    );
    assert!(
        stderr.contains(&site.cause),
        "{name}: expected {:?}\n{stderr}",
        site.cause
    );
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let up = run_site(upstream.binary(), root, site);
    let up_stderr = String::from_utf8_lossy(&up.stderr);
    assert_eq!(
        up.status.success(),
        site.succeeds,
        "{name}: 3.5.1\n{up_stderr}"
    );
    assert!(
        up_stderr.contains(&site.cause),
        "{name}: 3.5.1 does not print {:?}\n{up_stderr}",
        site.cause
    );
}

/// upstream: options.c:2667-2669 - strerror without the errno number.
#[test]
fn a_missing_files_from_file_is_reported_like_upstream() {
    let tmp = fixture();
    let list = tmp.path().join("nonexist");
    let site = Site {
        args: vec![
            format!("--files-from={}", list.display()),
            ".".into(),
            "dst/".into(),
        ],
        connect_prog: None,
        cause: format!(
            "failed to open files-from file {}: No such file or directory",
            list.display()
        ),
        succeeds: false,
    };
    assert_site(tmp.path(), "files-from", &site);
    unlock(tmp.path());
}

/// upstream: log.c:175-182 logfile_open() - reported, then ignored.
#[test]
fn an_unopenable_log_file_is_reported_and_ignored_like_upstream() {
    let tmp = fixture();
    let log = tmp.path().join("nodir/x.log");
    let site = Site {
        args: vec![
            format!("--log-file={}", log.display()),
            "src/a".into(),
            "dst/".into(),
        ],
        connect_prog: None,
        cause: format!(
            "[client] failed to open log-file {}: No such file or directory (2)\n",
            log.display()
        ),
        succeeds: true,
    };
    assert_site(tmp.path(), "log-file", &site);
    unlock(tmp.path());
}

/// upstream: clientserver.c:306-307 - rsyserr "failed to open %s".
#[test]
fn a_missing_early_input_file_is_reported_like_upstream() {
    let tmp = fixture();
    let input = tmp.path().join("nonexist");
    let site = Site {
        args: vec![
            format!("--early-input={}", input.display()),
            "rsync://localhost/m/".into(),
        ],
        connect_prog: Some(daemon_config(tmp.path())),
        cause: format!(
            "failed to open {}: No such file or directory (2)",
            input.display()
        ),
        succeeds: false,
    };
    assert_site(tmp.path(), "early-input", &site);
    unlock(tmp.path());
}

/// upstream: authenticate.c:245-247 - rsyserr "could not open password file
/// %s". Upstream opens the file only when the module asks for a password.
#[test]
fn a_missing_password_file_is_reported_like_upstream() {
    let tmp = fixture();
    let conf = tmp.path().join("auth.conf");
    fs::write(
        &conf,
        format!(
            "use chroot = no\n[m]\npath = {}\nauth users = u\nsecrets file = {}\n",
            tmp.path().display(),
            tmp.path().join("secrets").display()
        ),
    )
    .expect("write config");
    let password = tmp.path().join("nonexist");
    let site = Site {
        args: vec![
            format!("--password-file={}", password.display()),
            "rsync://u@localhost/m/".into(),
        ],
        connect_prog: Some(conf),
        cause: format!(
            "could not open password file {}: No such file or directory (2)",
            password.display()
        ),
        succeeds: false,
    };
    assert_site(tmp.path(), "password-file", &site);
    unlock(tmp.path());
}

#[test]
fn errno_tails_pick_only_strerror_and_number() {
    let stderr = "rsync: [sender] link_stat \"/x\" failed: No such file or directory (2)\n\
                  rsync error: some files/attrs were not transferred (see previous errors) (code 23) at main.c(1338) [sender=3.5.1]\n";
    assert_eq!(errno_tails(stderr), ["No such file or directory (2)"]);
}

#[test]
fn errno_tails_find_the_errno_before_an_oc_trailer() {
    let stderr = "oc-rsync error: failed to copy file 'src/a': No space left on device (28) (code 23) at error.rs(560) [sender=0.6.4]\n";
    assert_eq!(errno_tails(stderr), ["No space left on device (28)"]);
}

/// Static half of the gate: no code may build or expect Rust's OS-error
/// rendering. Only `logging::strerror`, which derives upstream's text from it,
/// may spell it; every other site reports through `logging::rsyserr` and
/// friends with the raw errno, as upstream's call sites use `rsyserr()`.
#[test]
fn no_test_expects_rust_os_error_text() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let needle = ["(os", " error "].concat();
    let mut hits = Vec::new();
    for base in ["crates", "src", "tests"] {
        visit_rust_files(&root.join(base), &mut |path| {
            let relative = path.strip_prefix(root).unwrap_or(path);
            // The one place that turns std's rendering into strerror text.
            if relative.ends_with("crates/logging/src/errno_text.rs") {
                return;
            }
            let Ok(text) = fs::read_to_string(path) else {
                return;
            };
            for (number, line) in text.lines().enumerate() {
                if line.contains(&needle) && !line.trim_start().starts_with("//") {
                    hits.push(format!("{}:{}", relative.display(), number + 1));
                }
            }
        });
    }
    assert!(
        hits.is_empty(),
        "Rust os-error text in source:\n{}",
        hits.join("\n")
    );
}

fn visit_rust_files(dir: &Path, visit: &mut dyn FnMut(&Path)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit_rust_files(&path, visit);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            visit(&path);
        }
    }
}
