//! A pull whose `--files-from` list names a directory ending in a backslash
//! accepts the names below it.
//!
//! upstream: exclude.c:476-670 `add_implied_include()`. For `-r` it adds the
//! entry's `/**` rule; a non-wild entry's backslashes are doubled first
//! (exclude.c:640-647) so wildmatch keeps them literal. `a\` therefore becomes
//! `/a\\/**`, which admits `a\/x`. The trailing backslash is the case
//! CVE-2026-70461 mis-counted (NEWS.md, 3.5.2): left single, the rule would be
//! `/a\/**`, which wildmatch reads as `/a/**`, so the receiver would refuse
//! `a\/x` as an unrequested file-list name (flist.c:1368).
//!
//! Every cell asserts the exit code and tree measured with rsync 3.5.2. When
//! the 3.5.2 oracle resolves (`OC_RSYNC_UPSTREAM_BIN_3_5_2`), it runs the same
//! pull and must match.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[cfg(feature = "daemon")]
use test_support::oc_daemon::OcDaemon;
use test_support::{
    Deadlined, UpstreamVersion, oc_rsync_bin, require_upstream_rsync, run_deadlined,
};

const BUDGET: Duration = Duration::from_secs(120);
/// The directory name: one byte, then a trailing backslash.
const DIR: &str = "a\\";

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    /// `src/a\/x`, a list naming `a\`, and a remote shell that drops the host.
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical tempdir");
        let src = root.join("src");
        fs::create_dir_all(src.join(DIR)).expect("mkdir src/a\\");
        fs::write(src.join(DIR).join("x"), "x\n").expect("write x");
        fs::write(root.join("list"), format!("{DIR}\n")).expect("write list");
        let rsh = root.join("lsh");
        fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh");
        fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
        Self { _temp: temp, root }
    }

    /// A fresh, empty destination.
    fn dest(&self) -> PathBuf {
        let dest = self.root.join("dst");
        if dest.exists() {
            fs::remove_dir_all(&dest).expect("clear destination");
        }
        fs::create_dir(&dest).expect("mkdir destination");
        dest
    }

    /// `client -r --files-from=list <source> dst/`.
    fn pull(&self, client: &Path, source: &str, extra: &[String]) -> (Option<i32>, Vec<String>) {
        let dest = self.dest();
        let mut command = Command::new(client);
        command
            .arg("-r")
            .arg(format!("--files-from={}", self.root.join("list").display()))
            .args(extra)
            .arg(source)
            .arg(format!("{}/", dest.display()));
        let Deadlined::Finished { status, stderr, .. } =
            run_deadlined(&mut command, BUDGET).expect("spawn client")
        else {
            panic!("the pull did not finish within {BUDGET:?}");
        };
        assert!(
            status.code() == Some(0),
            "pull failed: {}",
            String::from_utf8_lossy(&stderr)
        );
        (status.code(), tree(&dest))
    }

    /// The remote-shell spelling of `src/`, with `client` on both ends.
    fn rsh_args(&self, client: &Path) -> (String, Vec<String>) {
        (
            format!("localhost:{}/", self.root.join("src").display()),
            vec![
                "-e".to_owned(),
                self.root.join("lsh").display().to_string(),
                format!("--rsync-path={}", client.display()),
            ],
        )
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

/// What rsync 3.5.2 does with this list: exit 0, the directory and its file.
fn expected() -> Vec<String> {
    vec![DIR.to_owned(), format!("{DIR}/x")]
}

/// Remote shell: the receiver accepts `a\/x` under the `/a\\/**` rule.
#[test]
fn a_pull_over_a_remote_shell_accepts_names_below_a_trailing_backslash_entry() {
    let fx = Fixture::new();
    let oc = oc_rsync_bin();
    let (source, extra) = fx.rsh_args(&oc);
    let (code, tree) = fx.pull(&oc, &source, &extra);
    assert_eq!((code, tree.clone()), (Some(0), expected()), "oc");
    if let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) {
        let (source, extra) = fx.rsh_args(upstream.binary());
        let reference = fx.pull(upstream.binary(), &source, &extra);
        assert_eq!(reference, (code, tree), "oc differs from rsync 3.5.2");
    }
}

/// Daemon: the same list pulled from an oc daemon module.
#[cfg(feature = "daemon")]
#[test]
fn a_pull_from_a_daemon_accepts_names_below_a_trailing_backslash_entry() {
    let fx = Fixture::new();
    let config = fx.root.join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "use chroot = false\n\n[data]\npath = {}\nread only = true\n",
            fx.root.join("src").display()
        ),
    )
    .expect("write rsyncd.conf");
    let daemon = OcDaemon::builder(&config).spawn().expect("spawn daemon");
    let source = format!("rsync://127.0.0.1:{}/data/", daemon.port());
    let (code, tree) = fx.pull(&oc_rsync_bin(), &source, &[]);
    assert_eq!((code, tree.clone()), (Some(0), expected()), "oc client");
    if let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) {
        let reference = fx.pull(upstream.binary(), &source, &[]);
        assert_eq!(reference, (code, tree), "a 3.5.2 client differs from oc");
    }
}
