//! A `/***` rule is wild as a whole, so a `\` in its stem is an escape.
//!
//! upstream: exclude.c:326-334 - add_rule() appends any `/***` suffix to the
//! pattern first, then sets FILTRULE_WILD from `strpbrk(rule->pattern, "*[?")`
//! on the whole pattern. wildmatch therefore reads `a\b/***` as the directory
//! `ab` and everything below it; a directory literally named `a\b` is not
//! matched. Measured with rsync 3.5.2: `--exclude 'a\b/***'` on a tree holding
//! `ab/x`, `a\b/x` and `keep` transfers `a\b`, `a\b/x` and `keep`, exit 0, on a
//! local copy and on a push.
//!
//! When the 3.5.2 oracle resolves (`OC_RSYNC_UPSTREAM_BIN_3_5_2`), each cell
//! runs the same transfer with it and must match.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use test_support::{
    Deadlined, UpstreamVersion, oc_rsync_bin, require_upstream_rsync, run_deadlined,
};

const BUDGET: Duration = Duration::from_secs(120);
const RULE: &str = "a\\b/***";

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    /// `src/ab/x`, `src/a\b/x`, `src/keep`, and a remote shell that drops the host.
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical tempdir");
        let src = root.join("src");
        for (dir, body) in [("ab", "x\n"), ("a\\b", "y\n")] {
            fs::create_dir_all(src.join(dir)).expect("mkdir source dir");
            fs::write(src.join(dir).join("x"), body).expect("write source file");
        }
        fs::write(src.join("keep"), "z\n").expect("write keep");
        let rsh = root.join("lsh");
        fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh");
        fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
        Self { _temp: temp, root }
    }

    /// `client -r --exclude RULE src/ dest/`, locally or as a push over the
    /// remote shell with `client` on both ends.
    fn run(&self, client: &Path, push: bool) -> (Option<i32>, Vec<String>) {
        let dest = self.root.join("dst");
        if dest.exists() {
            fs::remove_dir_all(&dest).expect("clear destination");
        }
        fs::create_dir(&dest).expect("mkdir destination");
        let mut command = Command::new(client);
        command
            .arg("-r")
            .arg(format!("--exclude={RULE}"))
            .arg(format!("{}/", self.root.join("src").display()));
        if push {
            command
                .arg("-e")
                .arg(self.root.join("lsh"))
                .arg(format!("--rsync-path={}", client.display()))
                .arg(format!("localhost:{}/", dest.display()));
        } else {
            command.arg(format!("{}/", dest.display()));
        }
        let Deadlined::Finished { status, .. } =
            run_deadlined(&mut command, BUDGET).expect("spawn client")
        else {
            panic!("the transfer did not finish within {BUDGET:?}");
        };
        (status.code(), tree(&dest))
    }
}

/// Every path below `dir`, relative and sorted.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).expect("read dir") {
            let path = entry.expect("dir entry").path();
            out.push(
                path.strip_prefix(base)
                    .expect("below base")
                    .display()
                    .to_string(),
            );
            if path.is_dir() {
                walk(base, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// What rsync 3.5.2 transfers: `ab` is excluded, `a\b` is not.
fn expected() -> (Option<i32>, Vec<String>) {
    (
        Some(0),
        vec!["a\\b".to_owned(), "a\\b/x".to_owned(), "keep".to_owned()],
    )
}

fn assert_leg(push: bool) {
    let fx = Fixture::new();
    let oc = fx.run(&oc_rsync_bin(), push);
    assert_eq!(oc, expected(), "oc, push={push}");
    if let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) {
        let reference = fx.run(upstream.binary(), push);
        assert_eq!(reference, oc, "oc differs from rsync 3.5.2, push={push}");
    }
}

#[test]
fn a_local_copy_reads_a_wild3_stem_backslash_as_an_escape() {
    assert_leg(false);
}

#[test]
fn a_push_reads_a_wild3_stem_backslash_as_an_escape() {
    assert_leg(true);
}
