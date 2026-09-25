//! `--temp-dir` on a different filesystem from the destination.
//!
//! A rename cannot cross a mount, so every commit from such a temp dir fails
//! with `EXDEV` unless the receiver copies the file across instead. Upstream
//! does exactly that in `robust_rename()`, so a transfer that works with an
//! in-tree temp dir must work, with the same result, when the temp dir is on
//! another filesystem. Before the receiver learned the fallback, every pull
//! and push failed with "Invalid cross-device link (os error 18)".
//!
//! The second filesystem is `/dev/shm` (tmpfs). Each test checks that it and
//! the destination really have different `st_dev`, and skips with a message
//! when the host has no second filesystem: a same-device run would pass
//! without ever reaching the fallback.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/util1.c:596` `robust_rename()` - the `EXDEV` arm.
//! - `rsync-3.5.0/rsync.c:882` `finish_transfer()`.

#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use test_support::{
    LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};

const OTHER_FILESYSTEM: &str = "/dev/shm";

/// A source tree, an empty destination, and a temp dir on another filesystem.
struct Fixture {
    _root: tempfile::TempDir,
    _temp: tempfile::TempDir,
    src: PathBuf,
    dest: PathBuf,
    temp_dir: PathBuf,
}

/// Builds the fixture, or returns `None` when no second filesystem exists.
fn fixture() -> Option<Fixture> {
    let root = create_tempdir();
    let Ok(temp) = tempfile::Builder::new()
        .prefix("oc-temp-dir-exdev")
        .tempdir_in(OTHER_FILESYSTEM)
    else {
        eprintln!("SKIP: {OTHER_FILESYSTEM} is not writable; no second filesystem");
        return None;
    };
    let root_dev = fs::metadata(root.path()).expect("stat root").dev();
    let temp_dev = fs::metadata(temp.path()).expect("stat temp").dev();
    if root_dev == temp_dev {
        eprintln!(
            "SKIP: {} and {OTHER_FILESYSTEM} share st_dev {root_dev}; \
             a rename would never cross a filesystem",
            root.path().display()
        );
        return None;
    }

    let src = root.path().join("src");
    let dest = root.path().join("dest");
    fs::create_dir_all(src.join("sub")).expect("mkdir src/sub");
    fs::create_dir(&dest).expect("mkdir dest");
    let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    fs::write(src.join("big"), &big).expect("write big");
    fs::set_permissions(src.join("big"), fs::Permissions::from_mode(0o640)).expect("chmod big");
    fs::write(src.join("sub").join("small"), b"small file\n").expect("write small");

    Some(Fixture {
        src,
        dest,
        temp_dir: temp.path().to_path_buf(),
        _root: root,
        _temp: temp,
    })
}

fn with_slash(path: &Path) -> String {
    format!("{}/", path.display())
}

/// The destination must hold the source bytes and mode, and the temp dir must
/// be empty: a copy that left its temp behind would leak one file per commit.
fn assert_committed(fx: &Fixture) {
    for name in ["big", "sub/small"] {
        assert_eq!(
            fs::read(fx.dest.join(name)).expect("read dest"),
            fs::read(fx.src.join(name)).expect("read src"),
            "{name} arrived intact",
        );
    }
    assert_eq!(
        fs::metadata(fx.dest.join("big"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640,
        "-a carries the mode across the copy",
    );
    let leftovers: Vec<_> = fs::read_dir(&fx.temp_dir).unwrap().collect();
    assert!(
        leftovers.is_empty(),
        "temp files were removed: {leftovers:?}"
    );
}

fn pull(fx: &Fixture, extra: &[&str]) -> test_support::CliOutput {
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    OcRsyncCliRunner::new()
        .arg("-a")
        .args(extra)
        .arg(format!("--temp-dir={}", fx.temp_dir.display()))
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ))
        .arg(format!("localhost:{}", with_slash(&fx.src)))
        .arg(with_slash(&fx.dest))
        .run()
        .expect("run oc-rsync")
}

#[test]
fn pull_commits_across_filesystems() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let Some(fx) = fixture() else { return };
    pull(&fx, &[]).assert_success();
    assert_committed(&fx);
}

/// A relative `--partial-dir` is where upstream stages the cross-filesystem
/// copy before renaming it into place; the staging dir must not survive.
#[test]
fn pull_with_relative_partial_dir_commits_across_filesystems() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let Some(fx) = fixture() else { return };
    pull(&fx, &["--partial-dir=.rsync-partial"]).assert_success();
    assert_committed(&fx);
    for dir in [
        fx.dest.join(".rsync-partial"),
        fx.dest.join("sub/.rsync-partial"),
    ] {
        assert!(!dir.exists(), "{} was removed", dir.display());
    }
}

/// `--delay-updates` renames the temp into its staging dir first, which
/// crosses the filesystem the same way.
#[test]
fn pull_with_delay_updates_commits_across_filesystems() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let Some(fx) = fixture() else { return };
    pull(&fx, &["--delay-updates"]).assert_success();
    assert_committed(&fx);
    assert!(!fx.dest.join(".~tmp~").exists());
}

/// A push makes the `--server` side the receiver. Its Landlock allowlist has to
/// admit the operator's temp dir, or every mkstemp there fails `EACCES`
/// before the commit is ever reached.
#[test]
fn push_commits_across_filesystems() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let Some(fx) = fixture() else { return };
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    OcRsyncCliRunner::new()
        .arg("-a")
        .arg(format!("--temp-dir={}", fx.temp_dir.display()))
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ))
        .arg(with_slash(&fx.src))
        .arg(format!("localhost:{}", with_slash(&fx.dest)))
        .run()
        .expect("run oc-rsync")
        .assert_success();
    assert_committed(&fx);
}

/// The local-copy executor has its own commit path; pin it to the same result.
#[test]
fn local_copy_commits_across_filesystems() {
    require_binaries!("oc-rsync");
    let Some(fx) = fixture() else { return };
    OcRsyncCliRunner::new()
        .arg("-a")
        .arg(format!("--temp-dir={}", fx.temp_dir.display()))
        .arg(with_slash(&fx.src))
        .arg(with_slash(&fx.dest))
        .run()
        .expect("run oc-rsync")
        .assert_success();
    assert_committed(&fx);
}
