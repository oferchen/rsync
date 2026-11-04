//! Local `-vv` listing order: the generator's notices and the sender's names
//! interleave as rsync 3.5.1 prints them, with and without `--progress`.
//!
//! upstream: generator.c:2803-2841 - the generator prints `is uptodate` the
//! moment it checks an entry, while each transferred name waits in its output
//! buffer until it blocks on the next list; sender.c:515-547 - the sender's
//! sub-list lookahead decides when that happens. The order itself is
//! `core::client::listing_order`; these cells hold the rendered output to it.
//!
//! # Gating
//!
//! The order cells always run. The oracle cell self-skips unless
//! `OC_RSYNC_UPSTREAM_COMPAT=1` and an upstream 3.5.1 binary resolves
//! (override with `OC_RSYNC_UPSTREAM_BIN_3_5_1`).
#![cfg(unix)]

use std::fs::{self, File};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use test_support::{
    UpstreamVersion, oc_rsync_bin, require_upstream_rsync, upstream_compat_enabled,
};

/// A source tree: each file's path and whether the copy transfers it. The
/// rest already sit up to date in the destination.
type Tree = Vec<(String, bool)>;

fn tree(entries: &[(&str, bool)]) -> Tree {
    entries
        .iter()
        .map(|&(path, transferred)| (path.to_owned(), transferred))
        .collect()
}

/// One file transferred among two up-to-date ones, in a single list.
fn flat() -> Tree {
    tree(&[("aaa", true), ("bbb", false), ("zzz", false)])
}

/// A sub-list whose transferred file sits between up-to-date ones; no names
/// are pending when the initial list ends, so upstream's order is fixed.
fn nested() -> Tree {
    tree(&[
        ("aaa", false),
        ("bbb", false),
        ("d/ccc", true),
        ("d/ddd", false),
        ("zzz", false),
    ])
}

/// Three directories of 500 files with `f001` and `f075` changed in each.
/// The sender's lookahead covers `x` and `y` but not `z`.
fn three_dirs_of_500() -> Tree {
    ["x", "y", "z"]
        .iter()
        .flat_map(|dir| (1..=500).map(move |i| (format!("{dir}/f{i:03}"), i == 1 || i == 75)))
        .collect()
}

/// Writes `tree` under `root/src` and its up-to-date part under `root/dst`,
/// with every file and directory time equal on both sides so that only the
/// transferred files and the notices print.
fn build(root: &Path, tree: &Tree) {
    let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_836_800);
    let set_time = |path: &Path| {
        File::open(path)
            .and_then(|file| file.set_modified(stamp))
            .expect("set mtime");
    };
    for (path, transferred) in tree {
        for side in ["src", "dst"] {
            let file = root.join(side).join(path);
            fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
            if side == "src" || !transferred {
                fs::write(&file, format!("content {path}\n")).expect("write");
                set_time(&file);
            }
        }
    }
    for side in ["src", "dst"] {
        let base = root.join(side);
        let mut dirs = vec![base.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in fs::read_dir(&dir).expect("read_dir") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    dirs.push(path);
                }
            }
            set_time(&dir);
        }
    }
}

/// Runs `binary -r -t <flags> src/ dst/` on a fresh copy of `tree` and returns
/// its stdout lines. Dropped, because they differ from upstream for reasons
/// other than the order under test: progress lines (task 579), the
/// `sent`/`total size` trailer (task 158), and the sender's `expand file_list
/// pointer array` debug lines, whose "did move" depends on realloc
/// (flist.c:flist_expand(), task 580).
fn listing(binary: &Path, tree: &Tree, flags: &[&str]) -> Vec<String> {
    let tmp = tempfile::tempdir().expect("tempdir");
    build(tmp.path(), tree);
    let output = Command::new(binary)
        .current_dir(tmp.path())
        .args(["-r", "-t"])
        .args(flags)
        .args(["src/", "dst/"])
        .output()
        .expect("spawn rsync");
    assert!(
        output.status.success(),
        "{} {flags:?} failed: {}",
        binary.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .split(['\r', '\n'])
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with(' ')
                && !line.starts_with("sent ")
                && !line.starts_with("total size ")
                && !line.starts_with("[sender] expand file_list pointer array")
        })
        .map(str::to_owned)
        .collect()
}

/// The notices and names alone: each run of `is uptodate` lines as `[NU]`.
fn signature(lines: &[String]) -> String {
    let mut out = Vec::new();
    let mut uptodate = 0;
    for line in lines {
        if line.ends_with(" is uptodate") {
            uptodate += 1;
            continue;
        }
        if line.starts_with("sending ") || line.starts_with("delta-") || line.starts_with("total:")
        {
            continue;
        }
        if uptodate > 0 {
            out.push(format!("[{uptodate}U]"));
            uptodate = 0;
        }
        out.push(line.clone());
    }
    if uptodate > 0 {
        out.push(format!("[{uptodate}U]"));
    }
    out.join(" ")
}

const FLAG_SETS: [&[&str]; 3] = [&["-vv"], &["-vv", "--progress"], &["-v", "--progress"]];

#[test]
fn notices_print_before_the_names_their_list_holds_back() {
    // A live --progress copy reaches `aaa` first, but upstream's generator has
    // already printed both notices by the time the sender names `aaa`.
    let oc = oc_rsync_bin();
    for flags in [&["-vv"][..], &["-vv", "--progress"]] {
        assert_eq!(
            signature(&listing(&oc, &flat(), flags)),
            "[2U] aaa",
            "{flags:?}"
        );
        assert_eq!(
            signature(&listing(&oc, &nested(), flags)),
            "[4U] d/ccc",
            "{flags:?}"
        );
    }
}

#[test]
fn a_large_tree_prints_window_by_window() {
    // The generator flushes x's and y's names only on reaching z, the first
    // list the sender's 1000-entry lookahead had not sent.
    let oc = oc_rsync_bin();
    for flags in [&["-vv"][..], &["-vv", "--progress"]] {
        assert_eq!(
            signature(&listing(&oc, &three_dirs_of_500(), flags)),
            "[996U] x/f001 x/f075 y/f001 y/f075 [498U] z/f001 z/f075",
            "{flags:?}"
        );
    }
}

#[test]
fn listing_matches_upstream_3_5_1() {
    if !upstream_compat_enabled() {
        return;
    }
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let oc = oc_rsync_bin();
    for (name, tree) in [
        ("flat", flat()),
        ("nested", nested()),
        ("three dirs of 500", three_dirs_of_500()),
    ] {
        for flags in FLAG_SETS {
            assert_eq!(
                listing(&oc, &tree, flags),
                listing(upstream.binary(), &tree, flags),
                "{name} {flags:?}"
            );
        }
    }
}
