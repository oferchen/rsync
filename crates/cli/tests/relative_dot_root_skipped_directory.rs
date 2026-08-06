//! A positional leading-`./` `-R` operand reports `skipping directory .` when
//! directories are not being transferred.
//!
//! Upstream arms `implied_dot_dir` for an operand whose transmitted name starts
//! with a bare `./` and then injects a synthetic transfer-root entry with
//! `send_file_name(f, flist, ".", NULL, (flags | FLAG_IMPLIED_DIR) &
//! ~FLAG_CONTENT_DIR, ALL_FILTERS)` (`flist.c:2417-2419`). That call reaches
//! `make_file()`, whose directory branch is
//!
//! ```c
//! if (S_ISDIR(st.st_mode)) {
//!         if (!xfer_dirs) {
//!                 rprintf(FINFO, "skipping directory %s\n", thisname);
//!                 return NULL;
//!         }
//! ```
//!
//! (`flist.c:1336-1340`). `thisname` is the cleaned name handed in, so the text
//! is exactly `skipping directory .` - a bare `.`, unquoted, no trailing slash
//! and no "(no recursion)" suffix. Returning NULL also keeps the `.` out of the
//! file list entirely, so it must not be counted under `--stats`.
//!
//! `xfer_dirs` resolves to `recurse || -d`, falling back to `list_only` when
//! neither was given (`options.c:2197-2203`); `--files-from` forces it on
//! (`options.c:2190-2191`). The implied ancestors of the operand are emitted
//! either way, because `send_implied_dirs()` sets `copy_links = xfer_dirs = 1`
//! for the duration of its loop (`flist.c:1982-2012`).
//!
//! Expectations below were captured from rsync 3.4.4 (protocol 32) run in the
//! same layout: `rsync -Rv ./sub/f.txt dst/` prints `skipping directory .`,
//! `sub/`, `sub/f.txt`, and `rsync -R --stats ./sub/f.txt dst/` reports
//! `Number of files: 2 (reg: 1, dir: 1)`.

#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use test_support::oc_rsync_bin;

/// Creates `src/sub/f.txt` plus an empty `dst/` and returns the temp root.
fn dot_root_tree() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    fs::create_dir_all(tmp.path().join("src/sub")).expect("source tree");
    fs::create_dir_all(tmp.path().join("dst")).expect("destination root");
    fs::write(tmp.path().join("src/sub/f.txt"), b"hi\n").expect("leaf file");
    tmp
}

/// Runs `oc-rsync` from inside `src/` (so the operand really is a relative
/// `./...` path, the only form that arms upstream's `implied_dot_dir`) and
/// returns its stdout.
fn run_from_source(root: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new(oc_rsync_bin());
    cmd.current_dir(root.join("src"));
    cmd.args(args);
    let mut destination = root.join("dst").into_os_string();
    destination.push("/");
    cmd.arg(destination);
    let output = cmd.output().expect("spawn oc-rsync");
    assert!(
        output.status.success(),
        "oc-rsync {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout is utf-8")
}

/// The upstream text, verbatim. A drift in wording, quoting or the printed name
/// breaks this and every downstream log scraper that greps for it.
const SKIP_LINE: &str = "skipping directory .";

/// Without `-r`/`-d` the synthetic `.` transfer root is dropped and announced,
/// while the operand's implied ancestor still ships. The line must lead the
/// listing: upstream prints it from `send_file_list()`, before any name row.
#[test]
fn leading_dot_operand_without_xfer_dirs_reports_skipping_directory_dot() {
    let tmp = dot_root_tree();
    let stdout = run_from_source(tmp.path(), &["-Rv", "./sub/f.txt"]);

    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some(SKIP_LINE),
        "rsync 3.4.4 leads with {SKIP_LINE:?} for `-Rv ./sub/f.txt`, got: {stdout}"
    );
    assert_eq!(
        &lines[1..3],
        ["sub/", "sub/f.txt"],
        "the implied ancestor and the leaf must still be listed: {stdout}"
    );

    // The skip is announced, not enforced: the payload still lands.
    assert_eq!(
        fs::read(tmp.path().join("dst/sub/f.txt")).expect("leaf copied"),
        b"hi\n"
    );
}

/// `make_file()` returns NULL for the skipped `.`, so it never enters the file
/// list and must not inflate the `--stats` directory tally. rsync 3.4.4 reports
/// `dir: 1` here (the implied ancestor `sub` only).
#[test]
fn skipped_dot_root_is_not_counted_in_stats() {
    let tmp = dot_root_tree();
    let stdout = run_from_source(tmp.path(), &["-R", "--stats", "./sub/f.txt"]);

    assert!(
        stdout.contains("Number of files: 2 (reg: 1, dir: 1)"),
        "the dropped `.` must not be counted; rsync 3.4.4 reports dir: 1: {stdout}"
    );
}

/// `-r` and `-d` both turn `xfer_dirs` on, so the `.` rides the file list and
/// nothing is skipped. Guards against emitting the notice unconditionally.
#[test]
fn xfer_dirs_suppresses_the_skip_notice() {
    for flag in ["-r", "-d"] {
        let tmp = dot_root_tree();
        let stdout = run_from_source(tmp.path(), &["-Rv", flag, "./sub/f.txt"]);
        assert!(
            !stdout.contains("skipping directory"),
            "{flag} enables xfer_dirs, so upstream skips nothing: {stdout}"
        );
    }
}

/// An operand without the bare leading `./` never arms `implied_dot_dir`, so
/// there is no `.` entry to skip and no notice - even though `xfer_dirs` is
/// still off. Guards against keying the notice on `!xfer_dirs` alone.
#[test]
fn operand_without_leading_dot_reports_nothing() {
    let tmp = dot_root_tree();
    let stdout = run_from_source(tmp.path(), &["-Rv", "sub/f.txt"]);

    assert!(
        !stdout.contains("skipping directory"),
        "no leading `./` means no implied `.` root to skip: {stdout}"
    );
    assert_eq!(
        stdout.lines().take(2).collect::<Vec<_>>(),
        ["sub/", "sub/f.txt"],
        "the ancestor and leaf listing is unchanged: {stdout}"
    );
}
