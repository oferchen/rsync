//! A local copy reads each entry's ACL and xattrs once, as it lists the entry,
//! and every later step applies those values.
//!
//! upstream: a local copy forks a sender and a receiver; flist.c:2050-2087
//! send_file_name() reads the ACL and xattrs into the file list and the
//! receiver applies what the list carried. Each cell compares the destination
//! oc writes with the one 3.5.1 writes, along one path the values travel: a
//! nested file, a directory's own attributes (the transfer root included), a
//! single-file operand, a `--delay-updates` commit, a link copied as its
//! referent (`-L`, `--copy-unsafe-links`) and the itemize `x` column.

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use filetime::FileTime;
use test_support::{UpstreamVersion, require_upstream_rsync};

fn oc_rsync() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// 3.5.1 first, when installed, so each cell is proven on the oracle before
/// oc is held to it.
fn implementations() -> Vec<(&'static str, PathBuf)> {
    let mut impls: Vec<_> = require_upstream_rsync(UpstreamVersion::V3_5_1)
        .map(|u| ("upstream", u.binary().to_path_buf()))
        .into_iter()
        .collect();
    impls.push(("oc", oc_rsync()));
    impls
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    /// `src/{a.txt,sub/b.log,link}` and `outside/target.txt`, where `link`
    /// points outside the tree. Mtimes are backdated so a rerun passes the
    /// quick check.
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("src/sub")).unwrap();
        fs::create_dir_all(root.join("outside")).unwrap();
        for (path, body) in [
            ("src/a.txt", "a\n"),
            ("src/sub/b.log", "b\n"),
            ("outside/target.txt", "t\n"),
        ] {
            fs::write(root.join(path), body).unwrap();
        }
        std::os::unix::fs::symlink("../outside/target.txt", root.join("src/link")).unwrap();
        let fx = Self { _temp: temp, root };
        fx.backdate();
        fx
    }

    fn backdate(&self) {
        for path in ["src/a.txt", "src/sub/b.log", "outside/target.txt"] {
            filetime::set_file_mtime(
                self.root.join(path),
                FileTime::from_unix_time(1_600_000_000, 0),
            )
            .unwrap();
        }
    }

    /// Runs `tool args path` against a fixture path; `false` when it fails.
    fn tool(&self, tool: &str, args: &[&str], path: &str) -> bool {
        Command::new(tool)
            .args(args)
            .arg(self.root.join(path))
            .status()
            .is_ok_and(|s| s.success())
    }

    fn run(&self, bin: &Path, args: &[&str]) -> Output {
        Command::new(bin)
            .args(args)
            .current_dir(&self.root)
            .output()
            .unwrap()
    }
}

/// `tool -R` output for `dir`, keyed by path; `keep` drops header lines.
fn snapshot(
    tool: &str,
    args: &[&str],
    dir: &Path,
    keep: impl Fn(&str) -> bool,
) -> BTreeMap<String, Vec<String>> {
    let out = Command::new(tool)
        .args(args)
        .arg(".")
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{tool} {}: {out:?}", dir.display());
    let mut map = BTreeMap::new();
    for block in String::from_utf8_lossy(&out.stdout).split("\n\n") {
        let mut lines = block.lines().filter(|l| !l.is_empty());
        let Some(file) = lines.next() else { continue };
        map.insert(
            file.to_owned(),
            lines.filter(|l| keep(l)).map(str::to_owned).collect(),
        );
    }
    map
}

/// Every xattr, the `user.rsync.*` store included, of every path under `dir`.
fn xattrs(dir: &Path) -> BTreeMap<String, Vec<String>> {
    snapshot(
        "getfattr",
        &["-R", "-d", "-m", "-", "-e", "hex"],
        dir,
        |_| true,
    )
}

/// The ACL of every path under `dir`, ids numeric, without the owner header a
/// non-root run cannot change.
fn acls(dir: &Path) -> BTreeMap<String, Vec<String>> {
    snapshot("getfacl", &["-R", "-p", "-n"], dir, |l| {
        !l.starts_with("# owner:") && !l.starts_with("# group:")
    })
}

fn has_capability(name: &str, tools: &[&str]) -> bool {
    if let Some(reason) = test_support::Capabilities::probe().skip_reason(&[name]) {
        eprintln!("skip: {reason}");
        return false;
    }
    tools
        .iter()
        .all(|tool| test_support::skip::require_command_on_path(tool))
}

/// One copy: its name, arguments, the destination directory to read, and a
/// line the result must hold, so a cell that copies nothing cannot pass.
/// `None` means the copy carries no attributes, so only a populated
/// destination is required.
type Cell<'a> = (&'a str, &'a [&'a str], &'a str, Option<&'a str>);

/// Runs each cell with every implementation on a fresh fixture and asserts oc
/// writes what 3.5.1 writes.
fn assert_cells_match(
    decorate: &dyn Fn(&Fixture) -> bool,
    cells: &[Cell<'_>],
    read: fn(&Path) -> BTreeMap<String, Vec<String>>,
) {
    for &(case, args, dest, expect) in cells {
        let mut results = Vec::new();
        for (name, bin) in implementations() {
            let fx = Fixture::new();
            if !decorate(&fx) {
                eprintln!("skip: the temp filesystem does not take the attributes");
                return;
            }
            let out = fx.run(&bin, args);
            assert!(out.status.success(), "{case} {name}: {out:?}");
            let dest = fx.root.join(dest);
            assert!(
                fs::read_dir(&dest).is_ok_and(|mut d| d.next().is_some()),
                "{case} {name}: nothing copied"
            );
            let got = read(&dest);
            if let Some(expect) = expect {
                assert!(
                    got.values().flatten().any(|line| line.contains(expect)),
                    "{case} {name}: no {expect} in {got:?}"
                );
            }
            results.push((name, got));
        }
        if let [(_, upstream), (_, oc)] = &results[..] {
            assert_eq!(oc, upstream, "{case}: oc against upstream");
        }
    }
}

#[test]
fn local_xattrs_land_like_upstream() {
    if !has_capability(
        test_support::capabilities::names::XATTRS,
        &["setfattr", "getfattr"],
    ) {
        return;
    }
    let long = "v".repeat(64);
    let decorate = |fx: &Fixture| {
        [
            ("user.root", "r", "src"),
            ("user.short", "blue", "src/a.txt"),
            ("user.long", long.as_str(), "src/sub/b.log"),
            ("user.dir", "d", "src/sub"),
            ("user.long", long.as_str(), "src/sub"),
            ("user.t", "target", "outside/target.txt"),
        ]
        .into_iter()
        .all(|(name, value, path)| fx.tool("setfattr", &["-n", name, "-v", value], path))
    };
    let user = Some("user.");
    // A link copied as its referent carries the link's own xattrs, none:
    // upstream's sender reads through sys_llistxattr/sys_lgetxattr
    // (xattrs.c:162,200), never following the path.
    assert_cells_match(
        &decorate,
        &[
            ("tree", &["-rX", "src/", "dst/"], "dst", user),
            (
                "delay-updates",
                &["-rX", "--delay-updates", "src/", "dst/"],
                "dst",
                user,
            ),
            ("operand", &["-X", "src/a.txt", "one/"], "one", user),
            ("directory operand", &["-rX", "src", "two/"], "two", user),
            ("referent under -L", &["-rLX", "src/", "dst/"], "dst", user),
            (
                "unsafe referent operand",
                &["-lX", "--copy-unsafe-links", "src/link", "three/"],
                "three",
                None,
            ),
        ],
        xattrs,
    );
}

/// upstream: generator.c:571-582 itemize() lights the `x` column when
/// xattr_diff() finds the sender's list and the destination's differ, on an
/// up-to-date file too.
#[test]
fn local_itemize_reports_xattr_drift_like_upstream() {
    if !has_capability(
        test_support::capabilities::names::XATTRS,
        &["setfattr", "getfattr"],
    ) {
        return;
    }
    let mut rows = Vec::new();
    for (name, bin) in implementations() {
        let fx = Fixture::new();
        if !fx.tool("setfattr", &["-n", "user.x", "-v", "1"], "src/a.txt") {
            eprintln!("skip: the temp filesystem does not take user xattrs");
            return;
        }
        let out = fx.run(&bin, &["-rX", "src/", "dst/"]);
        assert!(out.status.success(), "{name} copy: {out:?}");
        assert!(fx.tool("setfattr", &["-n", "user.x", "-v", "2"], "src/a.txt"));
        fx.backdate();
        let out = fx.run(&bin, &["-rXi", "src/", "dst/"]);
        assert!(out.status.success(), "{name} rerun: {out:?}");
        let row: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|line| line.ends_with(" a.txt"))
            .map(str::to_owned)
            .collect();
        assert!(
            row.iter().any(|line| line.contains("x a.txt")),
            "{name}: {row:?}"
        );
        rows.push(row);
    }
    if let [upstream, oc] = &rows[..] {
        assert_eq!(oc, upstream, "itemize rows");
    }
}

/// upstream: xattrs.c:1074-1094 rsync_xal_set() removes a destination xattr
/// the sender's list does not carry, on a directory updated in place.
#[test]
fn local_xattr_prune_like_upstream() {
    if !has_capability(
        test_support::capabilities::names::XATTRS,
        &["setfattr", "getfattr"],
    ) {
        return;
    }
    let mut results = Vec::new();
    for (name, bin) in implementations() {
        let fx = Fixture::new();
        if !fx.tool("setfattr", &["-n", "user.keep", "-v", "1"], "src/sub") {
            eprintln!("skip: the temp filesystem does not take user xattrs");
            return;
        }
        let out = fx.run(&bin, &["-rX", "src/", "dst/"]);
        assert!(out.status.success(), "{name} copy: {out:?}");
        assert!(fx.tool("setfattr", &["-n", "user.extra", "-v", "x"], "dst/sub"));
        let out = fx.run(&bin, &["-rX", "src/", "dst/"]);
        assert!(out.status.success(), "{name} rerun: {out:?}");
        let got = xattrs(&fx.root.join("dst"));
        let lines: Vec<&String> = got.values().flatten().collect();
        assert!(
            lines.iter().any(|l| l.contains("user.keep")),
            "{name}: {got:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("user.extra")),
            "{name}: the extraneous xattr survives: {got:?}"
        );
        results.push(got);
    }
    if let [upstream, oc] = &results[..] {
        assert_eq!(oc, upstream, "pruned destination");
    }
}

#[test]
fn local_acls_land_like_upstream() {
    if !has_capability(
        test_support::capabilities::names::ACLS,
        &["setfacl", "getfacl"],
    ) {
        return;
    }
    let decorate = |fx: &Fixture| {
        [
            (&["-m", "u:65534:r-x"][..], "src"),
            (&["-m", "u:65534:r-x,g:65534:rw-"], "src/a.txt"),
            (&["-m", "g:65534:r--"], "src/sub/b.log"),
            (&["-d", "-m", "u:65534:r-x"], "src/sub"),
            (&["-m", "u:65534:r--"], "outside/target.txt"),
        ]
        .into_iter()
        .all(|(args, path)| fx.tool("setfacl", args, path))
    };
    let named = Some("user:65534");
    // A link copied as its referent carries the referent's ACL: upstream's
    // get_acl() reads through sys_acl_get_file(), which follows the path.
    assert_cells_match(
        &decorate,
        &[
            ("tree", &["-rA", "src/", "dst/"], "dst", named),
            (
                "delay-updates",
                &["-rA", "--delay-updates", "src/", "dst/"],
                "dst",
                named,
            ),
            ("operand", &["-A", "src/a.txt", "one/"], "one", named),
            ("directory operand", &["-rA", "src", "two/"], "two", named),
            ("referent under -L", &["-rLA", "src/", "dst/"], "dst", named),
            (
                "unsafe referent operand",
                &["-lA", "--copy-unsafe-links", "src/link", "three/"],
                "three",
                named,
            ),
        ],
        acls,
    );
}
