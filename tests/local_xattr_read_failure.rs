//! A local copy whose xattr read fails leaves the entry out of the file list
//! before the directory's delete pass.
//!
//! upstream: a local copy still forks a sender, and flist.c:2058-2063
//! send_file_name() drops an entry whose get_xattr() fails, after printing
//! `get_xattr_data: lgetxattr(<full_fname>,"<name>",0) failed: ...`, with
//! `io_error |= IOERR_GENERAL`. The list is built before the generator
//! sweeps that directory, so delete_in_dir() sees the error and skips the
//! deletion (generator.c:304-307). The rest of the directory still
//! transfers and the run ends with code 23.
//!
//! A copy that reads the xattrs only when it applies them notices the
//! failure after the sweep (or never, when nothing is applied), so it
//! deletes what upstream keeps and aborts instead of continuing.
//!
//! The fixture is a user xattr on a mode-000 file: listing names needs no
//! permission, reading a `user.*` value does, so only the value read fails.

#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use filetime::FileTime;
use test_support::{UpstreamVersion, require_upstream_rsync};

fn oc_rsync() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    /// `src/a.txt`, then `src/sub/{b.txt,locked,z.txt}` with `locked` holding
    /// an unreadable `user.x`. All mtimes are backdated so a copied
    /// destination passes the quick check. `None` when the filesystem takes
    /// no user xattrs.
    fn new() -> Option<Self> {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let sub = root.join("src/sub");
        fs::create_dir_all(&sub).unwrap();
        for (path, body) in [
            (root.join("src/a.txt"), "a\n"),
            (sub.join("b.txt"), "b\n"),
            (sub.join("locked"), "locked\n"),
            (sub.join("z.txt"), "z\n"),
        ] {
            fs::write(&path, body).unwrap();
            filetime::set_file_mtime(&path, FileTime::from_unix_time(1_600_000_000, 0)).unwrap();
        }
        let tagged = Command::new("setfattr")
            .args(["-n", "user.x", "-v", "1"])
            .arg(sub.join("locked"))
            .status()
            .is_ok_and(|s| s.success());
        tagged.then_some(Self { _temp: temp, root })
    }

    fn locked(&self) -> PathBuf {
        self.root.join("src/sub/locked")
    }

    /// Copies the source to `dst` as an up-to-date destination, minus the
    /// locked file's xattr, plus an extraneous `dst/sub/gone`.
    fn seed_destination(&self) {
        let status = Command::new("cp")
            .args(["-a", "src", "dst"])
            .current_dir(&self.root)
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("setfattr")
            .args(["-x", "user.x"])
            .arg(self.root.join("dst/sub/locked"))
            .status()
            .unwrap();
        assert!(status.success());
        fs::write(self.root.join("dst/sub/gone"), "gone\n").unwrap();
    }

    fn run(&self, bin: &Path, args: &[&str]) -> Output {
        fs::set_permissions(self.locked(), fs::Permissions::from_mode(0o000)).unwrap();
        let out = Command::new(bin)
            .args(args)
            .args(["src/", "dst/"])
            .current_dir(&self.root)
            .output()
            .unwrap();
        fs::set_permissions(self.locked(), fs::Permissions::from_mode(0o600)).unwrap();
        out
    }
}

/// 3.5.1 first, when installed, so the expectations are proven on the oracle
/// before oc is held to them.
fn implementations() -> Vec<(&'static str, PathBuf)> {
    let mut impls: Vec<_> = require_upstream_rsync(UpstreamVersion::V3_5_1)
        .map(|u| ("upstream", u.binary().to_path_buf()))
        .into_iter()
        .collect();
    impls.push(("oc", oc_rsync()));
    impls
}

fn skip_reason() -> Option<String> {
    if let Some(reason) = test_support::Capabilities::probe()
        .skip_reason(&[test_support::capabilities::names::XATTRS])
    {
        return Some(reason.to_string());
    }
    if !test_support::skip::require_command_on_path("setfattr") {
        return Some("setfattr is not on PATH".into());
    }
    if fs::metadata("/proc/self").is_ok_and(|meta| std::os::unix::fs::MetadataExt::uid(&meta) == 0)
    {
        return Some("root reads a mode-000 file's xattrs".into());
    }
    None
}

/// The common verdict: code 23, upstream's read-failure line naming the full
/// path, and no later failure from the dropped entry.
fn assert_dropped_at_list_time(case: &str, fx: &Fixture, out: &Output) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(23), "{case}: {out:?}");
    assert!(
        stderr.contains(&format!(
            "rsync: [sender] get_xattr_data: lgetxattr(\"{}\",\"user.x\",0) failed: Permission denied (13)",
            fx.locked().display()
        )),
        "{case}: names the failed read: {stderr}"
    );
    assert!(
        !stderr.contains("failed to open") && !stderr.contains("failed to read extended attribute"),
        "{case}: the dropped entry fails nothing later: {stderr}"
    );
}

#[test]
fn local_copy_drops_a_file_whose_xattrs_cannot_be_read() {
    if let Some(reason) = skip_reason() {
        eprintln!("skip: {reason}");
        return;
    }
    for (name, bin) in implementations() {
        let Some(fx) = Fixture::new() else {
            eprintln!("skip: the temp filesystem does not take user xattrs");
            return;
        };
        let out = fx.run(&bin, &["-rX"]);
        assert_dropped_at_list_time(name, &fx, &out);
        for kept in ["a.txt", "sub/b.txt", "sub/z.txt"] {
            assert!(
                fx.root.join("dst").join(kept).exists(),
                "{name}: {kept} arrives"
            );
        }
        assert!(
            !fx.root.join("dst/sub/locked").exists(),
            "{name}: the dropped file does not"
        );
    }
}

/// A dry run never opens a file, so only a list-time read sees the failure,
/// and the entries after it are still itemized.
#[test]
fn local_dry_run_reports_an_unreadable_xattr_and_lists_the_rest() {
    if let Some(reason) = skip_reason() {
        eprintln!("skip: {reason}");
        return;
    }
    for (name, bin) in implementations() {
        let Some(fx) = Fixture::new() else {
            eprintln!("skip: the temp filesystem does not take user xattrs");
            return;
        };
        let out = fx.run(&bin, &["-rXni"]);
        assert_dropped_at_list_time(name, &fx, &out);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains(">f+++++++++ sub/z.txt"), "{name}: {stdout}");
        assert!(!stdout.contains("sub/locked"), "{name}: {stdout}");
    }
}

/// An up-to-date destination transfers no data, and the failed read must
/// still suppress `--delete` in that directory, recording a batch or not.
#[test]
fn local_delete_is_skipped_after_an_unreadable_xattr() {
    if let Some(reason) = skip_reason() {
        eprintln!("skip: {reason}");
        return;
    }
    for batch in [false, true] {
        for (name, bin) in implementations() {
            let Some(fx) = Fixture::new() else {
                eprintln!("skip: the temp filesystem does not take user xattrs");
                return;
            };
            fx.seed_destination();
            let mut args = vec!["-rX", "--delete"];
            if batch {
                args.push("--write-batch=bf");
            }
            let case = format!("{name} {args:?}");
            let out = fx.run(&bin, &args);
            assert_dropped_at_list_time(&case, &fx, &out);
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                stdout.contains("IO error encountered -- skipping file deletion"),
                "{case}: {stdout}"
            );
            assert!(
                fx.root.join("dst/sub/gone").exists(),
                "{case}: the extraneous file survives"
            );
        }
    }
}
