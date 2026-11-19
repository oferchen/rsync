//! `-H` links a name only to a member of its group that this run produced
//! or verified.
//!
//! The receiver requests one member of a hard-link group and links the rest
//! to it afterwards. When that member was not transferred - `--update` or
//! `--ignore-existing` kept the destination's own file, a size limit skipped
//! it, or the sender could not open it - the link pass took any member that
//! happened to exist at the destination and linked the group's new names to
//! it. The run exited 0 with names holding data the source never had.
//!
//! upstream: generator.c:2585-2606 `handle_skipped_hlink()` and
//! hlink.c:601-621 `skip_hard_link()` mark a skipped member
//! `FLAG_SKIP_HLINK`; hlink.c:260-295 `check_prior()` never links to one, and
//! hlink.c:313-324 `hard_link_check()` makes the next member the group's
//! "virtual first", which is transferred like any other file. A size limit
//! (generator.c:2159-2173) skips without that call, and every member has the
//! same size, so nothing of the group is created.
//!
//! Each cell compares the destination tree and the exit code with those of
//! rsync 3.5.2 as the receiver, on a push and on a pull.

#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use filetime::FileTime;
use test_support::{Deadlined, UpstreamVersion, require_upstream_rsync, run_deadlined};

const BUDGET: Duration = Duration::from_secs(120);
const SOURCE_MTIME: i64 = 1_577_934_245;
const OLDER_MTIME: i64 = 1_546_300_800;
const NEWER_MTIME: i64 = 1_704_067_200;

/// The group's data at the source, and what a destination holds before a run.
const SOURCE_DATA: &str = "newcontent\n";
const NEWER_DATA: &str = "newer-at-dest\n";
const OLD_DATA: &str = "old\n";

/// Every name below a directory with its size, and the sets of names that
/// share an inode.
type Tree = (BTreeMap<String, u64>, BTreeSet<Vec<String>>);

struct Cell {
    /// The options given besides `-aH`.
    options: fn(&Path) -> Vec<String>,
    /// Fills the destination a run starts from.
    prepare: fn(&Path, &Path),
    /// The source tree: `src`, or `locked`, whose group no sender can open.
    source: &'static str,
    /// The exit code of the run.
    exit_code: i32,
    /// rsync 3.5.2 does not finish this cell under incremental recursion
    /// (rsync issue 1177), so its tree is taken without it.
    oracle_without_inc_recurse: bool,
    /// What the tree rsync 3.5.2 leaves must show for the cell to mean
    /// anything.
    check_oracle: fn(&Tree),
}

fn set_mtime(path: &Path, seconds: i64) {
    let time = FileTime::from_unix_time(seconds, 0);
    filetime::set_file_times(path, time, time).expect("set mtime");
}

fn write_file(path: &Path, data: &str, mtime: i64) {
    fs::write(path, data).expect("write file");
    set_mtime(path, mtime);
}

/// One group, `a1`, `a2`, `a3`, in each of two trees. No sender can open the
/// group in `locked`.
fn build_sources(root: &Path) {
    for tree in ["src", "locked"] {
        let dir = root.join(tree);
        fs::create_dir_all(&dir).expect("create source");
        write_file(&dir.join("a1"), SOURCE_DATA, SOURCE_MTIME);
        for follower in ["a2", "a3"] {
            fs::hard_link(dir.join("a1"), dir.join(follower)).expect("link follower");
        }
    }
    fs::set_permissions(root.join("locked/a1"), fs::Permissions::from_mode(0o000))
        .expect("lock the group");
    let rsh = root.join("rsh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh");
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
}

fn snapshot(dir: &Path) -> Tree {
    let mut names = BTreeMap::new();
    let mut inodes: BTreeMap<u64, Vec<String>> = BTreeMap::new();
    for entry in fs::read_dir(dir).expect("read dir") {
        let path = entry.expect("dir entry").path();
        let meta = fs::symlink_metadata(&path).expect("lstat");
        let name = path
            .file_name()
            .expect("a name")
            .to_string_lossy()
            .into_owned();
        names.insert(name.clone(), meta.len());
        inodes.entry(meta.ino()).or_default().push(name);
    }
    let groups = inodes
        .into_values()
        .map(|mut group| {
            group.sort();
            group
        })
        .collect();
    (names, groups)
}

fn size_of(data: &str) -> u64 {
    data.len() as u64
}

fn names_of(tree: &Tree) -> Vec<&str> {
    tree.0.keys().map(String::as_str).collect()
}

fn group(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// Transfers the cell's source into a fresh copy of its starting destination
/// and returns the tree the run leaves.
fn transfer(
    root: &Path,
    cell: &Cell,
    client: &Path,
    server: &Path,
    pull: bool,
    inc: &[&str],
    tag: &str,
) -> Tree {
    let dst = root.join(format!("dst-{tag}"));
    fs::create_dir_all(&dst).expect("create destination");
    (cell.prepare)(root, &dst);
    let remote = |path: &Path| format!("localhost:{}/", path.display());
    let local = |path: &Path| format!("{}/", path.display());
    let src = root.join(cell.source);
    let (from, to) = if pull {
        (remote(&src), local(&dst))
    } else {
        (local(&src), remote(&dst))
    };
    let mut command = Command::new(client);
    command
        .current_dir(root)
        .arg("-aH")
        .args((cell.options)(root))
        .args(inc)
        .arg("-e")
        .arg(root.join("rsh"))
        .arg(format!("--rsync-path={}", server.display()))
        .arg(from)
        .arg(to);
    match run_deadlined(&mut command, BUDGET).expect("spawn rsync") {
        Deadlined::Finished { status, stderr, .. } => assert_eq!(
            status.code(),
            Some(cell.exit_code),
            "{tag}: {}",
            String::from_utf8_lossy(&stderr)
        ),
        Deadlined::Expired { stderr, .. } => {
            panic!("{tag} did not finish: {}", String::from_utf8_lossy(&stderr))
        }
    }
    snapshot(&dst)
}

fn compare_with_upstream(cell: &Cell, pull: bool) {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let up = upstream.binary();
    let oc = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical tempdir");
    build_sources(&root);

    for inc in [&[][..], &["--no-inc-recursive"]] {
        let tag = inc.join("");
        let oracle_inc = if cell.oracle_without_inc_recurse {
            &["--no-inc-recursive"][..]
        } else {
            inc
        };
        // The receiver is the client on a pull and the server on a push.
        let (reference, ours) = if pull {
            (
                transfer(&root, cell, up, up, true, oracle_inc, &format!("up{tag}")),
                transfer(&root, cell, &oc, up, true, inc, &format!("oc{tag}")),
            )
        } else {
            (
                transfer(&root, cell, up, up, false, oracle_inc, &format!("up{tag}")),
                transfer(&root, cell, up, &oc, false, inc, &format!("oc{tag}")),
            )
        };
        (cell.check_oracle)(&reference);
        assert_eq!(ours, reference, "{inc:?}: the destination tree");
    }
}

fn no_options(_root: &Path) -> Vec<String> {
    Vec::new()
}

fn stale_first_member(_root: &Path, dst: &Path) {
    write_file(&dst.join("a1"), OLD_DATA, OLDER_MTIME);
}

/// The first member is kept as the destination has it, and the other two are
/// new and linked to each other.
fn first_member_kept(tree: &Tree, kept: &str) {
    let (names, groups) = tree;
    assert_eq!(names.get("a1"), Some(&size_of(kept)), "3.5.2 keeps a1");
    for created in ["a2", "a3"] {
        assert_eq!(
            names.get(created),
            Some(&size_of(SOURCE_DATA)),
            "3.5.2 creates {created} from the source: {names:?}"
        );
    }
    assert!(
        groups.contains(&group(&["a2", "a3"])),
        "3.5.2 links a2 and a3 to each other only: {groups:?}"
    );
}

/// Nothing of the group is created beside the stale first member.
fn only_the_stale_member(tree: &Tree, stale: &str) {
    assert_eq!(names_of(tree), [stale], "3.5.2 creates nothing");
    assert_eq!(tree.0.get(stale), Some(&size_of(OLD_DATA)));
}

const UPDATE: Cell = Cell {
    options: |_| vec!["--update".to_owned()],
    prepare: |_, dst| write_file(&dst.join("a1"), NEWER_DATA, NEWER_MTIME),
    source: "src",
    exit_code: 0,
    oracle_without_inc_recurse: false,
    check_oracle: |tree| first_member_kept(tree, NEWER_DATA),
};

const IGNORE_EXISTING: Cell = Cell {
    options: |_| vec!["--ignore-existing".to_owned()],
    prepare: stale_first_member,
    source: "src",
    exit_code: 0,
    oracle_without_inc_recurse: false,
    check_oracle: |tree| first_member_kept(tree, OLD_DATA),
};

const MAX_SIZE: Cell = Cell {
    options: |_| vec!["--max-size=1".to_owned()],
    prepare: stale_first_member,
    source: "src",
    exit_code: 0,
    oracle_without_inc_recurse: false,
    check_oracle: |tree| only_the_stale_member(tree, "a1"),
};

const MIN_SIZE: Cell = Cell {
    options: |_| vec!["--min-size=1M".to_owned()],
    prepare: stale_first_member,
    source: "src",
    exit_code: 0,
    oracle_without_inc_recurse: false,
    check_oracle: |tree| only_the_stale_member(tree, "a1"),
};

/// The sender can open no member, so no transfer completes; `a2` holds what
/// an earlier run left.
const FAILED_TRANSFER: Cell = Cell {
    options: no_options,
    prepare: |_, dst| write_file(&dst.join("a2"), OLD_DATA, OLDER_MTIME),
    source: "locked",
    exit_code: 23,
    oracle_without_inc_recurse: true,
    check_oracle: |tree| only_the_stale_member(tree, "a2"),
};

/// An identical `a1` in the `--compare-dest` directory keeps `a1` out of the
/// transfer, yet the group is created whole.
///
/// upstream: generator.c:2197-2202 - `try_dests_reg()` returning -2 leaves
/// the name absent without `handle_skipped_hlink()`, so the next member is
/// transferred and `a1` is linked to it.
const COMPARE_DEST: Cell = Cell {
    options: |root| vec![format!("--compare-dest={}", root.join("alt").display())],
    prepare: |root, _| {
        let alt = root.join("alt");
        fs::create_dir_all(&alt).expect("create alt");
        write_file(&alt.join("a1"), SOURCE_DATA, SOURCE_MTIME);
    },
    source: "src",
    exit_code: 0,
    oracle_without_inc_recurse: false,
    check_oracle: |tree| {
        assert!(
            tree.1.contains(&group(&["a1", "a2", "a3"])),
            "3.5.2 creates the whole group: {tree:?}"
        );
    },
};

#[test]
fn a_pull_with_update_keeps_the_newer_member_out_of_the_group() {
    compare_with_upstream(&UPDATE, true);
}

#[test]
fn a_push_with_update_keeps_the_newer_member_out_of_the_group() {
    compare_with_upstream(&UPDATE, false);
}

#[test]
fn a_pull_with_ignore_existing_keeps_the_existing_member_out_of_the_group() {
    compare_with_upstream(&IGNORE_EXISTING, true);
}

#[test]
fn a_push_with_ignore_existing_keeps_the_existing_member_out_of_the_group() {
    compare_with_upstream(&IGNORE_EXISTING, false);
}

#[test]
fn a_pull_with_max_size_links_nothing_to_the_stale_member() {
    compare_with_upstream(&MAX_SIZE, true);
}

#[test]
fn a_push_with_max_size_links_nothing_to_the_stale_member() {
    compare_with_upstream(&MAX_SIZE, false);
}

#[test]
fn a_pull_with_min_size_links_nothing_to_the_stale_member() {
    compare_with_upstream(&MIN_SIZE, true);
}

#[test]
fn a_push_with_min_size_links_nothing_to_the_stale_member() {
    compare_with_upstream(&MIN_SIZE, false);
}

#[test]
fn a_pull_whose_transfer_fails_links_nothing_to_the_stale_member() {
    compare_with_upstream(&FAILED_TRANSFER, true);
}

#[test]
fn a_push_whose_transfer_fails_links_nothing_to_the_stale_member() {
    compare_with_upstream(&FAILED_TRANSFER, false);
}

#[test]
fn a_pull_with_compare_dest_creates_the_whole_group() {
    compare_with_upstream(&COMPARE_DEST, true);
}

#[test]
fn a_push_with_compare_dest_creates_the_whole_group() {
    compare_with_upstream(&COMPARE_DEST, false);
}
