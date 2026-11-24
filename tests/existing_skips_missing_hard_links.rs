//! `--existing` creates nothing, hard links included.
//!
//! upstream: generator.c:1794-1810 - under `--existing` an entry whose
//! destination is missing is skipped before `hard_link_check()` is reached
//! (generator.c:1980), so a hard-linked name that does not exist at the
//! destination is not linked to a member that does. The receiver linked it:
//! with `a1` present and `a2`, `d1/a3` absent, the destination ended with
//! three names for `a1` where rsync 3.5.2 leaves one.
//!
//! Each cell compares the destination tree with the one rsync 3.5.2 leaves as
//! the receiver, on a push and on a pull.

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
const DEST_MTIME: i64 = 1_546_300_800;

fn set_mtime(path: &Path, seconds: i64) {
    let time = FileTime::from_unix_time(seconds, 0);
    test_support::file_times::set_file_times(path, time, time).expect("set mtime");
}

/// Three groups: `a1`, `a2`, `d1/a3`; `b1`, `b2`, `d1/b3`; `c1`, `c2`.
fn build_source(root: &Path) {
    let src = root.join("src");
    fs::create_dir_all(src.join("d1")).expect("create src/d1");
    for (leader, content, followers) in [
        ("a1", "g1\n", &["a2", "d1/a3"][..]),
        ("b1", "g2\n", &["b2", "d1/b3"]),
        ("c1", "g3-new\n", &["c2"]),
    ] {
        fs::write(src.join(leader), content).expect("write leader");
        for follower in followers {
            fs::hard_link(src.join(leader), src.join(follower)).expect("link follower");
        }
        set_mtime(&src.join(leader), SOURCE_MTIME);
    }
    let rsh = root.join("rsh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh");
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
}

/// The destination a run starts from: `a1` is up to date and its two other
/// names are missing, the `b` group is missing altogether, and `c1` is out of
/// date with `c2` linked to it.
fn build_destination(dst: &Path) {
    fs::create_dir_all(dst.join("d1")).expect("create dst/d1");
    fs::write(dst.join("a1"), "g1\n").expect("write a1");
    set_mtime(&dst.join("a1"), SOURCE_MTIME);
    fs::write(dst.join("c1"), "g3-old-longer\n").expect("write c1");
    fs::hard_link(dst.join("c1"), dst.join("c2")).expect("link c2");
    set_mtime(&dst.join("c1"), DEST_MTIME);
}

/// Every name below `dir` with its size, and the sets of names that share an
/// inode.
type Tree = (BTreeMap<String, u64>, BTreeSet<Vec<String>>);

fn snapshot(dir: &Path) -> Tree {
    let mut names = BTreeMap::new();
    let mut inodes: BTreeMap<u64, Vec<String>> = BTreeMap::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current).expect("read dir") {
            let path = entry.expect("dir entry").path();
            let meta = fs::symlink_metadata(&path).expect("lstat");
            let name = path
                .strip_prefix(dir)
                .expect("below the root")
                .to_string_lossy()
                .into_owned();
            if meta.is_dir() {
                pending.push(path);
                names.insert(format!("{name}/"), 0);
            } else {
                names.insert(name.clone(), meta.len());
                inodes.entry(meta.ino()).or_default().push(name);
            }
        }
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

/// Transfers `src/` into a fresh copy of the starting destination and
/// returns the tree it leaves.
fn transfer(
    root: &Path,
    client: &Path,
    server: &Path,
    pull: bool,
    inc: &[&str],
    tag: &str,
) -> Tree {
    let dst = root.join(format!("dst-{tag}"));
    build_destination(&dst);
    let remote = |path: &Path| format!("localhost:{}/", path.display());
    let local = |path: &Path| format!("{}/", path.display());
    let src = root.join("src");
    let (from, to) = if pull {
        (remote(&src), local(&dst))
    } else {
        (local(&src), remote(&dst))
    };
    let mut command = Command::new(client);
    command
        .current_dir(root)
        .args(["-aH", "--existing"])
        .args(inc)
        .arg("-e")
        .arg(root.join("rsh"))
        .arg(format!("--rsync-path={}", server.display()))
        .arg(from)
        .arg(to);
    match run_deadlined(&mut command, BUDGET).expect("spawn rsync") {
        Deadlined::Finished { status, stderr, .. } => assert!(
            status.success(),
            "{tag}: {}",
            String::from_utf8_lossy(&stderr)
        ),
        Deadlined::Expired { stderr, .. } => {
            panic!("{tag} did not finish: {}", String::from_utf8_lossy(&stderr))
        }
    }
    snapshot(&dst)
}

fn compare_with_upstream(pull: bool) {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let up = upstream.binary();
    let oc = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical tempdir");
    build_source(&root);

    for inc in [&[][..], &["--no-inc-recursive"]] {
        let tag = inc.join("");
        // The receiver is the client on a pull and the server on a push.
        let (reference, ours) = if pull {
            (
                transfer(&root, up, up, true, inc, &format!("up{tag}")),
                transfer(&root, &oc, up, true, inc, &format!("oc{tag}")),
            )
        } else {
            (
                transfer(&root, up, up, false, inc, &format!("up{tag}")),
                transfer(&root, up, &oc, false, inc, &format!("oc{tag}")),
            )
        };
        // The oracle must have updated what exists and created nothing, or
        // agreeing with it says nothing about the missing names.
        let (names, groups) = &reference;
        assert_eq!(names.get("c1"), Some(&7), "3.5.2 {inc:?} updates c1");
        for missing in ["a2", "d1/a3", "b1", "b2", "d1/b3"] {
            assert!(
                !names.contains_key(missing),
                "3.5.2 {inc:?} must not create {missing}: {names:?}"
            );
        }
        assert!(
            groups.contains(&vec!["c1".to_owned(), "c2".to_owned()]),
            "3.5.2 {inc:?} keeps c2 linked to c1: {groups:?}"
        );
        assert_eq!(ours, reference, "{inc:?}: the destination tree");
    }
}

#[test]
fn a_pull_with_existing_links_no_missing_name() {
    compare_with_upstream(true);
}

#[test]
fn a_push_with_existing_links_no_missing_name() {
    compare_with_upstream(false);
}
