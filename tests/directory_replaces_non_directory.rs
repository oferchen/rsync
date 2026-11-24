//! A non-directory standing where a transferred directory must go is cleared
//! out of the way, as rsync 3.5.2 does on a push and on a pull.
//!
//! upstream: generator.c:1782 `gen_entry_stat(fname, file, &sx.st,
//! keep_dirlinks && is_dir)` stats the destination without following a
//! symlink, and generator.c:1876-1880 hands anything that is not a directory
//! to `delete_item(fname, sx.st.st_mode, del_opts | DEL_FOR_DIR)` before the
//! mkdir. For a non-directory that is delete.c:228-238: `make_backup()` under
//! `--backup`, `unlink` otherwise, and no `*deleting` line because
//! `DEL_MAKE_ROOM` is set (delete.c:241). `--delete` and `--force` only add
//! `DEL_RECURSE` (generator.c:1666), which a non-directory never consults, so
//! the obstacle is replaced with or without them. The directory is then new:
//! `statret = -1` makes `itemize()` print `cd+++++++++`, in a dry run too.
//!
//! Every cell runs the same transfer with the 3.5.2 binary and with oc and
//! compares exit code, stdout and the final tree.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use filetime::FileTime;
use test_support::{Deadlined, UpstreamVersion, require_upstream_rsync, run_deadlined};

const BUDGET: Duration = Duration::from_secs(120);

/// Source mtime, so a directory that received it can be told from one left
/// with the time of its creation.
const SOURCE_MTIME: i64 = 1_577_934_245;
const DEST_MTIME: i64 = 1_546_300_800;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Obstacle {
    File,
    Fifo,
    /// A relative link to a directory inside the destination.
    LinkToDir,
    LinkToFile,
    DanglingLink,
    /// A file at `ro/d1`, inside a directory the receiver cannot write to
    /// until it has raised it for the transfer (generator.c:1941-1949).
    FileInReadOnlyDir,
    /// Nothing in the way: `ro/` exists, empty and unwritable, and `ro/d1`
    /// has to be made inside it.
    EmptyReadOnlyDir,
}

const OBSTACLES: [Obstacle; 7] = [
    Obstacle::File,
    Obstacle::Fifo,
    Obstacle::LinkToDir,
    Obstacle::LinkToFile,
    Obstacle::DanglingLink,
    Obstacle::FileInReadOnlyDir,
    Obstacle::EmptyReadOnlyDir,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Push,
    Pull,
}

/// Option sets, `{bak}` standing for an absolute backup directory.
const FLAG_SETS: [&[&str]; 12] = [
    &["-ai"],
    &["-ai", "--delete-during"],
    &["-ai", "--delete-after"],
    &["-ai", "--force"],
    &["-aiK"],
    &["-ai", "-b"],
    &["-ai", "-b", "--backup-dir={bak}"],
    &["-ain"],
    &["-a"],
    // Without -p the new directory takes dest_mode()'s "absent" arm.
    &["-rti"],
    // The receiver driver that makes every directory of the list in one pass.
    &["-ai", "--no-inc-recursive"],
    &["-rti", "--no-inc-recursive"],
];

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    status: Option<i32>,
    stdout: String,
    stderr_is_empty: bool,
    tree: Vec<String>,
}

fn set_mtime(path: &Path, seconds: i64) {
    let time = FileTime::from_unix_time(seconds, 0);
    test_support::file_times::set_symlink_file_times(path, time, time).expect("set mtime");
}

/// `src/` holds `a`, `d1/f1`, `d1/sub/f2` and `ro/d1/x`; `dst/` holds
/// `obstacle` at `d1` (or at `ro/d1`), older than the source.
fn build_fixture(cell: &Path, obstacle: Obstacle) {
    let src = cell.join("src");
    fs::create_dir_all(src.join("d1/sub")).expect("create source tree");
    fs::create_dir_all(src.join("ro/d1")).expect("create source ro tree");
    fs::write(src.join("ro/d1/x"), "x\n").expect("write x");
    fs::write(src.join("a"), "a\n").expect("write a");
    fs::write(src.join("d1/f1"), "f1\n").expect("write f1");
    fs::write(src.join("d1/sub/f2"), "f2\n").expect("write f2");
    fs::set_permissions(src.join("d1"), fs::Permissions::from_mode(0o750)).expect("chmod d1");
    for name in [
        "ro/d1/x",
        "ro/d1",
        "ro",
        "d1/sub/f2",
        "d1/sub",
        "d1/f1",
        "d1",
        "a",
        "",
    ] {
        set_mtime(&src.join(name), SOURCE_MTIME);
    }

    let dst = cell.join("dst");
    fs::create_dir(&dst).expect("create dst");
    let in_read_only_dir = matches!(
        obstacle,
        Obstacle::FileInReadOnlyDir | Obstacle::EmptyReadOnlyDir
    );
    let d1 = if in_read_only_dir {
        fs::create_dir(dst.join("ro")).expect("create dst ro");
        dst.join("ro/d1")
    } else {
        dst.join("d1")
    };
    match obstacle {
        Obstacle::File | Obstacle::FileInReadOnlyDir => {
            fs::write(&d1, "not a directory\n").expect("write obstacle");
        }
        Obstacle::Fifo => {
            let status = Command::new("mkfifo")
                .arg(&d1)
                .status()
                .expect("spawn mkfifo");
            assert!(status.success(), "mkfifo {} failed: {status}", d1.display());
        }
        Obstacle::LinkToDir => {
            fs::create_dir(dst.join("real")).expect("create link target");
            fs::write(dst.join("real/keep"), "keep\n").expect("write keep");
            set_mtime(&dst.join("real/keep"), DEST_MTIME);
            set_mtime(&dst.join("real"), DEST_MTIME);
            symlink("real", &d1).expect("symlink to directory");
        }
        Obstacle::LinkToFile => {
            fs::write(cell.join("target-file"), "t\n").expect("write link target");
            symlink(cell.join("target-file"), &d1).expect("symlink to file");
        }
        Obstacle::DanglingLink => symlink(cell.join("nowhere"), &d1).expect("dangling symlink"),
        Obstacle::EmptyReadOnlyDir => {}
    }
    if obstacle != Obstacle::EmptyReadOnlyDir {
        set_mtime(&d1, DEST_MTIME);
    }
    if in_read_only_dir {
        set_mtime(&dst.join("ro"), DEST_MTIME);
        fs::set_permissions(dst.join("ro"), fs::Permissions::from_mode(0o555)).expect("chmod ro");
    }
    set_mtime(&dst, DEST_MTIME);
}

fn walk(root: &Path, relative: &Path, lines: &mut Vec<String>) {
    let path = root.join(relative);
    let Ok(meta) = fs::symlink_metadata(&path) else {
        return;
    };
    let file_type = meta.file_type();
    let mode = meta.permissions().mode() & 0o7777;
    let mtime = test_support::file_times::mtime_of(&meta).unix_seconds();
    let detail = if file_type.is_dir() {
        // A created directory that was not given the source time carries the
        // time of the run, which differs between the two binaries.
        format!("dir source-mtime={}", mtime == SOURCE_MTIME)
    } else if file_type.is_symlink() {
        let target = fs::read_link(&path).expect("read link");
        let target = target.strip_prefix(root).unwrap_or(&target).to_path_buf();
        format!("link mtime={mtime} -> {}", target.display())
    } else if file_type.is_fifo() {
        format!("fifo mtime={mtime}")
    } else {
        format!("file mtime={mtime}")
    };
    lines.push(format!("{} {mode:o} {detail}", relative.display()));
    if file_type.is_dir() {
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

/// Runs `client` against `server` over the fake remote shell.
fn run_cell(
    client: &Path,
    server: &Path,
    cell: &Path,
    obstacle: Obstacle,
    direction: Direction,
    flags: &[&str],
) -> Outcome {
    fs::create_dir(cell).expect("create cell");
    build_fixture(cell, obstacle);
    let rsh = cell.join("rsh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh");
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");

    let bak = cell.join("bak");
    let src = format!("{}/", cell.join("src").display());
    let dst = format!("{}/", cell.join("dst").display());
    let mut command = Command::new(client);
    command.current_dir(cell);
    for flag in flags {
        command.arg(flag.replace("{bak}", &bak.display().to_string()));
    }
    command
        .arg("-e")
        .arg(&rsh)
        .arg(format!("--rsync-path={}", server.display()));
    match direction {
        Direction::Push => command.arg(&src).arg(format!("localhost:{dst}")),
        Direction::Pull => command.arg(format!("localhost:{src}")).arg(&dst),
    };
    let (status, stdout, stderr) = match run_deadlined(&mut command, BUDGET).expect("spawn rsync") {
        Deadlined::Finished {
            status,
            stdout,
            stderr,
        } => (status, stdout, stderr),
        Deadlined::Expired { stdout, stderr, .. } => panic!(
            "{} did not finish {obstacle:?} {direction:?} {flags:?}\nstdout:\n{}\nstderr:\n{}",
            client.display(),
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr),
        ),
    };

    let mut tree = Vec::new();
    for top in ["dst", "bak"] {
        walk(cell, Path::new(top), &mut tree);
    }
    Outcome {
        status: status.code(),
        stdout: without_byte_totals(&String::from_utf8_lossy(&stdout)),
        stderr_is_empty: stderr.is_empty(),
        tree,
    }
}

/// Drops the two `-v` summary lines: the byte counts and the speed-up derived
/// from them belong to the wire accounting, not to what was done to the tree.
fn without_byte_totals(stdout: &str) -> String {
    stdout
        .split_inclusive('\n')
        .filter(|line| !line.starts_with("sent ") && !line.starts_with("total size is "))
        .collect()
}

/// 3.5.2 does not complete `-K` over a symlink to a directory: its attribute
/// pass refuses the kept link, so that pairing says nothing about replacing
/// an obstacle and is left out.
fn is_kept_dirlink(obstacle: Obstacle, flags: &[&str]) -> bool {
    obstacle == Obstacle::LinkToDir && flags.contains(&"-aiK")
}

fn compare_with_upstream(direction: Direction) {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let oc = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical tempdir");

    let mut differences = Vec::new();
    for obstacle in OBSTACLES {
        for (index, flags) in FLAG_SETS.into_iter().enumerate() {
            if is_kept_dirlink(obstacle, flags) {
                continue;
            }
            // The two cells share a name under different parents so that any
            // path either binary prints has the same length and shape.
            let name = format!("{obstacle:?}-{index}");
            let up_parent = root.join("up");
            let oc_parent = root.join("oc");
            fs::create_dir_all(&up_parent).expect("create up parent");
            fs::create_dir_all(&oc_parent).expect("create oc parent");
            let up = run_cell(
                upstream.binary(),
                upstream.binary(),
                &up_parent.join(&name),
                obstacle,
                direction,
                flags,
            );
            let ours = run_cell(&oc, &oc, &oc_parent.join(&name), obstacle, direction, flags);

            // The oracle must have replaced the obstacle, or agreeing with it
            // proves nothing about replacement.
            assert_eq!(
                up.status,
                Some(0),
                "3.5.2 {obstacle:?} {direction:?} {flags:?}: {up:#?}"
            );
            if flags.contains(&"-ain") {
                assert!(
                    up.stdout.contains("cd+++++++++ d1/\n"),
                    "3.5.2 dry run must itemize d1/ as new: {up:#?}"
                );
            } else {
                assert!(
                    up.tree
                        .iter()
                        .any(|line| line.contains(" dir ") && line.starts_with("dst/d1 "))
                        && up
                            .tree
                            .iter()
                            .any(|line| line.starts_with("dst/d1/sub/f2 "))
                        && up.tree.iter().any(|line| line.starts_with("dst/ro/d1/x ")),
                    "3.5.2 must end with d1 as a populated directory: {up:#?}"
                );
            }

            if up != ours {
                differences.push(format!(
                    "{obstacle:?} {direction:?} {flags:?}\n  3.5.2: {up:#?}\n  oc: {ours:#?}"
                ));
            }
        }
    }
    assert!(
        differences.is_empty(),
        "{} cell(s) differ from rsync 3.5.2:\n{}",
        differences.len(),
        differences.join("\n")
    );
}

/// The server is the receiver: its generator must clear the obstacle.
#[test]
fn a_push_replaces_a_non_directory_where_a_directory_goes_like_upstream() {
    compare_with_upstream(Direction::Push);
}

/// The client is the receiver.
#[test]
fn a_pull_replaces_a_non_directory_where_a_directory_goes_like_upstream() {
    compare_with_upstream(Direction::Pull);
}

/// Runs each `(obstacle, flags)` cell with 3.5.2 at both ends and again with
/// oc as the receiving end - `oc_receives` picks the client and server for
/// that run - and requires the two outcomes to be equal.
fn compare_cells(
    direction: Direction,
    cells: &[(Obstacle, &[&str])],
    oc_receives: impl Fn(&Path, &Path) -> (PathBuf, PathBuf),
) {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let oc = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical tempdir");
    fs::create_dir(root.join("up")).expect("create up parent");
    fs::create_dir(root.join("oc")).expect("create oc parent");

    let mut differences = Vec::new();
    for (index, (obstacle, flags)) in cells.iter().enumerate() {
        let name = format!("{obstacle:?}-{index}");
        let up = run_cell(
            upstream.binary(),
            upstream.binary(),
            &root.join("up").join(&name),
            *obstacle,
            direction,
            flags,
        );
        let (client, server) = oc_receives(upstream.binary(), &oc);
        let ours = run_cell(
            &client,
            &server,
            &root.join("oc").join(&name),
            *obstacle,
            direction,
            flags,
        );
        assert_eq!(
            up.status,
            Some(0),
            "3.5.2 {obstacle:?} {direction:?} {flags:?}: {up:#?}"
        );
        assert!(
            up.stdout.contains("d1/\n"),
            "3.5.2 must report d1/: {up:#?}"
        );
        if up != ours {
            differences.push(format!(
                "{obstacle:?} {direction:?} {flags:?}\n  3.5.2: {up:#?}\n  oc receiving: {ours:#?}"
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "{} cell(s) differ from rsync 3.5.2:\n{}",
        differences.len(),
        differences.join("\n")
    );
}

/// The pairings that put oc on the receiving side of a 3.5.2 peer.
const MIXED_CELLS: [(Obstacle, &[&str]); 9] = [
    (Obstacle::File, &["-ai"]),
    (Obstacle::File, &["-ai", "-b"]),
    (Obstacle::File, &["-ain"]),
    (Obstacle::Fifo, &["-ai"]),
    (Obstacle::Fifo, &["-ai", "-b"]),
    (Obstacle::Fifo, &["-ain"]),
    (Obstacle::LinkToDir, &["-ai"]),
    (Obstacle::LinkToDir, &["-ai", "-b"]),
    (Obstacle::LinkToDir, &["-ain"]),
];

/// Plain `-v` names the directory that replaced the obstacle. Push only: on
/// a pull the client receiver's `-v` listing differs from 3.5.2 with or
/// without an obstacle, which is a listing difference and not this rule.
const VERBOSE_CELLS: [(Obstacle, &[&str]); 2] =
    [(Obstacle::File, &["-av"]), (Obstacle::LinkToDir, &["-av"])];

/// A 3.5.2 client pushing to an oc server.
#[test]
fn an_upstream_push_into_an_oc_server_replaces_the_non_directory() {
    compare_cells(Direction::Push, &MIXED_CELLS, |upstream, oc| {
        (upstream.to_path_buf(), oc.to_path_buf())
    });
}

/// An oc client pulling from a 3.5.2 server.
#[test]
fn an_oc_pull_from_an_upstream_server_replaces_the_non_directory() {
    compare_cells(Direction::Pull, &MIXED_CELLS, |upstream, oc| {
        (oc.to_path_buf(), upstream.to_path_buf())
    });
}

#[test]
fn a_verbose_push_names_the_replacing_directory_like_upstream() {
    compare_cells(Direction::Push, &VERBOSE_CELLS, |_, oc| {
        (oc.to_path_buf(), oc.to_path_buf())
    });
}
