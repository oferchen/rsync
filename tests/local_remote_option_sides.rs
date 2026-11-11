//! A local copy's `-M` values reach its receiver half only.
//!
//! upstream: pipe.c:109-148 local_child() forks the receiver, which re-runs
//! parse_arguments() over the `-M` values on top of the options it inherited.
//! The sender keeps the client's own, so a sender-side option given with `-M`
//! changes nothing the sender does, and a receiver-side one still applies.
//! Every expectation below is what 3.5.1 produces for the same command.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// `src/d/{f,x.tmp,e/g}`.
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("src/d/e")).unwrap();
        fs::write(root.join("src/d/f"), "f\n").unwrap();
        fs::write(root.join("src/d/x.tmp"), "x\n").unwrap();
        fs::write(root.join("src/d/e/g"), "g\n").unwrap();
        Self { _temp: temp, root }
    }

    fn run(&self, args: &[&str]) -> Output {
        let out = Command::new(env!("CARGO_BIN_EXE_oc-rsync"))
            .args(args)
            .args(["src/", "dst/"])
            .current_dir(&self.root)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
        out
    }

    fn dst(&self, rel: &str) -> PathBuf {
        self.root.join("dst").join(rel)
    }
}

fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// `-r` is read by the sender's file-list walk alone (flist.c
/// send_file_list()), so `-d -M-r` copies `d` without its contents.
#[test]
fn remote_option_recursion_does_not_make_the_sender_recurse() {
    let fx = Fixture::new();
    fx.run(&["-d", "-M-r"]);
    assert!(fx.dst("d").is_dir());
    assert!(!exists(&fx.dst("d/f")), "the sender recursed into d");
}

/// `-r -M--no-r` still sends the whole tree: the receiver's `--no-r` does not
/// reach the sender's walk.
#[test]
fn remote_option_no_recursion_leaves_the_sender_recursing() {
    let fx = Fixture::new();
    fx.run(&["-r", "-M--no-r"]);
    assert!(fx.dst("d/e/g").is_file());
}

/// The sender names the files, so `-M-R` leaves them non-relative.
#[test]
fn remote_option_relative_leaves_the_sender_names_alone() {
    let fx = Fixture::new();
    fx.run(&["-r", "-M-R"]);
    assert!(fx.dst("d/f").is_file());
    assert!(!exists(&fx.dst("src")));
}

/// A `-M` exclude joins the receiver's filter list only: it does not hide a
/// file from the sender, but it protects a matching destination file from
/// `--delete` (generator.c delete_in_dir() -> is_excluded()).
#[test]
fn remote_option_exclude_protects_without_hiding() {
    let fx = Fixture::new();
    fs::create_dir_all(fx.dst("d")).unwrap();
    fs::write(fx.dst("d/y.tmp"), "y\n").unwrap();
    fs::write(fx.dst("d/z"), "z\n").unwrap();
    fx.run(&["-r", "--delete", "-M--exclude=*.tmp"]);
    assert!(fx.dst("d/x.tmp").is_file(), "the sender hid x.tmp");
    assert!(fx.dst("d/y.tmp").is_file(), "the receiver deleted y.tmp");
    assert!(!exists(&fx.dst("d/z")), "z was kept");
}

/// Without `-M` both halves read one set: the same exclude hides `x.tmp` and
/// still protects `y.tmp`.
#[test]
fn a_client_exclude_hides_and_protects() {
    let fx = Fixture::new();
    fs::create_dir_all(fx.dst("d")).unwrap();
    fs::write(fx.dst("d/y.tmp"), "y\n").unwrap();
    fx.run(&["-r", "--delete", "--exclude=*.tmp"]);
    assert!(!exists(&fx.dst("d/x.tmp")));
    assert!(fx.dst("d/y.tmp").is_file());
}
