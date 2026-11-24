//! `--only-write-batch` over a remote shell itemizes every entry and records
//! it in the batch, as rsync 3.5.2 does.
//!
//! upstream: main.c:1950 `if (write_batch < 0) dry_run = 1` only turns the
//! filesystem calls into no-ops. The generator still runs `itemize()` for each
//! entry, and generator.c:625-630 writes `write_ndx(sock_f_out, ndx)` plus the
//! `iflags` shortint for every one whose flags are significant - a directory
//! whose time changes, a new symlink - gated on `!read_batch` alone. A file
//! is requested from `notify_others` (generator.c:2410-2431), which calls
//! `itemize()` with the real stat, so the request carries the size, time and
//! is-new bits and not a bare `ITEM_TRANSFER`. The sender echoes each record,
//! prints it, and the echo is what the batch holds.
//!
//! An oc receiver sent requests for files only, each with a bare
//! `ITEM_TRANSFER`: two rows where 3.5.2 prints five, and a batch that replays
//! with the same two.
//!
//! Every cell compares with the 3.5.2-to-3.5.2 run of the same transfer.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use filetime::FileTime;
use test_support::{Deadlined, UpstreamVersion, require_upstream_rsync, run_deadlined};

const BUDGET: Duration = Duration::from_secs(120);
const SOURCE_MTIME: i64 = 1_577_934_245;
const DEST_MTIME: i64 = 1_546_300_800;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Push,
    Pull,
}

fn set_mtime(path: &Path, seconds: i64) {
    let time = FileTime::from_unix_time(seconds, 0);
    test_support::file_times::set_symlink_file_times(path, time, time).expect("set mtime");
}

/// `src/` holds `a`, `l -> a` and `sub/f`. The destination a run starts from
/// holds an older `a` of another size and an older, empty `sub/`, so the root
/// and `sub/` change time only, `a` is updated, and `l` and `sub/f` are new.
fn build_source(root: &Path) {
    let src = root.join("src");
    fs::create_dir_all(src.join("sub")).expect("create src");
    fs::write(src.join("a"), "a-new\n").expect("write a");
    fs::write(src.join("sub/f"), "f\n").expect("write sub/f");
    symlink("a", src.join("l")).expect("symlink l");
    for name in ["sub/f", "sub", "l", "a", ""] {
        set_mtime(&src.join(name), SOURCE_MTIME);
    }
    let rsh = root.join("rsh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh");
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
}

fn build_destination(dst: &Path) {
    fs::create_dir_all(dst.join("sub")).expect("create dst");
    fs::write(dst.join("a"), "a-old-longer\n").expect("write old a");
    for name in ["sub", "a", ""] {
        set_mtime(&dst.join(name), DEST_MTIME);
    }
}

fn run(command: &mut Command, what: &str) -> (Option<i32>, String, String) {
    match run_deadlined(command, BUDGET).expect("spawn rsync") {
        Deadlined::Finished {
            status,
            stdout,
            stderr,
        } => (
            status.code(),
            String::from_utf8_lossy(&stdout).into_owned(),
            String::from_utf8_lossy(&stderr).into_owned(),
        ),
        Deadlined::Expired { stdout, stderr, .. } => panic!(
            "{what} did not finish\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr),
        ),
    }
}

fn walk(root: &Path, relative: &Path, lines: &mut Vec<String>) {
    let path = root.join(relative);
    let meta = fs::symlink_metadata(&path).expect("lstat");
    let mode = meta.permissions().mode() & 0o7777;
    let mtime = test_support::file_times::mtime_of(&meta).unix_seconds();
    let target = fs::read_link(&path).unwrap_or_default();
    lines.push(format!(
        "{} {mode:o} {mtime} {}",
        relative.display(),
        target.display()
    ));
    if meta.is_dir() {
        let mut names: Vec<_> = fs::read_dir(&path)
            .expect("read dir")
            .map(|entry| entry.expect("dir entry").file_name())
            .collect();
        names.sort();
        for name in names {
            walk(root, &relative.join(name), lines);
        }
    }
}

/// What one `--only-write-batch` run printed and recorded.
struct Written {
    stdout: String,
    batch: PathBuf,
    batch_len: u64,
}

fn write_batch(
    root: &Path,
    client: &Path,
    server: &Path,
    direction: Direction,
    extra: &[&str],
    tag: &str,
) -> Written {
    let dst = root.join(format!("dst-{tag}"));
    build_destination(&dst);
    let batch = root.join(format!("batch-{tag}"));
    let src = format!("{}/", root.join("src").display());
    let mut command = Command::new(client);
    command
        .current_dir(root)
        .arg("-ai")
        .args(extra)
        .arg("--checksum-seed=1")
        .arg(format!("--only-write-batch={}", batch.display()))
        .arg("-e")
        .arg(root.join("rsh"))
        .arg(format!("--rsync-path={}", server.display()));
    match direction {
        Direction::Push => command
            .arg(&src)
            .arg(format!("localhost:{}/", dst.display())),
        Direction::Pull => command
            .arg(format!("localhost:{src}"))
            .arg(format!("{}/", dst.display())),
    };
    let (status, stdout, stderr) = run(&mut command, tag);
    assert_eq!(status, Some(0), "{tag}: {stderr}");
    assert!(stderr.is_empty(), "{tag}: {stderr}");
    let mut untouched = Vec::new();
    walk(&dst, Path::new(""), &mut untouched);
    let mut pristine = Vec::new();
    let reference = root.join(format!("pristine-{tag}"));
    build_destination(&reference);
    walk(&reference, Path::new(""), &mut pristine);
    assert_eq!(
        untouched, pristine,
        "{tag}: --only-write-batch must leave the destination alone"
    );
    let batch_len = fs::metadata(&batch).expect("batch written").len();
    Written {
        stdout,
        batch,
        batch_len,
    }
}

/// Replays `batch` with `binary` onto a fresh copy of the starting
/// destination and returns the rows it printed and the tree it left.
fn replay(root: &Path, binary: &Path, batch: &Path, tag: &str) -> (String, Vec<String>) {
    let dst = root.join(format!("replay-{tag}"));
    build_destination(&dst);
    let mut command = Command::new(binary);
    command
        .current_dir(root)
        .arg("-ai")
        .arg(format!("--read-batch={}", batch.display()))
        .arg(format!("{}/", dst.display()));
    let (status, stdout, stderr) = run(&mut command, tag);
    assert_eq!(status, Some(0), "{tag}: {stderr}");
    let mut tree = Vec::new();
    walk(&dst, Path::new(""), &mut tree);
    (stdout, tree)
}

fn compare_with_upstream(direction: Direction, extra: &[&str]) {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let up = upstream.binary();
    let oc = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical tempdir");
    build_source(&root);

    let reference = write_batch(&root, up, up, direction, extra, "ref");
    // The oracle itself must itemize the entries that are not transferred, or
    // agreeing with it says nothing about them.
    let arrow = if direction == Direction::Push {
        '<'
    } else {
        '>'
    };
    assert_eq!(
        reference.stdout,
        format!(
            ".d..t...... ./\n{arrow}f.st...... a\ncL+++++++++ l -> a\n.d..t...... sub/\n\
             {arrow}f+++++++++ sub/f\n"
        ),
        "3.5.2 {direction:?} {extra:?}"
    );
    let (reference_rows, reference_tree) = replay(&root, up, &reference.batch, "ref-up");

    // oc on the receiving side of a 3.5.2 sender, and oc at both ends.
    let (mixed_client, mixed_server) = match direction {
        Direction::Push => (up, oc.as_path()),
        Direction::Pull => (oc.as_path(), up),
    };
    let runs = [
        ("oc-receiving", mixed_client, mixed_server),
        ("oc-both", oc.as_path(), oc.as_path()),
    ];
    for (label, client, server) in runs {
        let written = write_batch(&root, client, server, direction, extra, label);
        assert_eq!(
            written.stdout, reference.stdout,
            "{label} {direction:?} {extra:?}: live rows"
        );
        for (reader_label, reader) in [("up", up), ("oc", oc.as_path())] {
            let (rows, tree) = replay(
                &root,
                reader,
                &written.batch,
                &format!("{label}-{reader_label}"),
            );
            assert_eq!(
                rows, reference_rows,
                "{label} {direction:?} {extra:?}: rows replayed by {reader_label}"
            );
            assert_eq!(
                tree, reference_tree,
                "{label} {direction:?} {extra:?}: tree replayed by {reader_label}"
            );
        }
        // A batch is the sender's stream, so its length is only comparable
        // when the same binary sent it: the 3.5.2 sender here. It then differs
        // by exactly the records the oc receiver did not ask for.
        if label == "oc-receiving" {
            assert_eq!(
                written.batch_len, reference.batch_len,
                "{label} {direction:?} {extra:?}: batch length"
            );
        }
    }
}

#[test]
fn a_push_itemizes_and_records_every_entry_like_upstream() {
    compare_with_upstream(Direction::Push, &[]);
}

#[test]
fn a_pull_itemizes_and_records_every_entry_like_upstream() {
    compare_with_upstream(Direction::Pull, &[]);
}

#[test]
fn a_push_without_incremental_recursion_itemizes_and_records_every_entry() {
    compare_with_upstream(Direction::Push, &["--no-inc-recursive"]);
}

#[test]
fn a_pull_without_incremental_recursion_itemizes_and_records_every_entry() {
    compare_with_upstream(Direction::Pull, &["--no-inc-recursive"]);
}
