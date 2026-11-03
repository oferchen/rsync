//! A write the receiver cannot complete ends the transfer with upstream's own
//! line for that site and the `error in file IO` exit, not a generic cause.
//!
//! ```c
//! /* receiver.c:658-660 */
//! report_write_error:
//!         rsyserr(FERROR_XFER, errno, "write failed on %s", full_fname(fname));
//!         exit_cleanup(RERR_FILEIO);
//! ```
//!
//! `log_exit()` then prints only the code's description (cleanup.c:248). Each
//! cell sends one file to `/dev/full` under `--write-devices`, whose writes fail
//! with ENOSPC, through each path whose receiver is oc: an rsh pull, an rsh
//! push from an oc client and from a 3.5.1 client, and a daemon pull. A
//! 3.5.1-to-3.5.1 pull runs the same assertions as the fixture's oracle.
//!
//! Linux only: `/dev/full` is a Linux device. The trailer's role is not
//! asserted here; the pull's role is task 268's.

#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use test_support::DaemonProcess;
use test_support::upstream_compat::{UpstreamVersion, require_upstream_rsync};

/// upstream: receiver.c:659 through full_fname() and rsyserr()'s errno suffix.
const WRITE_FAILED: &str = "write failed on \"/dev/full\": No space left on device (28)";
/// upstream: errcode.h RERR_FILEIO via log.c rerr_name().
const FILEIO_TRAILER: &str = "error in file IO (code 11)";

struct Fixture {
    dir: tempfile::TempDir,
    rsh: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = test_support::create_tempdir();
        fs::write(dir.path().join("a"), vec![0x5a; 100_000]).expect("seed a");
        let rsh = dir.path().join("rsh");
        fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh");
        fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
        Self { dir, rsh }
    }

    fn source(&self) -> PathBuf {
        self.dir.path().join("a")
    }

    /// `client` pulls `a` from a `server` over the stub remote shell.
    fn pull(&self, client: &Path, server: &Path) -> Output {
        let mut remote = std::ffi::OsString::from("fakehost:");
        remote.push(self.source());
        rsh_command(client, server, &self.rsh)
            .arg(remote)
            .arg("/dev/full")
            .output()
            .expect("run pull")
    }

    /// `client` pushes `a` to a `server` over the stub remote shell.
    fn push(&self, client: &Path, server: &Path) -> Output {
        rsh_command(client, server, &self.rsh)
            .arg(self.source())
            .arg("fakehost:/dev/full")
            .output()
            .expect("run push")
    }
}

fn rsh_command(client: &Path, server: &Path, rsh: &Path) -> Command {
    let mut command = Command::new(client);
    command
        .arg("--write-devices")
        .arg("-e")
        .arg(rsh)
        .arg("--rsync-path")
        .arg(server)
        .env_remove("RSYNC_RSH");
    command
}

fn dev_full_present() -> bool {
    if Path::new("/dev/full").exists() {
        return true;
    }
    eprintln!("skip: /dev/full is missing");
    false
}

/// The run reports the write site and nothing generic, and exits 11.
fn assert_site_line(output: &Output, cell: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(11), "{cell}: {stderr}");
    assert!(stderr.contains(WRITE_FAILED), "{cell}: {stderr}");
    assert!(
        !stderr.contains("transfer failed") && !stderr.contains("server error"),
        "{cell}: a generic cause replaced the site line: {stderr}"
    );
}

/// [`assert_site_line`], then the `log_exit()` trailer the receiver ends on.
fn assert_write_failure(output: &Output, cell: &str) {
    assert_site_line(output, cell);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(FILEIO_TRAILER), "{cell}: {stderr}");
}

#[test]
fn rsh_pull_into_dev_full_reports_the_write_site() {
    if !dev_full_present() {
        return;
    }
    let oc = test_support::oc_rsync_bin();
    let output = Fixture::new().pull(&oc, &oc);
    assert_write_failure(&output, "oc pull");
}

#[test]
fn rsh_push_into_dev_full_reports_the_write_site() {
    if !dev_full_present() {
        return;
    }
    let oc = test_support::oc_rsync_bin();
    let output = Fixture::new().push(&oc, &oc);
    assert_write_failure(&output, "oc push");
}

#[test]
fn upstream_client_push_to_an_oc_server_reports_the_write_site() {
    if !dev_full_present() {
        return;
    }
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let output = Fixture::new().push(upstream.binary(), &test_support::oc_rsync_bin());
    // The trailer is not asserted: at protocol 31+ the 3.5.1 sender echoes the
    // server's MSG_ERROR_EXIT (io.c:1904-1925) and the oc server leaves on the
    // echo without its own log_exit() line, which 3.5.1's server prints. That
    // gap predates this fix and is tracked separately.
    assert_site_line(&output, "3.5.1 client push to oc");
}

/// Oracle: the tarball 3.5.1 passes the same assertions on the same fixture.
#[test]
fn upstream_pull_into_dev_full_is_the_oracle() {
    if !dev_full_present() {
        return;
    }
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let output = Fixture::new().pull(upstream.binary(), upstream.binary());
    assert_write_failure(&output, "3.5.1 pull");
}

#[test]
fn daemon_pull_into_dev_full_reports_the_write_site() {
    if !dev_full_present() {
        return;
    }
    let fixture = Fixture::new();
    let config = fixture.dir.path().join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "use chroot = false\n[src]\n    path = {}\n",
            fixture.dir.path().display()
        ),
    )
    .expect("write config");
    let daemon = DaemonProcess::spawn(&config, ["--once"]).expect("spawn daemon");

    let output = Command::new(test_support::oc_rsync_bin())
        .arg("--write-devices")
        .arg(format!("rsync://127.0.0.1:{}/src/a", daemon.port()))
        .arg("/dev/full")
        .output()
        .expect("run daemon pull");
    assert_write_failure(&output, "oc daemon pull");
}
