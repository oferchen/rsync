//! A local copy that finishes every entry but owes a non-zero exit code still
//! prints its listing and statistics, and picks that exit code the way upstream
//! does.
//!
//! upstream `cleanup.c:210-218` decides the exit status only after the transfer:
//!
//! ```text
//! if (io_error & IOERR_DEL_LIMIT) exit_code = RERR_DEL_LIMIT;   /* 25 */
//! if (io_error & IOERR_VANISHED)  exit_code = RERR_VANISHED;    /* 24 */
//! if (io_error & IOERR_GENERAL || got_xfer_error)
//!         exit_code = RERR_PARTIAL;                             /* 23 */
//! ```
//!
//! so a later flag overrides an earlier one and 23 outranks 25. A `--max-delete`
//! cap is only `skipped_deletes++` (`delete.c:217-219`), reported once as
//! `Deletions stopped due to --max-delete limit (%d skipped)`
//! (`generator.c:2904-2907`); it never stops the transfer.
//!
//! Measured against rsync 3.5.1 with `src/{a,sub/z}` copied and `src/unread`
//! unreadable, `dst/{stale1,stale2,sub/stale3}` extraneous:
//!
//! | flags | rsync 3.5.1 | oc before |
//! |---|---|---|
//! | `-ri` | 23, `>f+++++++++ a` printed | 23, no output at all |
//! | `-rv --delete-before --max-delete=1` | 23, files copied, `(2 skipped)` | 25, nothing copied |
//! | `-rv --delete --max-delete=1` without the failure | 25, rows and totals printed | 25, rows and totals lost |
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

struct Fixture {
    _temp: tempfile::TempDir,
    src: PathBuf,
    dst: PathBuf,
}

/// `src/{a,sub/z}` plus `dst/{stale1,stale2,sub/stale3}`; `unreadable` adds a
/// `src/unread` the sender cannot open.
fn fixture(unreadable: bool) -> Fixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let src = temp.path().join("src");
    let dst = temp.path().join("dst");
    fs::create_dir_all(src.join("sub")).expect("mkdir src/sub");
    fs::create_dir_all(dst.join("sub")).expect("mkdir dst/sub");
    fs::write(src.join("a"), "a\n").expect("write a");
    fs::write(src.join("sub/z"), "z\n").expect("write sub/z");
    for stale in ["stale1", "stale2", "sub/stale3"] {
        fs::write(dst.join(stale), "old\n").expect("write stale");
    }
    if unreadable {
        let unread = src.join("unread");
        fs::write(&unread, "x\n").expect("write unread");
        fs::set_permissions(&unread, fs::Permissions::from_mode(0o000)).expect("chmod 000");
    }
    Fixture {
        _temp: temp,
        src,
        dst,
    }
}

/// A privileged process reads a mode-0 file, so the fixture's failure does not
/// happen; such a host cannot exercise these cases.
fn unreadable_file_fails(fx: &Fixture) -> bool {
    if fs::read(fx.src.join("unread")).is_ok() {
        eprintln!("SKIP: this process can read a mode-0 file (running as root?)");
        return false;
    }
    true
}

fn run(fx: &Fixture, flags: &[&str]) -> Output {
    let mut src = fx.src.clone().into_os_string();
    src.push("/");
    let mut dst = fx.dst.clone().into_os_string();
    dst.push("/");
    Command::new(oc_rsync_binary())
        .args(flags)
        .arg(src)
        .arg(dst)
        .output()
        .expect("spawn oc-rsync")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn exists(fx: &Fixture, rel: &str) -> bool {
    Path::new(&fx.dst).join(rel).exists()
}

#[test]
fn an_exit_23_run_still_prints_its_itemize_rows() {
    let fx = fixture(true);
    if !unreadable_file_fails(&fx) {
        return;
    }
    let out = run(&fx, &["-ri"]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(23), "stderr: {}", text(&out.stderr));
    assert!(
        stdout.contains(">f+++++++++ a\n") && stdout.contains(">f+++++++++ sub/z\n"),
        "the copied entries keep their itemize rows on an exit-23 run:\n{stdout}"
    );
}

#[test]
fn an_exit_23_run_still_prints_its_statistics() {
    let fx = fixture(true);
    if !unreadable_file_fails(&fx) {
        return;
    }
    let out = run(&fx, &["-r", "--stats"]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(23), "stderr: {}", text(&out.stderr));
    assert!(
        stdout.contains("Number of files:") && stdout.contains("total size is"),
        "an exit-23 run keeps its --stats block and totals line:\n{stdout}"
    );
}

#[test]
fn a_delete_limit_never_stops_the_transfer_and_a_partial_error_outranks_it() {
    let fx = fixture(true);
    if !unreadable_file_fails(&fx) {
        return;
    }
    let out = run(&fx, &["-rv", "--delete-before", "--max-delete=1"]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(
        out.status.code(),
        Some(23),
        "RERR_PARTIAL outranks RERR_DEL_LIMIT:\n{stderr}"
    );
    assert!(
        exists(&fx, "a") && exists(&fx, "sub/z"),
        "the cap must not abort the copy"
    );
    assert!(
        !exists(&fx, "stale2") && exists(&fx, "stale1") && exists(&fx, "sub/stale3"),
        "one deletion is spent, the rest are refused"
    );
    assert!(stdout.contains("deleting stale2\n"), "{stdout}");
    assert!(
        !stdout.contains("IO error encountered"),
        "no delete pass is pending after the copy, so the skip notice has no site:\n{stdout}"
    );
    assert!(
        stderr.contains("Deletions stopped due to --max-delete limit (2 skipped)\n"),
        "the run-wide skip total is reported once:\n{stderr}"
    );
}

#[test]
fn a_pure_delete_limit_exits_25_after_its_rows_and_totals() {
    let fx = fixture(false);
    let out = run(&fx, &["-rv", "--delete", "--max-delete=1"]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(25), "{stderr}");
    assert!(
        stdout.contains("deleting stale2\n") && stdout.contains("total size is"),
        "the listing and totals precede the exit trailer:\n{stdout}"
    );
    assert!(
        stderr.contains("Deletions stopped due to --max-delete limit (2 skipped)\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("the --max-delete limit stopped deletions (code 25)"),
        "log.c:100 names RERR_DEL_LIMIT in the trailer:\n{stderr}"
    );
    assert!(
        !stdout.contains("max deletions reached"),
        "upstream prints no such line:\n{stdout}"
    );
}
