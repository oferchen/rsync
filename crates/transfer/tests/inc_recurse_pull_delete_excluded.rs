//! `--delete-excluded` under an incremental-recursion pull, where the receiver
//! deletes per directory as each sub-list arrives.
//!
//! upstream: exclude.c:1479-1481 parse_rule_tok() is the only place the flag
//! acts. It turns an un-modified include/exclude into a sender-side rule when
//! the rule is parsed, whether it comes from the command line or from a
//! per-directory merge file, and the receiver then ignores it. rule_matches()
//! has no `delete_excluded` term, so a rule scoped to the receiver (`-r`)
//! still protects its matches. Every expectation below was taken from
//! rsync 3.5.1 on the same tree.
#![cfg(unix)]

use std::fs;
use std::path::Path;

use test_support::{
    LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};

/// Twelve directories, so the deletions happen in per-segment passes rather
/// than in one sweep of a single list.
const DIRS: usize = 12;

/// Destination-only entries each cell decides about.
const EXCLUDED: &str = "junk.tmp";
const RECEIVER_ONLY: &str = "x.keep";
const MERGE_EXCLUDED: &str = "m.merge";

fn fixture(src: &Path, dest: &Path) {
    for dir in 1..=DIRS {
        let path = src.join(format!("d{dir}/s/keep"));
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir src");
        fs::write(&path, b"keep").expect("write src file");

        let sub = dest.join(format!("d{dir}/s"));
        fs::create_dir_all(&sub).expect("mkdir dest");
        for name in ["stale", EXCLUDED, RECEIVER_ONLY, MERGE_EXCLUDED] {
            fs::write(sub.join(name), name).expect("write dest-only file");
        }
        fs::write(sub.join(".rsync-filter"), format!("- {MERGE_EXCLUDED}\n"))
            .expect("write per-dir merge file");
    }
}

/// Pulls `src` into `dest` with `--delete` plus `rules`, and returns how
/// many of the twelve directories still hold each destination-only entry.
fn pull(rules: &[&str], delete_excluded: bool) -> Survivors {
    let tmp = create_tempdir();
    let (src, dest) = (tmp.path().join("src"), tmp.path().join("dest"));
    fixture(&src, &dest);
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let mut runner = OcRsyncCliRunner::new()
        .arg("-r")
        .arg("--delete")
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ));
    for rule in rules {
        runner = runner.arg(rule);
    }
    if delete_excluded {
        runner = runner.arg("--delete-excluded");
    }
    runner
        .arg(format!("localhost:{}/", src.display()))
        .arg(format!("{}/", dest.display()))
        .run()
        .expect("transfer run")
        .assert_exit(0);

    let count = |name: &str| {
        (1..=DIRS)
            .filter(|dir| dest.join(format!("d{dir}/s/{name}")).exists())
            .count()
    };
    assert_eq!(count("keep"), DIRS, "every source file arrives");
    assert_eq!(count("stale"), 0, "extraneous files are deleted");
    Survivors {
        excluded: count(EXCLUDED),
        receiver_only: count(RECEIVER_ONLY),
        merge_excluded: count(MERGE_EXCLUDED),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Survivors {
    excluded: usize,
    receiver_only: usize,
    merge_excluded: usize,
}

const ALL: usize = DIRS;

#[test]
fn delete_excluded_deletes_what_a_command_line_exclude_covers() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let got = pull(&["--exclude=*.tmp"], true);
    assert_eq!(got.excluded, 0, "{got:?}");
}

/// The control: the same exclude protects without --delete-excluded.
#[test]
fn a_command_line_exclude_protects_without_delete_excluded() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let got = pull(&["--exclude=*.tmp"], false);
    assert_eq!(got.excluded, ALL, "{got:?}");
}

/// A receiver-side rule is not un-modified, so exclude.c:1479 leaves it
/// alone and it keeps protecting its matches under --delete-excluded.
#[test]
fn a_receiver_side_exclude_still_protects_under_delete_excluded() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let got = pull(&["--filter=-r *.keep"], true);
    assert_eq!(got.receiver_only, ALL, "{got:?}");
}

/// Rules read from a per-directory merge file on the receiver are parsed
/// under --delete-excluded too, so they stop protecting.
#[test]
fn delete_excluded_deletes_what_a_per_directory_merge_rule_covers() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let got = pull(&["--filter=: .rsync-filter"], true);
    assert_eq!(got.merge_excluded, 0, "{got:?}");
}

/// The control: the merge file's exclude protects without --delete-excluded.
#[test]
fn a_per_directory_merge_rule_protects_without_delete_excluded() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let got = pull(&["--filter=: .rsync-filter"], false);
    assert_eq!(got.merge_excluded, ALL, "{got:?}");
}
