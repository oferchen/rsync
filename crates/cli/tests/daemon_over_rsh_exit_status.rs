//! The exit status of a client that reaches a daemon through a remote shell
//! (`host::module` with `-e`).
//!
//! upstream: main.c:1666-1676 - the remote shell's pid goes to `client_run()`,
//! which waits for it after the transfer (`wait_process_with_flush()`,
//! main.c:198-226). A push exits with that status (main.c:1408-1416) and a pull
//! with the larger of it and the local result (main.c:1455-1461). A daemon
//! reached through `RSYNC_CONNECT_PROG` has no such pid (socket.c sock_exec),
//! so its exit status is never read. Expected values were measured against the
//! rsync 3.5.2 reference binary.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;
use test_support::oc_rsync_bin;

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    /// A source tree with one file and a writable module with one other file.
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        fs::create_dir(dir.path().join("src")).expect("mkdir src");
        fs::create_dir(dir.path().join("mod")).expect("mkdir mod");
        fs::write(dir.path().join("src/f"), "x\n").expect("write src/f");
        fs::write(dir.path().join("mod/g"), "y\n").expect("write mod/g");
        let conf = format!(
            "[mod]\npath = {}\nuse chroot = no\nread only = no\n",
            dir.path().join("mod").display()
        );
        fs::write(dir.path().join("rsyncd.conf"), conf).expect("write config");
        Self { dir }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    /// A remote shell that runs the daemon command it is given and exits with
    /// the daemon's status, or with `exit_code` in its place.
    fn remote_shell(&self, exit_code: Option<i32>) -> PathBuf {
        let rsh = self.path("rsh.sh");
        let body = match exit_code {
            Some(code) => format!("sh -c \"$*\"\nexit {code}"),
            None => "exec sh -c \"$*\"".to_owned(),
        };
        fs::write(&rsh, format!("#!/bin/sh\nshift\n{body}\n")).expect("write rsh");
        fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
        rsh
    }

    fn rsync_path(&self) -> String {
        format!(
            "{} --config={}",
            oc_rsync_bin().display(),
            self.path("rsyncd.conf").display()
        )
    }

    fn over_rsh(&self, rsh: &Path, operands: [&str; 2]) -> Output {
        Command::new(oc_rsync_bin())
            .arg("-a")
            .arg("-e")
            .arg(rsh)
            .arg(format!("--rsync-path={}", self.rsync_path()))
            .args(operands)
            .output()
            .expect("run oc-rsync")
    }

    fn push_over_rsh(&self, rsh: &Path) -> Output {
        let src = format!("{}/", self.path("src").display());
        self.over_rsh(rsh, [&src, "localhost::mod/"])
    }

    fn pull_over_rsh(&self, rsh: &Path) -> Output {
        let dest = format!("{}/", self.path("src").display());
        self.over_rsh(rsh, ["localhost::mod/", &dest])
    }

    /// A pull from the module directory through a plain remote shell
    /// (`host:path`), with no daemon in between.
    fn pull_over_plain_rsh(&self, rsh: &Path) -> Output {
        let src = format!("localhost:{}/", self.path("mod").display());
        let dest = format!("{}/", self.path("src").display());
        Command::new(oc_rsync_bin())
            .arg("-a")
            .arg("-e")
            .arg(rsh)
            .arg(format!("--rsync-path={}", oc_rsync_bin().display()))
            .args([&src, &dest])
            .output()
            .expect("run oc-rsync")
    }

    /// Puts a non-empty directory where the receiving side gets file `f`, so
    /// that side fails the file and the run is a partial transfer (code 23).
    fn block_file_on(&self, receiving: &str) {
        let blocker = self.path(receiving).join("f");
        if blocker.is_file() {
            fs::remove_file(&blocker).expect("remove f");
        }
        fs::create_dir_all(&blocker).expect("mkdir blocker");
        fs::write(blocker.join("keep"), "k\n").expect("write keep");
        let sending = if receiving == "mod" { "src" } else { "mod" };
        fs::write(self.path(sending).join("f"), "x\n").expect("write f");
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A push exits with the remote shell's status after a clean transfer:
/// `rsync error: unexplained error (code 42) at main.c(1416) [sender=3.5.2]`.
#[test]
fn push_exits_with_the_remote_shell_status() {
    let fx = Fixture::new();
    let out = fx.push_over_rsh(&fx.remote_shell(Some(42)));
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(42), "stderr: {err}");
    assert!(err.contains("unexplained error (code 42)"), "{err}");
    assert!(err.contains("[sender="), "{err}");
    assert_eq!(fs::read_to_string(fx.path("mod/f")).unwrap(), "x\n");
}

/// A pull exits with the larger of the remote shell's status and its own:
/// `rsync error: unexplained error (code 42) at main.c(1987) [generator=3.5.2]`.
#[test]
fn pull_exits_with_the_remote_shell_status() {
    let fx = Fixture::new();
    let out = fx.pull_over_rsh(&fx.remote_shell(Some(42)));
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(42), "stderr: {err}");
    assert!(err.contains("unexplained error (code 42)"), "{err}");
    assert!(err.contains("[generator="), "{err}");
    assert_eq!(fs::read_to_string(fx.path("src/g")).unwrap(), "y\n");
}

/// A daemon-side failure reaches a pushing client only through the daemon's
/// exit status: a non-empty directory in the way of `f` makes the daemon exit
/// 23, and so does the client, as rsync 3.5.2's does.
#[test]
fn push_reports_a_daemon_side_partial_transfer() {
    let fx = Fixture::new();
    fs::create_dir_all(fx.path("mod/f")).expect("mkdir mod/f");
    fs::write(fx.path("mod/f/keep"), "k\n").expect("write mod/f/keep");
    let out = fx.push_over_rsh(&fx.remote_shell(None));
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(23), "stderr: {err}");
    assert!(
        err.contains("could not make way for new regular file: f"),
        "{err}"
    );
    assert!(fx.path("mod/f/keep").exists());
}

/// A clean transfer through a remote shell that passes the daemon's status on
/// still exits 0.
#[test]
fn clean_transfer_exits_zero() {
    let fx = Fixture::new();
    let out = fx.push_over_rsh(&fx.remote_shell(None));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
}

/// `RSYNC_CONNECT_PROG` is not a remote shell: upstream keeps no pid for it,
/// so its exit status never becomes the client's.
#[test]
fn connect_program_status_is_not_the_client_exit() {
    let fx = Fixture::new();
    let src = format!("{}/", fx.path("src").display());
    let out = Command::new(oc_rsync_bin())
        .arg("-a")
        .arg(&src)
        .arg("rsync://localhost/mod/")
        .env(
            "RSYNC_CONNECT_PROG",
            format!("{} --server --daemon .; exit 42", fx.rsync_path()),
        )
        .output()
        .expect("run oc-rsync");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(fs::read_to_string(fx.path("mod/f")).unwrap(), "x\n");
}

/// A non-zero remote shell status wins outright over the client's own partial
/// transfer: upstream lifts `io_error` to code 23 only when the status is 0
/// (cleanup.c:208-216). rsync 3.5.2 exits 20 in each of these cells.
#[test]
fn remote_shell_status_outranks_a_local_partial_transfer() {
    let push = Fixture::new();
    push.block_file_on("mod");
    let out = push.push_over_rsh(&push.remote_shell(Some(20)));
    assert_eq!(out.status.code(), Some(20), "push: {}", stderr(&out));

    let pull = Fixture::new();
    pull.block_file_on("src");
    let out = pull.pull_over_rsh(&pull.remote_shell(Some(20)));
    assert_eq!(out.status.code(), Some(20), "pull: {}", stderr(&out));
    assert!(stderr(&out).contains("[generator="), "{}", stderr(&out));
}

/// The plain remote-shell pull folds the status through the same reaper.
#[test]
fn plain_remote_shell_status_outranks_a_local_partial_transfer() {
    let fx = Fixture::new();
    fx.block_file_on("src");
    let out = fx.pull_over_plain_rsh(&fx.remote_shell(Some(20)));
    assert_eq!(out.status.code(), Some(20), "{}", stderr(&out));
}
