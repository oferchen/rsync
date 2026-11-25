//! What a daemon reached through a remote shell (`host::module` with `-e`)
//! writes to its own stderr, which the remote shell hands to the client.
//!
//! The fake remote shell sends the daemon's stderr to a file, so these tests
//! see the daemon's lines apart from the client's. Expected text was measured
//! against the rsync 3.5.2 reference daemon.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

use tempfile::TempDir;
use test_support::oc_rsync_bin;

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    /// A one-file source tree and a writable module with `extra` config lines.
    fn new(extra: &str) -> Self {
        let dir = TempDir::new().expect("tempdir");
        fs::create_dir(dir.path().join("src")).expect("mkdir src");
        fs::create_dir(dir.path().join("mod")).expect("mkdir mod");
        fs::write(dir.path().join("src/f"), "x\n").expect("write src/f");
        let conf = format!(
            "[mod]\npath = {}\nuse chroot = no\nread only = no\n{extra}",
            dir.path().join("mod").display()
        );
        fs::write(dir.path().join("rsyncd.conf"), conf).expect("write config");
        Self { dir }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    /// Pushes `src/` to the module through a remote shell that sends the
    /// daemon's stderr to `daemon.stderr`.
    fn push(&self) -> Output {
        let rsh = self.path("rsh.sh");
        fs::write(
            &rsh,
            format!(
                "#!/bin/sh\nshift\nexec sh -c \"$*\" 2>'{}'\n",
                self.path("daemon.stderr").display()
            ),
        )
        .expect("write rsh");
        fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
        Command::new(oc_rsync_bin())
            .arg("-a")
            .arg("-e")
            .arg(&rsh)
            .arg(format!(
                "--rsync-path={} --config={}",
                oc_rsync_bin().display(),
                self.path("rsyncd.conf").display()
            ))
            .arg(format!("{}/", self.path("src").display()))
            .arg("localhost::mod/")
            .output()
            .expect("run oc-rsync")
    }

    fn daemon_stderr(&self) -> String {
        fs::read_to_string(self.path("daemon.stderr")).expect("read daemon stderr")
    }
}

/// A transfer that ran and ended with code 23 leaves no trailer of the
/// daemon's own: its per-file error went to the peer, and upstream's
/// `log_exit()` trailer is queued behind the final goodbye where no client
/// reads it. rsync 3.5.2's daemon writes only `could not make way for new
/// regular file: f` here.
#[test]
fn finished_transfer_prints_no_daemon_trailer() {
    let fx = Fixture::new("");
    fs::create_dir_all(fx.path("mod/f")).expect("mkdir mod/f");
    fs::write(fx.path("mod/f/keep"), "k\n").expect("write mod/f/keep");
    // The client's exit status for this cell is the subject of
    // daemon_over_rsh_exit_status.rs; only the daemon's stderr is checked here.
    fx.push();
    let daemon = fx.daemon_stderr();
    assert!(
        daemon.contains("could not make way for new regular file: f"),
        "{daemon}"
    );
    assert!(!daemon.contains("rsync error"), "{daemon}");
}

/// A refusal before any transfer prints its trailer once. rsync 3.5.2 ends a
/// module whose filter fails to parse with `rsync error: syntax or usage
/// error (code 1)`; the message must not be wrapped in a second trailer.
#[test]
fn refusal_prints_one_unwrapped_trailer() {
    let fx = Fixture::new("filter = - foo clear,- keep\n");
    let out = fx.push();
    let daemon = fx.daemon_stderr();
    assert_eq!(out.status.code(), Some(5), "daemon stderr: {daemon}");
    assert_eq!(
        daemon.matches("syntax or usage error (code 1)").count(),
        1,
        "{daemon}"
    );
    assert_eq!(daemon.matches("rsync error").count(), 1, "{daemon}");
    assert_eq!(daemon.matches("(code ").count(), 1, "{daemon}");
}
