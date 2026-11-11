//! Local-copy residuals pinned against 3.5.1: what `--fake-super -A` stashes,
//! how `--copy-unsafe-links` treats a link when `-l` is off, and which rows a
//! skipped non-regular entry prints.
//!
//! upstream: `--fake-super` sets `am_root = -1` for the whole process
//! (options.c:672), so in a local copy the sender reads only the
//! `%aacl`/`%dacl` stash (acls.c:599-607) and the receiver stores one only when
//! it differs from what the destination holds (acls.c:1388-1415).
//! flist.c:482-506 readlink_stat() dereferences an unsafe link before `-l`
//! matters, and generator.c:2109-2115 skips a remaining non-regular entry with
//! a notice and no itemized row.

#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use test_support::{UpstreamVersion, require_upstream_rsync};

fn oc_rsync() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// 3.5.1 and oc, or `None` when the oracle is not installed.
fn oracle_and_oc() -> Option<[(&'static str, PathBuf); 2]> {
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_1)?;
    Some([
        ("upstream", upstream.binary().to_path_buf()),
        ("oc", oc_rsync()),
    ])
}

/// Whether `tool` runs on this host; a cell that needs it skips otherwise.
fn has_tool(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// `src/{plain,sub/inner,ln -> ../out/t,sub/deep -> ../../out/t,safe -> plain,
/// fifo}` and `out/t`. `ln` and `sub/deep` escape the tree, so
/// `--copy-unsafe-links` copies them; `safe` stays a link and is skipped
/// without `-l`.
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("src/sub")).unwrap();
        fs::create_dir_all(root.join("out")).unwrap();
        fs::write(root.join("src/plain"), "p\n").unwrap();
        fs::write(root.join("src/sub/inner"), "i\n").unwrap();
        fs::write(root.join("out/t"), "t\n").unwrap();
        std::os::unix::fs::symlink("../out/t", root.join("src/ln")).unwrap();
        std::os::unix::fs::symlink("../../out/t", root.join("src/sub/deep")).unwrap();
        std::os::unix::fs::symlink("plain", root.join("src/safe")).unwrap();
        let fifo = Command::new("mkfifo")
            .arg(root.join("src/fifo"))
            .status()
            .unwrap();
        assert!(fifo.success());
        Self { _temp: temp, root }
    }

    fn run(&self, bin: &Path, cwd: &str, args: &[&str]) -> Output {
        Command::new(bin)
            .args(args)
            .current_dir(self.root.join(cwd))
            .output()
            .unwrap()
    }

    fn dest(&self, label: &str) -> PathBuf {
        self.root.join(format!("dst_{label}"))
    }
}

fn stdout_lines(out: &Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.starts_with("created directory"))
        .map(str::to_owned)
        .collect()
}

/// `(relative path, kind)` for every entry under `dir`, kind being `f`, `d`,
/// `l` or `p`, so a link copied as its referent shows up as `f`.
fn tree(dir: &Path) -> BTreeSet<(String, char)> {
    let mut out = BTreeSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current).unwrap() {
            let entry = entry.unwrap();
            let ft = entry.file_type().unwrap();
            let kind = if ft.is_dir() {
                stack.push(entry.path());
                'd'
            } else if ft.is_symlink() {
                'l'
            } else if ft.is_file() {
                'f'
            } else {
                'p'
            };
            let rel = entry
                .path()
                .strip_prefix(dir)
                .unwrap()
                .display()
                .to_string();
            out.insert((rel, kind));
        }
    }
    out
}

/// The `user.rsync.%aacl`/`%dacl` stash names on every path under `dir`.
fn acl_stashes(dir: &Path) -> BTreeSet<String> {
    let out = Command::new("getfattr")
        .args(["-R", "-d", "-m", r"^user\.rsync\.%[ad]acl$", "."])
        .current_dir(dir)
        .output()
        .unwrap();
    let mut stashes = BTreeSet::new();
    let mut file = String::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(name) = line.strip_prefix("# file: ") {
            file = name.to_owned();
        } else if let Some((attr, _)) = line.split_once('=') {
            stashes.insert(format!("{file} {attr}"));
        }
    }
    stashes
}

/// A `%aacl` stash that grants `nobody` read, as a fake-super receive of a
/// named-user ACL writes it.
const NAMED_USER_AACL: &str = "0x80000000800000000400000080000000feff000004000080";

/// The stashes a local `--fake-super -A` copy writes: none for a source that
/// carries only its permission bits, and the source's own stash where it has
/// one. The bait file proves the comparison can see a stash at all.
#[test]
fn local_fake_super_stashes_only_what_the_source_stash_carries() {
    let Some(impls) = oracle_and_oc() else { return };
    if !has_tool("getfattr") || !has_tool("setfattr") {
        eprintln!("skipping: getfattr/setfattr not installed");
        return;
    }
    let fx = Fixture::new();
    let bait = fx.root.join("src/sub/inner");
    let set = Command::new("setfattr")
        .args(["-n", "user.rsync.%aacl", "-v", NAMED_USER_AACL])
        .arg(&bait)
        .status()
        .unwrap();
    if !set.success() {
        eprintln!("skipping: the filesystem refuses user xattrs");
        return;
    }

    let mut seen = Vec::new();
    for (label, bin) in &impls {
        let dest = fx.dest(label);
        let out = fx.run(
            bin,
            ".",
            &["-a", "--fake-super", "-A", "src/", dest.to_str().unwrap()],
        );
        assert!(out.status.success(), "{label}: {out:?}");
        seen.push((label, acl_stashes(&dest)));
    }
    let (_, upstream) = &seen[0];
    assert_eq!(
        upstream,
        &BTreeSet::from(["sub/inner user.rsync.%aacl".to_owned()]),
        "oracle: only the stashed source carries an ACL stash",
    );
    assert_eq!(seen[1].1, *upstream, "oc stashes differ from 3.5.1");
}

/// `-M--fake-super` on a local copy reaches the receiver half only, so the
/// sender reads the source's real named-user ACL and the receiver stashes it.
///
/// upstream: pipe.c:143-148 local_child() appends the `-M` values to the forked
/// receiver's argv; acls.c:609-631 then reads the real ACL on the sender and
/// acls.c:933-971 stores `%aacl` on the receiver. The 3.5.1 testsuite's
/// fake-super-acl-xattr leg 1 is this transfer.
#[test]
fn receiver_only_fake_super_stashes_the_real_source_acl() {
    let Some(impls) = oracle_and_oc() else { return };
    if !has_tool("getfattr") || !has_tool("setfacl") {
        eprintln!("skipping: getfattr/setfacl not installed");
        return;
    }
    let fx = Fixture::new();
    let named = fx.root.join("src/named");
    std::fs::write(&named, "named\n").unwrap();
    let uid = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(&named).unwrap());
    let set = Command::new("setfacl")
        .args(["-m", &format!("u:{uid}:rwx")])
        .arg(&named)
        .status()
        .unwrap();
    if !set.success() {
        eprintln!("skipping: the filesystem refuses ACLs");
        return;
    }

    let mut seen = Vec::new();
    for (label, bin) in &impls {
        let dest = fx.dest(label);
        let out = fx.run(
            bin,
            ".",
            &["-rA", "-M--fake-super", "src/", dest.to_str().unwrap()],
        );
        assert!(out.status.success(), "{label}: {out:?}");
        seen.push((label, acl_stashes(&dest)));
    }
    let (_, upstream) = &seen[0];
    assert!(
        upstream.contains("named user.rsync.%aacl"),
        "oracle: the real ACL is stashed: {upstream:?}",
    );
    assert_eq!(seen[1].1, *upstream, "oc stashes differ from 3.5.1");
}

/// `-M--fake-super` leaves the sender reading the source's real stat, so a
/// `%stat` record already on the source does not stand in for it.
///
/// upstream: pipe.c:143-148 local_child() gives `--fake-super` to the receiver
/// alone; the sender's flist.c make_file() reads plain lstat() rather than
/// xattrs.c:1372 x_lstat(). MEASURED (3.5.1): a 0644 source recorded as
/// `100600 0,0 0:0` lands as 0644 with `%stat` `100644 0,0 0:0`; with
/// `--fake-super` on both halves it lands as 0600.
#[test]
fn receiver_only_fake_super_ignores_the_source_stat_record() {
    if !has_tool("getfattr") || !has_tool("setfattr") {
        eprintln!("skipping: getfattr/setfattr not installed");
        return;
    }
    let fx = Fixture::new();
    let plain = fx.root.join("src/plain");
    fs::set_permissions(&plain, fs::Permissions::from_mode(0o644)).unwrap();
    let set = Command::new("setfattr")
        .args(["-n", "user.rsync.%stat", "-v", "100600 0,0 0:0"])
        .arg(&plain)
        .status()
        .unwrap();
    if !set.success() {
        eprintln!("skipping: the filesystem refuses user xattrs");
        return;
    }
    let stat_of = |dest: &Path| {
        let out = Command::new("getfattr")
            .args(["--only-values", "-n", "user.rsync.%stat"])
            .arg(dest.join("plain"))
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let mode_of = |dest: &Path| {
        fs::metadata(dest.join("plain"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777
    };

    for (flag, record, mode) in [
        ("-M--fake-super", "100644 0,0 0:0", 0o644),
        ("--fake-super", "100600 0,0 0:0", 0o600),
    ] {
        let dest = fx.dest(flag);
        let out = fx.run(
            &oc_rsync(),
            ".",
            &["-r", flag, "src/", dest.to_str().unwrap()],
        );
        assert!(out.status.success(), "{flag}: {out:?}");
        assert_eq!(stat_of(&dest), record, "{flag}");
        assert_eq!(mode_of(&dest), mode, "{flag}");
    }
}

/// A `--fake-super -A` push sends only the stash, so a real named-user ACL
/// on the source does not reach a 3.5.1 receiver.
#[test]
fn fake_super_push_sends_the_stash_not_the_real_acl() {
    let Some(impls) = oracle_and_oc() else { return };
    if !has_tool("setfacl") || !has_tool("getfacl") {
        eprintln!("skipping: setfacl/getfacl not installed");
        return;
    }
    let fx = Fixture::new();
    let named = fx.root.join("src/plain");
    let set = Command::new("setfacl")
        .args(["-m", "u:nobody:r"])
        .arg(&named)
        .status()
        .unwrap();
    if !set.success() {
        eprintln!("skipping: the filesystem refuses POSIX ACLs");
        return;
    }
    // A remote shell that runs the server locally, like support/lsh.sh.
    let rsh = fx.root.join("rsh");
    fs::write(
        &rsh,
        "#!/bin/sh\nwhile [ \"$1\" != localhost ]; do shift; done\nshift\nexec \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).unwrap();
    let server = &impls[0].1;

    let mut acls = Vec::new();
    for (label, bin) in &impls {
        let dest = fx.dest(label);
        let out = fx.run(
            bin,
            ".",
            &[
                "-a",
                "-A",
                "--fake-super",
                "-e",
                rsh.to_str().unwrap(),
                &format!("--rsync-path={}", server.display()),
                "src/",
                &format!("localhost:{}/", dest.display()),
            ],
        );
        assert!(out.status.success(), "{label}: {out:?}");
        let acl = Command::new("getfacl")
            .args(["-p", "--omit-header"])
            .arg(dest.join("plain"))
            .output()
            .unwrap();
        acls.push(String::from_utf8_lossy(&acl.stdout).into_owned());
    }
    assert!(
        !acls[0].contains("user:nobody"),
        "oracle: the real ACL stays behind: {}",
        acls[0]
    );
    assert_eq!(
        acls[1], acls[0],
        "oc client sends a different ACL than 3.5.1"
    );
}

/// An unsafe operand link is copied as its referent under
/// `--copy-unsafe-links` even without `-l`.
#[test]
fn unsafe_operand_link_is_copied_without_links() {
    let Some(impls) = oracle_and_oc() else { return };
    let fx = Fixture::new();
    let mut seen = Vec::new();
    for (label, bin) in &impls {
        let dest = fx.dest(label);
        let out = fx.run(
            bin,
            "src",
            &[
                "-r",
                "-i",
                "--copy-unsafe-links",
                "ln",
                &format!("{}/", dest.display()),
            ],
        );
        assert!(out.status.success(), "{label}: {out:?}");
        seen.push((stdout_lines(&out), tree(&dest)));
    }
    assert_eq!(
        seen[0].1,
        BTreeSet::from([("ln".to_owned(), 'f')]),
        "oracle: the referent lands as a file",
    );
    assert_eq!(seen[1], seen[0], "oc differs from 3.5.1");
}

/// Inside a tree, `--copy-unsafe-links` without `-l` copies an escaping link
/// as its referent and still skips the FIFO. The rows are compared as a set:
/// 3.5.1 prints a directory's skip notices before its `cd` row.
#[test]
fn unsafe_link_in_tree_is_copied_without_links() {
    let Some(impls) = oracle_and_oc() else { return };
    let fx = Fixture::new();
    let mut seen = Vec::new();
    for (label, bin) in &impls {
        let dest = fx.dest(label);
        let out = fx.run(
            bin,
            ".",
            &[
                "-r",
                "-i",
                "--copy-unsafe-links",
                "src/",
                dest.to_str().unwrap(),
            ],
        );
        assert!(out.status.success(), "{label}: {out:?}");
        let rows: BTreeSet<_> = stdout_lines(&out).into_iter().collect();
        seen.push((rows, tree(&dest)));
    }
    assert!(
        seen[0].1.contains(&("sub/deep".to_owned(), 'f'))
            && !seen[0].1.iter().any(|(path, _)| path == "safe"),
        "oracle: the escaping link lands as a file, the safe one is skipped: {:?}",
        seen[0].1
    );
    assert_eq!(seen[1], seen[0], "oc differs from 3.5.1");
}

/// A link and a FIFO skipped without `-l`/`-D` print only the notice, never
/// an itemized row.
#[test]
fn skipped_non_regular_entries_print_no_itemize_row() {
    let Some(impls) = oracle_and_oc() else { return };
    let fx = Fixture::new();
    let mut seen = Vec::new();
    for (label, bin) in &impls {
        let dest = fx.dest(label);
        let tree_out = fx.run(bin, ".", &["-r", "-i", "src/", dest.to_str().unwrap()]);
        assert!(tree_out.status.success(), "{label}: {tree_out:?}");
        let operand_dest = fx.dest(&format!("{label}_op"));
        let operand_out = fx.run(
            bin,
            "src",
            &["-r", "-i", "ln", operand_dest.to_str().unwrap()],
        );
        assert!(operand_out.status.success(), "{label}: {operand_out:?}");
        let rows: BTreeSet<_> = stdout_lines(&tree_out).into_iter().collect();
        seen.push((rows, stdout_lines(&operand_out)));
    }
    assert!(
        seen[0].0.contains("skipping non-regular file \"ln\""),
        "oracle prints the notice: {:?}",
        seen[0].0
    );
    assert_eq!(seen[1], seen[0], "oc differs from 3.5.1");
}
