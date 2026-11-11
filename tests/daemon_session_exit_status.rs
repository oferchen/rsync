//! A daemon session's process exits with the code its transfer ended on, and
//! the daemon log carries upstream's log_exit() lines rather than a summary of
//! oc's own.
//!
//! ```c
//! /* cleanup.c:213-218 - the run's I/O errors become the exit code */
//!         if (io_error & IOERR_GENERAL || got_xfer_error)
//!                 exit_code = RERR_PARTIAL;
//! /* cleanup.c:223-224 - a daemon always reports through log_exit() */
//!         if ((exit_code && line > 0) || am_daemon || ...)
//!                 log_exit(exit_code, exit_file, exit_line);
//! /* cleanup.c:264 */
//!         exit(exit_code);
//! ```
//!
//! log_exit() (log.c:937-963) writes the `sent ... received ... total size`
//! totals for a clean exit or a sender, and the `rsync error:` trailer for a
//! fatal one; the end-of-run exit_cleanup(0) marks the server `am_server = 2`
//! (cleanup.c:121-122), which keeps a partial transfer's trailer out of the log.
//!
//! The session runs in remote-shell daemon mode, so the server is a process of
//! its own whose exit status the shim records. rsync 3.5.1 serving the same
//! module exits 23 for the missing source and 0 for the clean pull.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// One daemon session: the server's exit status and the daemon log it wrote.
struct Session {
    server_status: i32,
    log: String,
}

/// A module rooted at `<root>/mod` holding `src/a` and a write-denied `ro/`,
/// served in remote-shell daemon mode through a shim that records the server
/// process's exit status.
struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().expect("tempdir");
        let base = root.path();
        fs::create_dir_all(base.join("mod/src")).expect("module source");
        fs::create_dir_all(base.join("mod/ro")).expect("module ro dir");
        fs::create_dir_all(base.join("src")).expect("push source");
        fs::write(base.join("mod/src/a"), b"a\n").expect("module file");
        fs::write(base.join("src/a"), b"a\n").expect("push file");
        fs::create_dir_all(base.join("src2/locked")).expect("push tree");
        fs::write(base.join("src2/b"), b"b\n").expect("push tree file");
        fs::set_permissions(base.join("src2/locked"), fs::Permissions::from_mode(0o000))
            .expect("lock the subdirectory");
        fs::set_permissions(base.join("mod/ro"), fs::Permissions::from_mode(0o555))
            .expect("deny writes under ro");
        fs::write(
            base.join("d.conf"),
            format!(
                "use chroot = no\nlog file = {}\n[m]\npath = {}\nread only = no\n",
                base.join("d.log").display(),
                base.join("mod").display()
            ),
        )
        .expect("daemon config");
        let shim = base.join("rsh");
        fs::write(
            &shim,
            format!(
                "#!/bin/sh\n\
                 while [ \"${{1#-}}\" != \"$1\" ]; do shift; done; shift\n\
                 sh -c \"$*\"; echo $? > {}\n",
                base.join("status").display()
            ),
        )
        .expect("rsh shim");
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).expect("chmod shim");
        Self { root }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    /// Runs the oc client with `operands` against the module and returns what
    /// the server process did. The client reaps its remote-shell child before
    /// it exits, so the shim has written the status by then.
    fn session(&self, operands: [&str; 2]) -> Session {
        let _ = fs::remove_file(self.path("d.log"));
        let rsync_path = format!(
            "{} --config={}",
            oc_binary().display(),
            self.path("d.conf").display()
        );
        let output = Command::new(oc_binary())
            .arg("-r")
            .arg("-e")
            .arg(self.path("rsh"))
            .arg(format!("--rsync-path={rsync_path}"))
            .args(operands)
            .output()
            .expect("run oc-rsync");
        let status = fs::read_to_string(self.path("status")).unwrap_or_else(|_| {
            panic!(
                "the shim recorded no server status: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        Session {
            server_status: status.trim().parse().expect("numeric status"),
            log: fs::read_to_string(self.path("d.log")).unwrap_or_default(),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for dir in ["mod/ro", "src2/locked", "mod/dst/locked"] {
            let _ = fs::set_permissions(self.path(dir), fs::Permissions::from_mode(0o755));
        }
    }
}

fn dest(fixture: &Fixture) -> String {
    format!("{}/", fixture.path("dst").display())
}

fn trailer_code(log: &str) -> Option<i32> {
    let line = log.lines().find(|line| line.contains(" error: "))?;
    let start = line.find("(code ")? + "(code ".len();
    let end = start + line[start..].find(')')?;
    line[start..end].parse().ok()
}

/// WHY: a pull whose source is missing completes with `got_xfer_error`, so
/// the serving process must exit 23 like 3.5.1's (cleanup.c:217-218) while
/// logging only the sender's totals: the end-of-run exit keeps the trailer
/// out (cleanup.c:121-122, log.c:947).
#[test]
fn a_partial_pull_ends_the_session_with_rerr_partial_and_its_totals() {
    let fixture = Fixture::new();
    let session = fixture.session(["host::m/src/nope", &dest(&fixture)]);
    assert_eq!(session.server_status, 23, "{}", session.log);
    assert!(
        session.log.contains("sent ") && session.log.contains(" total size "),
        "{}",
        session.log
    );
    assert_eq!(trailer_code(&session.log), None, "{}", session.log);
}

/// WHY: the clean pull is the control - the same fixture, the same session
/// path, exiting 0 with its totals.
#[test]
fn a_clean_pull_ends_the_session_with_zero() {
    let fixture = Fixture::new();
    let session = fixture.session(["host::m/src/", &dest(&fixture)]);
    assert_eq!(session.server_status, 0, "{}", session.log);
    assert!(session.log.contains(" total size "), "{}", session.log);
}

/// WHY: the receiving side of a push takes the sender's file-list I/O error as
/// its own (flist.c:3312-3314 `io_error |= err & IOERR_VALID_MASK`) and exits 23, but logs neither
/// totals (only a sender's are whole before the end, log.c:941) nor a trailer
/// (the end-of-run exit, cleanup.c:121-122). rsync 3.5.1 serving this push
/// exits 23 with an empty log tail.
#[test]
fn a_push_with_a_sender_io_error_ends_the_session_with_rerr_partial_and_no_totals() {
    let fixture = Fixture::new();
    if fs::read_dir(fixture.path("src2/locked")).is_ok() {
        eprintln!("skipped: a mode-000 directory is readable here (running as root)");
        return;
    }
    let source = format!("{}/", fixture.path("src2").display());
    let session = fixture.session([&source, "host::m/dst/"]);
    assert_eq!(session.server_status, 23, "{}", session.log);
    assert!(!session.log.contains(" total size "), "{}", session.log);
    assert_eq!(trailer_code(&session.log), None, "{}", session.log);
}

/// WHY: a fatal error ends the session with the code it reports, and the log
/// carries that report - log_exit()'s `rsync error:` trailer (log.c:947-963) -
/// in place of the `transfer failed to HOST` line oc used to invent.
#[test]
fn a_fatal_push_ends_the_session_with_the_code_its_trailer_names() {
    let fixture = Fixture::new();
    let source = format!("{}/", fixture.path("src").display());
    let session = fixture.session([&source, "host::m/ro/new/"]);
    let logged = trailer_code(&session.log);
    assert!(logged.is_some_and(|code| code != 0), "{}", session.log);
    assert_eq!(Some(session.server_status), logged, "{}", session.log);
    assert!(
        !session.log.contains("transfer failed to"),
        "{}",
        session.log
    );
}
