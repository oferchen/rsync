//! A local copy without `--perms` reads the destination's default ACL by the
//! name rsync 3.5.2 reads it, so a new destination prints nothing.
//!
//! upstream: acls.c:1453-1488 `default_perms_for_dir()` turns a NULL dir into
//! "." and warns only when the read itself fails; both callers pass
//! `file->dirname ? file->dirname : "."` (generator.c:1729, receiver.c:1201)
//! and run only when `--perms` is off (generator.c:1776-1777,
//! receiver.c:1198-1204). oc once asked for the empty parent of a relative
//! `dst`, so the read failed with ENOENT and printed
//! `default_perms_for_dir: sys_acl_get_file(, ACL_TYPE_DEFAULT): ...`.
//!
//! Each cell runs rsync 3.5.2 and oc-rsync on the same fixture - relative and
//! absolute new destinations, with and without `-p` - and compares exit code,
//! stderr and the modes of the resulting tree. On Linux one more cell gives the
//! destination's parent a default ACL, so the tree's modes show whether the
//! ACL that was read was folded into `dest_mode()` (rsync.c:483-485).

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use test_support::{UpstreamVersion, create_tempdir, oc_rsync_bin, require_upstream_rsync};

/// How the destination operand is spelled.
#[derive(Clone, Copy, Debug)]
enum Spelling {
    /// `dst/`, resolved from the fixture as the working directory.
    Relative,
    /// The absolute path of `dst/`.
    Absolute,
}

const CELLS: &[(Spelling, &[&str])] = &[
    (Spelling::Relative, &[]),
    (Spelling::Relative, &["-p"]),
    (Spelling::Absolute, &[]),
    (Spelling::Absolute, &["-p"]),
];

/// What one run left behind.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    exit: Option<i32>,
    stderr: String,
    /// Every destination entry below `dst`, with its permission bits.
    modes: Vec<(PathBuf, u32)>,
}

fn build_source(root: &Path) {
    let src = root.join("src");
    fs::create_dir_all(src.join("d")).expect("source tree");
    fs::write(src.join("f"), b"x\n").expect("src/f");
    fs::write(src.join("d/g"), b"y\n").expect("src/d/g");
    fs::set_permissions(src.join("d/g"), fs::Permissions::from_mode(0o755)).expect("chmod g");
}

fn collect_modes(dir: &Path, base: &Path, out: &mut Vec<(PathBuf, u32)>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .expect("read destination")
        .map(|entry| entry.expect("entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let meta = fs::symlink_metadata(&path).expect("stat destination entry");
        let relative = path.strip_prefix(base).expect("below dst").to_path_buf();
        out.push((relative, meta.permissions().mode() & 0o7777));
        if meta.is_dir() {
            collect_modes(&path, base, out);
        }
    }
}

fn run(binary: &Path, spelling: Spelling, flags: &[&str]) -> Outcome {
    run_in(binary, spelling, flags, |_| {})
}

/// Runs one copy in a fresh fixture after `prepare` has seen its root, the
/// directory that will hold the new `dst`.
fn run_in(
    binary: &Path,
    spelling: Spelling,
    flags: &[&str],
    prepare: impl FnOnce(&Path),
) -> Outcome {
    let fixture = create_tempdir();
    let root = fixture.path();
    build_source(root);
    prepare(root);
    let dest = match spelling {
        Spelling::Relative => "dst/".to_owned(),
        Spelling::Absolute => format!("{}/dst/", root.display()),
    };
    let output = Command::new(binary)
        .current_dir(root)
        .arg("-r")
        .args(flags)
        .arg("src/")
        .arg(&dest)
        .output()
        .expect("spawn rsync");
    let dst = root.join("dst");
    let mut modes = vec![(
        PathBuf::from("."),
        fs::metadata(&dst)
            .expect("dst created")
            .permissions()
            .mode()
            & 0o7777,
    )];
    collect_modes(&dst, &dst, &mut modes);
    Outcome {
        exit: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        modes,
    }
}

#[test]
fn a_new_local_destination_reads_its_default_acl_like_rsync_3_5_2() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let oc = oc_rsync_bin();
    let mut differing = Vec::new();
    for &(spelling, flags) in CELLS {
        let expected = run(upstream.binary(), spelling, flags);
        // The oracle itself must succeed silently, or the cell cannot tell a
        // stray warning from an expected one.
        assert_eq!(
            (expected.exit, expected.stderr.as_str()),
            (Some(0), ""),
            "rsync 3.5.2 {spelling:?} {flags:?} did not copy silently"
        );
        let actual = run(&oc, spelling, flags);
        if actual != expected {
            differing.push(format!(
                "{spelling:?} {flags:?}:\n  rsync 3.5.2: {expected:?}\n  oc-rsync:    {actual:?}"
            ));
        }
    }
    assert!(
        differing.is_empty(),
        "{} of {} cells differ from rsync 3.5.2:\n{}",
        differing.len(),
        CELLS.len(),
        differing.join("\n")
    );
}

/// Gives `dir` the default ACL `d:u::rwx,d:g::r-x,d:o::---`.
///
/// A filesystem without POSIX default ACLs fails the cell instead of skipping
/// it: without the ACL the cell cannot tell a folded default from the umask.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
fn set_default_acl(dir: &Path) {
    use exacl::{AclEntry, AclOption, Perm};

    let entries = [
        AclEntry::allow_user("", Perm::READ | Perm::WRITE | Perm::EXECUTE, None),
        AclEntry::allow_group("", Perm::READ | Perm::EXECUTE, None),
        AclEntry::allow_other(Perm::empty(), None),
    ];
    if let Err(error) = exacl::setfacl(&[dir], &entries, Some(AclOption::DEFAULT_ACL)) {
        panic!(
            "cannot set a default ACL on {} ({error}); this cell needs a filesystem \
             with POSIX default ACLs, such as ext4",
            dir.display()
        );
    }
}

/// upstream: generator.c:1776-1777 reads the parent's default ACL only when
/// `--perms` is off, and rsync.c:483-485 masks a new entry's mode with it, so
/// `d:g::r-x,d:o::---` turns a 0644 source file into 0640 and a 0755 directory
/// into 0750. A copy that ignored the ACL would land the umask's 0644 / 0755.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn a_relative_destination_folds_its_parents_default_acl_like_rsync_3_5_2() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let expected = run_in(upstream.binary(), Spelling::Relative, &[], set_default_acl);
    assert_eq!(
        (expected.exit, expected.stderr.as_str()),
        (Some(0), ""),
        "rsync 3.5.2 did not copy silently under a default ACL"
    );
    // The fixture must make the ACL visible, or the comparison below proves
    // nothing about folding it.
    let file_mode = expected
        .modes
        .iter()
        .find(|(name, _)| name == Path::new("f"))
        .map(|&(_, mode)| mode);
    assert_eq!(
        file_mode,
        Some(0o640),
        "rsync 3.5.2 did not apply the default ACL: {expected:?}"
    );
    let actual = run_in(&oc_rsync_bin(), Spelling::Relative, &[], set_default_acl);
    assert_eq!(
        actual, expected,
        "oc-rsync differs from rsync 3.5.2 under a default ACL on the destination's parent"
    );
}
