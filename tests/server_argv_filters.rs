//! Filter options on a remote-shell server's own argv, as 3.5.1 applies them.
//!
//! upstream: options.c:1620-1656 and :1718-1727 parse `-f`, `--exclude`,
//! `--include`, the `-from` pair and `-F` into `filter_list` ahead of the
//! client's wire rules (exclude.c:1971-1986), relative to the server's cwd.
//! The pass restarts at `--server` (options.c:1517-1524) without clearing that
//! list, so an option ahead of `--server` applies twice. A missing filter file
//! is `rsyserr()` plus RERR_FILEIO (exclude.c:1714-1719). Every transfer is
//! driven by the 3.5.1 client under a deadline, so a hang fails as a TIMEOUT.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use test_support::{Deadlined, UpstreamVersion, require_upstream_rsync, run_deadlined};

const BUDGET: Duration = Duration::from_secs(30);

fn oc_rsync() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn upstream() -> Option<PathBuf> {
    require_upstream_rsync(UpstreamVersion::V3_5_1).map(|u| u.binary().to_path_buf())
}

#[derive(Clone, Copy)]
enum Direction {
    Pull,
    /// A `--delete` push into a destination holding `keep` and `gone`.
    DeletePush,
}

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    status: Option<i32>,
    /// Every file in the destination afterwards, sorted.
    files: Vec<String>,
    /// The server's refusal lines, program name and source site removed.
    refusal: Vec<String>,
}

/// Transfers `src/` (`d/a`, `d/b`, `d/g.tmp` and a `.rsync-filter` saying
/// `- a`) between the 3.5.1 client and `server`, whose argv gets `before`
/// ahead of `--server` and `after` right behind it.
fn transfer(server: &Path, direction: Direction, before: &[&str], after: &[&str]) -> Outcome {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::create_dir_all(root.join("src/d")).unwrap();
    fs::create_dir_all(root.join("dst")).unwrap();
    for name in ["a", "b", "g.tmp"] {
        fs::write(root.join("src/d").join(name), format!("{name}\n")).unwrap();
    }
    fs::write(root.join("src/.rsync-filter"), "- a\n").unwrap();
    let quote = |tokens: &[&str]| -> String { tokens.iter().map(|t| format!("'{t}' ")).collect() };
    let rsh = root.join("rsh");
    fs::write(
        &rsh,
        format!(
            "#!/bin/sh\nshift\nprog=$1\nshift\nshift\nexec \"$prog\" {}--server {}\"$@\"\n",
            quote(before),
            quote(after)
        ),
    )
    .unwrap();
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).unwrap();
    let src = format!("{}/", root.join("src").display());
    let dst = format!("{}/", root.join("dst").display());
    let mut command = Command::new(upstream().unwrap());
    command
        .arg("-r")
        .arg("-e")
        .arg(&rsh)
        .arg(format!("--rsync-path={}", server.display()));
    match direction {
        Direction::Pull => command.arg(format!("localhost:{src}")).arg(&dst),
        Direction::DeletePush => {
            fs::write(root.join("dst/keep"), "k\n").unwrap();
            fs::write(root.join("dst/gone"), "x\n").unwrap();
            command.arg("--delete").arg(&src).arg(format!("localhost:{dst}"))
        }
    };
    let (status, stderr) = match run_deadlined(&mut command, BUDGET).unwrap() {
        Deadlined::Finished { status, stderr, .. } => (status.code(), stderr),
        Deadlined::Expired { budget, stderr, .. } => panic!(
            "TIMEOUT: {before:?} --server {after:?} against {} still running after {budget:?}: {}",
            server.display(),
            String::from_utf8_lossy(&stderr)
        ),
    };
    Outcome {
        status,
        files: files_under(&root.join("dst")),
        refusal: refusal(&String::from_utf8_lossy(&stderr)),
    }
}

fn files_under(dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(dir).unwrap();
                files.push(relative.display().to_string());
            }
        }
    }
    files.sort();
    files
}

/// The server's own lines: the program name, the trailer's source site and
/// its version are each implementation's own.
fn refusal(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter(|line| line.contains("[server") || line.contains("filter rule"))
        .map(|line| {
            let line = line.strip_prefix("oc-").unwrap_or(line);
            match (line.find(" at "), line.rfind('[')) {
                (Some(at), Some(open)) if !line.contains("] ") => {
                    let tag = &line[open..];
                    format!("{}{}", &line[..at], &tag[..tag.find('=').unwrap_or(tag.len())])
                }
                _ => line.to_owned(),
            }
        })
        .collect()
}

/// Runs one cell against both servers and checks oc against 3.5.1, and 3.5.1
/// against the outcome the cell exists to show.
fn cell(direction: Direction, before: &[&str], after: &[&str], expected: &Outcome) {
    let reference = transfer(&upstream().unwrap(), direction, before, after);
    assert_eq!(&reference, expected, "3.5.1 itself: {before:?} --server {after:?}");
    let actual = transfer(&oc_rsync(), direction, before, after);
    assert_eq!(actual, reference, "{before:?} --server {after:?}");
}

fn outcome(status: i32, files: &[&str], refusal: &[&str]) -> Outcome {
    Outcome {
        status: Some(status),
        files: files.iter().map(|f| (*f).to_owned()).collect(),
        refusal: refusal.iter().map(|r| (*r).to_owned()).collect(),
    }
}

/// `--exclude` behind `--server` and `-f` ahead of it both drop `d/b` on a
/// pull, where the client asked for everything.
#[test]
fn argv_rules_filter_a_pull() {
    if upstream().is_none() {
        return;
    }
    let without_b = outcome(0, &[".rsync-filter", "d/a", "d/g.tmp"], &[]);
    cell(Direction::Pull, &[], &["--exclude=b"], &without_b);
    cell(Direction::Pull, &[], &["--exclude", "b"], &without_b);
    cell(Direction::Pull, &["-f", "- b"], &[], &without_b);
}

/// One `-F` reads the per-directory `.rsync-filter`; ahead of `--server` it
/// counts twice and also hides the `.rsync-filter` file itself.
#[test]
fn rsync_filter_shortcut_counts_like_upstream() {
    if upstream().is_none() {
        return;
    }
    cell(
        Direction::Pull,
        &[],
        &["-F"],
        &outcome(0, &[".rsync-filter", "d/b", "d/g.tmp"], &[]),
    );
    cell(Direction::Pull, &["-F"], &[], &outcome(0, &["d/b", "d/g.tmp"], &[]));
}

/// A missing `--exclude-from` file ends the server with RERR_FILEIO, the
/// cause printed by rsyserr() as the server's own.
#[test]
fn missing_exclude_from_file_is_a_file_io_exit() {
    if upstream().is_none() {
        return;
    }
    cell(
        Direction::Pull,
        &[],
        &["--exclude-from=nope"],
        &outcome(
            12,
            &[],
            &[
                "rsync: [server] failed to open exclude file nope: No such file or directory (2)",
                "rsync error: error in file IO (code 11)[server",
            ],
        ),
    );
}

/// An argv protect rule keeps an extraneous destination file from a
/// `--delete` push, while the unprotected one goes.
#[test]
fn argv_protect_rule_survives_a_delete_push() {
    if upstream().is_none() {
        return;
    }
    cell(
        Direction::DeletePush,
        &[],
        &["-f", "P keep"],
        &outcome(0, &[".rsync-filter", "d/a", "d/b", "d/g.tmp", "keep"], &[]),
    );
}
