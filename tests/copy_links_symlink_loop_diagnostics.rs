//! `-L` over a symlink loop reports upstream's per-entry error and finishes.
//!
//! Upstream has no symlink-cycle detector. Under `--copy-links` every entry
//! is stat'ed through `readlink_stat()`, and a failure is handled in
//! `make_file()` (`flist.c:1658-1697`): the entry is dropped, `io_error |=
//! IOERR_GENERAL`, the walk goes on, and the run exits `RERR_PARTIAL` (23).
//! ENOENT prints `symlink has no referent: <name>`; any other errno - ELOOP for
//! a cycle - prints `readlink_stat(<name>) failed: <strerror> (<errno>)`. The
//! kernel's symlink limit is the only loop bound, so an ancestor loop unrolls
//! until path resolution itself fails with ELOOP.
//!
//! Expectations pinned against rsync 3.5.1 on the identical fixtures:
//!
//! ```text
//! $ rsync -r -L sl/ out/        # sl/{normal.txt, link -> link}
//! rsync: [sender] readlink_stat("<cwd>/sl/link") failed: Too many levels of symbolic links (40)
//! exit 23, out/normal.txt copied
//! $ rsync -r -L al/ out/        # al/{normal.txt, sub/up -> ..}
//! rsync: [sender] opendir "<cwd>/al/(sub/up/)x40sub/up" failed: Too many levels of symbolic links (40)
//! exit 23, the loop unrolled into out/
//! ```
//!
//! The ancestor case pins only the bound, the ELOOP text and the exit code:
//! upstream stats each entry through the held scan dirfd (`flist.c:454-468`
//! `scan_link_stat()`), so it reaches the kernel limit one level deeper, in
//! `opendir`, than a full-path stat does.

#![cfg(unix)]

use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use test_support::deadline::{Deadlined, run_deadlined};

/// Generous for a debug build; the loop terminates in well under a second
/// upstream, so only a hang can reach it.
const BUDGET: Duration = Duration::from_secs(120);

/// Upstream's `strerror (errno)` rendering of ELOOP on this platform.
fn eloop_text() -> String {
    let error = io::Error::from_raw_os_error(libc::ELOOP);
    let full = error.to_string();
    let strerror = full
        .strip_suffix(&format!(" (os error {})", libc::ELOOP))
        .unwrap_or(&full);
    format!("{strerror} ({})", libc::ELOOP)
}

struct Run {
    code: i32,
    stderr: String,
}

fn copy_links(cwd: &Path, source: &str) -> Run {
    let mut command = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync")));
    command.args(["-r", "-L", source, "out/"]).current_dir(cwd);
    match run_deadlined(&mut command, BUDGET).expect("spawn oc-rsync") {
        Deadlined::Finished { status, stderr, .. } => Run {
            code: status.code().expect("exit code"),
            stderr: String::from_utf8(stderr).expect("utf-8 stderr"),
        },
        Deadlined::Expired { budget, stderr, .. } => panic!(
            "oc-rsync -L {source} did not finish within {budget:?}: {}",
            String::from_utf8_lossy(&stderr)
        ),
    }
}

fn error_lines(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter(|line| line.starts_with("rsync: "))
        .collect()
}

#[test]
fn self_loop_reports_readlink_stat_eloop_and_copies_the_rest() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cwd = temp.path().canonicalize().expect("canonical tempdir");
    fs::create_dir(cwd.join("sl")).expect("create sl");
    fs::write(cwd.join("sl/normal.txt"), b"n\n").expect("write normal");
    symlink("link", cwd.join("sl/link")).expect("self-referencing symlink");

    let run = copy_links(&cwd, "sl/");

    assert_eq!(run.code, 23, "stderr: {}", run.stderr);
    let expected = format!(
        "rsync: [sender] readlink_stat(\"{}/sl/link\") failed: {}",
        cwd.display(),
        eloop_text()
    );
    assert_eq!(error_lines(&run.stderr), [expected.as_str()]);
    assert_eq!(
        fs::read(cwd.join("out/normal.txt")).expect("normal.txt copied"),
        b"n\n"
    );
    assert!(fs::symlink_metadata(cwd.join("out/link")).is_err());
}

#[test]
fn ancestor_loop_unrolls_until_the_kernel_reports_eloop() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cwd = temp.path().canonicalize().expect("canonical tempdir");
    fs::create_dir_all(cwd.join("al/sub")).expect("create al/sub");
    fs::write(cwd.join("al/normal.txt"), b"n\n").expect("write normal");
    symlink("..", cwd.join("al/sub/up")).expect("ancestor symlink");

    let run = copy_links(&cwd, "al/");

    assert_eq!(run.code, 23, "stderr: {}", run.stderr);
    let errors = error_lines(&run.stderr);
    assert_eq!(errors.len(), 1, "stderr: {}", run.stderr);
    assert!(
        errors[0].starts_with("rsync: [sender] ")
            && errors[0].contains(&format!("\"{}/al/sub/up/sub/up/", cwd.display()))
            && errors[0].ends_with(&format!(" failed: {}", eloop_text())),
        "unexpected diagnostic: {}",
        errors[0]
    );
    assert_eq!(
        fs::read(cwd.join("out/normal.txt")).expect("normal.txt copied"),
        b"n\n"
    );
    assert!(cwd.join("out/sub/up/sub/up/normal.txt").is_file());
}
