//! A local `--write-batch` whose file lists span several incremental
//! sub-lists must replay into an exact copy of the source.
//!
//! Each record in the batch names its file by NDX, so one entry listed under
//! the wrong index shifts every later record onto the wrong file. The trees
//! here are built to make that visible:
//!
//! - more than `MIN_FILECNT_LOOKAHEAD` (1000) entries, so the sender has to
//!   interleave sub-lists with file data, plus nested and empty directories;
//! - hard-link groups whose members sit in different directories, on both
//!   sides of the 1000-entry boundary, so a group's data goes under its
//!   sorted-last member in a later sub-list;
//! - the same tree with `--no-inc-recursive`, one flat list.
//!
//! Every file has distinct content, so a record that lands on the wrong file
//! changes the replayed tree. The replay is `oc-rsync --read-batch` always,
//! and rsync 3.5.2's `--read-batch` when the oracle resolves
//! (`OC_RSYNC_UPSTREAM_BIN_3_5_2`, required under `OC_RSYNC_UPSTREAM_COMPAT=1`).
//!
//! # Upstream Reference
//!
//! - `sender.c:523-555` - sub-lists go out between file-data records
//! - `flist.c:send_extra_file_list()`, `flist.c:3503,3511` - each list's
//!   `ndx_start`
//! - `hlink.c:113-208 match_gnums()` - a group's data under its last member

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tempfile::tempdir;
use test_support::{Deadlined, UpstreamVersion, run_deadlined};

const TIMEOUT: Duration = Duration::from_secs(120);

/// A tree past the lookahead: 12 directories of 120 files, nested and empty
/// directories, and a top-level file.
fn build_tree(root: &Path) {
    for a in 1..=12 {
        let dir = root.join(format!("d{a:02}"));
        fs::create_dir_all(dir.join("e")).expect("mkdir");
        fs::create_dir_all(dir.join("empty")).expect("mkdir");
        for f in 1..=120 {
            fs::write(dir.join(format!("f{f:03}")), format!("{a}/{f}\n").repeat(f % 7 + 1))
                .expect("write");
        }
        fs::write(dir.join("e").join("g"), format!("deep {a}\n")).expect("write");
    }
    fs::write(root.join("top"), b"top\n").expect("write");
}

/// Hard-link groups across directories and across the lookahead boundary:
/// 1100 files in `z`, with groups whose members sit before and after it.
fn build_hard_links(root: &Path) {
    for dir in ["a/b", "c", "z"] {
        fs::create_dir_all(root.join(dir)).expect("mkdir");
    }
    for f in 1..=1100 {
        fs::write(root.join("z").join(format!("n{f:04}")), format!("n{f}\n")).expect("write");
    }
    fs::write(root.join("a/b/h1"), b"one\n").expect("write");
    fs::hard_link(root.join("a/b/h1"), root.join("c/h1")).expect("link");
    fs::hard_link(root.join("a/b/h1"), root.join("z/h1")).expect("link");
    fs::write(root.join("c/k"), b"two\n").expect("write");
    fs::hard_link(root.join("c/k"), root.join("k")).expect("link");
    fs::hard_link(root.join("z/n0005"), root.join("a/n0005")).expect("link");
}

/// What a replay must reproduce: every path's kind and content, and which
/// paths share an inode.
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    entries: BTreeMap<PathBuf, Option<Vec<u8>>>,
    links: Vec<Vec<PathBuf>>,
}

fn snapshot(root: &Path) -> Snapshot {
    fn walk(root: &Path, dir: &Path, entries: &mut BTreeMap<PathBuf, Option<Vec<u8>>>,
            inodes: &mut BTreeMap<u64, Vec<PathBuf>>) {
        for entry in fs::read_dir(dir).expect("read_dir") {
            let path = entry.expect("dir entry").path();
            let relative = path.strip_prefix(root).expect("under root").to_path_buf();
            let meta = fs::symlink_metadata(&path).expect("lstat");
            if meta.is_dir() {
                entries.insert(relative, None);
                walk(root, &path, entries, inodes);
            } else {
                entries.insert(relative.clone(), Some(fs::read(&path).expect("read")));
                if meta.nlink() > 1 {
                    inodes.entry(meta.ino()).or_default().push(relative);
                }
            }
        }
    }
    let mut entries = BTreeMap::new();
    let mut inodes = BTreeMap::new();
    walk(root, root, &mut entries, &mut inodes);
    let mut links: Vec<Vec<PathBuf>> = inodes
        .into_values()
        .map(|mut group| {
            group.sort();
            group
        })
        .collect();
    links.sort();
    Snapshot { entries, links }
}

fn run(cmd: &mut Command, what: &str) {
    let Deadlined::Finished { status, stderr, .. } =
        run_deadlined(cmd, TIMEOUT).unwrap_or_else(|e| panic!("spawn {what}: {e}"))
    else {
        panic!("{what} timed out after {TIMEOUT:?}");
    };
    assert!(status.success(), "{what} failed: {}", String::from_utf8_lossy(&stderr));
}

/// Writes a batch of `source` with `args`, then replays it with each reader
/// and compares the result with the source.
fn assert_replays(build: fn(&Path), args: &[&str]) {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    fs::create_dir(&source).expect("mkdir");
    build(&source);
    let batch = temp.path().join("batch");
    let mut src_arg = source.clone().into_os_string();
    src_arg.push("/");
    run(
        Command::new(test_support::oc_rsync_bin())
            .arg("-rt")
            .args(args)
            .arg(format!("--write-batch={}", batch.display()))
            .arg(&src_arg)
            .arg(temp.path().join("written")),
        "oc-rsync --write-batch",
    );
    let expected = snapshot(&source);
    assert_eq!(snapshot(&temp.path().join("written")), expected, "the writing run's own copy");

    let mut readers = vec![("oc-rsync", test_support::oc_rsync_bin())];
    if let Some(oracle) = test_support::require_upstream_rsync(UpstreamVersion::V3_5_2) {
        readers.push(("rsync 3.5.2", oracle.binary().to_path_buf()));
    }
    for (name, reader) in readers {
        let dest = temp.path().join(format!("replay-{}", name.replace(' ', "-")));
        run(
            Command::new(&reader)
                .arg("-rt")
                .args(args)
                .arg(format!("--read-batch={}", batch.display()))
                .arg(&dest),
            &format!("{name} --read-batch"),
        );
        assert_eq!(snapshot(&dest), expected, "{name} replay of the batch");
    }
}

#[test]
fn a_batch_spanning_many_sub_lists_replays_exactly() {
    assert_replays(build_tree, &[]);
}

#[test]
fn a_flat_batch_of_the_same_tree_replays_exactly() {
    assert_replays(build_tree, &["--no-inc-recursive"]);
}

#[test]
fn hard_link_groups_across_sub_lists_replay_exactly() {
    assert_replays(build_hard_links, &["-H"]);
}

#[test]
fn hard_link_groups_in_a_flat_batch_replay_exactly() {
    assert_replays(build_hard_links, &["-H", "--no-inc-recursive"]);
}
