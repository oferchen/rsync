//! The synthetic `.` transfer root of a leading-`./` `-R` operand is a real
//! file-list member, so it must appear in the listing and the itemize stream -
//! not only in the `--stats` tally.
//!
//! Upstream arms `implied_dot_dir` for an operand whose transmitted name begins
//! with a bare `./` (`flist.c:2640-2641`) and then emits exactly one synthetic
//! entry with `send_file_name(f, flist, ".", NULL, (flags | FLAG_IMPLIED_DIR) &
//! ~FLAG_CONTENT_DIR, ALL_FILTERS)` (`flist.c:2689-2692`). That is the same
//! call the implied *ancestors* go through, so the `.` is rendered by the same
//! rules as `sub/`: listed under `--list-only`, itemized against the basis, and
//! shown as an all-dot `.d ./` row at `-ii` while `-i` suppresses an unchanged
//! entry. A freshly created transfer root instead carries ITEM_IS_NEW and reads
//! `cd+++++++++ ./`.
//!
//! The expectations below were captured from rsync 3.5.0 (protocol 32) in this
//! exact layout, run from inside `src/`:
//!
//! | invocation                          | `.` row              |
//! |-------------------------------------|----------------------|
//! | `-R --list-only ./sub/f.txt`        | `drwxr-xr-x ... .`   |
//! | `-R --list-only sub/f.txt`          | absent               |
//! | `-Rrtii ./sub/f.txt dst/` (synced)  | `.d          ./`     |
//! | `-Rri ./sub/f.txt dst/` (fresh dst) | `cd+++++++++ ./`     |
//! | `-Rri ./sub/f.txt dst/` (dst exists)| absent               |
//!
//! The last two rows are what separates "emit the row" from "emit it
//! unconditionally": both have a pre-existing operand tree, and they differ
//! only in whether the destination *root* was created this run.

#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use test_support::oc_rsync_bin;

/// Creates `src/sub/f.txt` and returns the temp root. `dst/` is deliberately
/// not created: the cells that need it say so, because its presence is the
/// discriminator between two of them.
fn dot_root_tree() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    fs::create_dir_all(tmp.path().join("src/sub")).expect("source tree");
    fs::write(tmp.path().join("src/sub/f.txt"), b"hi\n").expect("leaf file");
    tmp
}

/// Runs `oc-rsync` from inside `src/`, so the operand really is a relative
/// `./...` path - the only form that arms upstream's `implied_dot_dir`.
fn run_from_source(root: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new(oc_rsync_bin());
    cmd.current_dir(root.join("src"));
    cmd.args(args);
    let output = cmd.output().expect("spawn oc-rsync");
    assert!(
        output.status.success(),
        "oc-rsync {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout is utf-8")
}

/// Appends `<root>/dst/` as the destination operand.
fn run_to_dst(root: &Path, args: &[&str]) -> String {
    let mut owned: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    owned.push(format!("{}/", root.join("dst").display()));
    let borrowed: Vec<&str> = owned.iter().map(String::as_str).collect();
    run_from_source(root, &borrowed)
}

/// The `--list-only` renderer prints the name last, so the `.` entry is the
/// line whose final field is exactly `.` and whose mode says directory.
fn lists_dot_entry(stdout: &str) -> bool {
    stdout
        .lines()
        .any(|line| line.starts_with('d') && line.split_whitespace().next_back() == Some("."))
}

/// `--list-only` must show the synthetic `.` alongside the implied ancestor.
#[test]
fn list_only_shows_the_implied_dot_root() {
    let tmp = dot_root_tree();
    let stdout = run_from_source(tmp.path(), &["-R", "--list-only", "./sub/f.txt"]);

    assert!(
        lists_dot_entry(&stdout),
        "rsync 3.5.0 lists the synthetic `.` transfer root: {stdout}"
    );
    assert!(
        stdout.lines().any(|line| line.ends_with(" sub")),
        "the implied ancestor must still be listed: {stdout}"
    );
}

/// NON-VACUITY: an operand without the bare leading `./` never arms
/// `implied_dot_dir`, so there is no `.` entry to list. Without this cell the
/// test above would also pass if the row were emitted unconditionally.
#[test]
fn list_only_without_leading_dot_shows_no_dot_root() {
    let tmp = dot_root_tree();
    let stdout = run_from_source(tmp.path(), &["-R", "--list-only", "sub/f.txt"]);

    assert!(
        !lists_dot_entry(&stdout),
        "no leading `./` means no implied `.` root: {stdout}"
    );
}

/// The accounting was already correct and must not move: the `.` counts toward
/// `Number of files` whether or not its row is rendered. A change here means
/// the fix reached the counters, which it must not.
#[test]
fn stats_still_count_the_dot_root_once() {
    let tmp = dot_root_tree();
    let stdout = run_from_source(tmp.path(), &["-R", "--list-only", "--stats", "./sub/f.txt"]);

    assert!(
        stdout.contains("Number of files: 3 (reg: 1, dir: 2)"),
        "the `.` and `sub` are both directories in the list: {stdout}"
    );
}

/// On a fully synced tree `-ii` shows every entry, including the unchanged
/// transfer root as an all-dot `.d ./`.
#[test]
fn itemize_twice_shows_the_unchanged_dot_root() {
    let tmp = dot_root_tree();
    fs::create_dir(tmp.path().join("dst")).expect("destination root");
    run_to_dst(tmp.path(), &["-Rrt", "./sub/f.txt"]);

    let stdout = run_to_dst(tmp.path(), &["-Rrtii", "./sub/f.txt"]);
    let rows: Vec<&str> = stdout.lines().collect();

    assert_eq!(
        rows,
        [
            ".d          ./",
            ".d          sub/",
            ".f          sub/f.txt"
        ],
        "rsync 3.5.0 itemizes the unchanged root ahead of the ancestor: {stdout}"
    );
}

/// A destination root created by this run keeps ITEM_IS_NEW, so the row is
/// `cd+++++++++ ./` and not the all-dot form. Guards against replacing the
/// created-root arm rather than adding beside it.
#[test]
fn fresh_destination_root_still_itemizes_as_created() {
    let tmp = dot_root_tree();
    let stdout = run_to_dst(tmp.path(), &["-Rri", "./sub/f.txt"]);

    assert!(
        stdout.lines().any(|line| line == "cd+++++++++ ./"),
        "a freshly created transfer root is new, not unchanged: {stdout}"
    );
}

/// THE DISCRIMINATOR: same first run, but the destination root already exists.
/// The root is then unchanged, and `-i` (one `i`, not two) suppresses an
/// all-dot row - upstream prints no `./` line at all here. A fix that emits the
/// row unconditionally, or that builds the change set from something other than
/// the source/destination comparison, fails exactly this cell.
#[test]
fn preexisting_destination_root_itemizes_no_dot_row() {
    let tmp = dot_root_tree();
    fs::create_dir(tmp.path().join("dst")).expect("destination root");

    let stdout = run_to_dst(tmp.path(), &["-Rri", "./sub/f.txt"]);
    let rows: Vec<&str> = stdout.lines().collect();

    assert_eq!(
        rows,
        ["cd+++++++++ sub/", ">f+++++++++ sub/f.txt"],
        "an unchanged transfer root produces no row at -i: {stdout}"
    );
}
