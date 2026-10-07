//! A `-L` transfer that meets a dangling symlink must report "symlink has no
//! referent" and exit 23, not call the entry vanished and exit 24.
//!
//! upstream `flist.c:1668-1681`, `make_file()`: when one of the `--copy*links`
//! options made the sender dereference the entry, an `ENOENT` on a path that
//! `lstat` still shows as a symlink sets `IOERR_GENERAL` and logs
//! `FERROR_XFER "symlink has no referent: %s"`. Only a genuinely missing entry
//! takes the `IOERR_VANISHED` / "file has vanished" arm. `cleanup.c:210-218`
//! then maps the two bits to 23 and 24.
//!
//! The network sender used to hand every `ENOENT` to the vanished arm, so a pull
//! from (or a push by) an oc sender reported a broken link as a file that
//! disappeared mid-run. Measured against rsync 3.5.1 on this fixture: rc 23 and
//! the "no referent" line, for both directions.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn write_rsh_shim(dir: &Path) -> PathBuf {
    let script = dir.join("fake_rsh.sh");
    fs::write(
        &script,
        "#!/bin/sh\n\
         while [ $# -gt 0 ]; do\n\
         case \"$1\" in\n\
         -*) shift ;;\n\
         *) break ;;\n\
         esac\n\
         done\n\
         shift || true\n\
         exec \"$@\"\n",
    )
    .expect("write rsh shim");
    let mut perms = fs::metadata(&script).expect("stat shim").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).expect("chmod shim");
    script
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    shim: PathBuf,
}

/// `src/` holds a regular file plus `link`, whose target does not exist (or,
/// for the control, does).
fn fixture(dangling: bool) -> Fixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().to_path_buf();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::create_dir_all(root.join("dst")).expect("create dst");
    fs::write(root.join("src/file"), "data\n").expect("write src");
    if !dangling {
        fs::write(root.join("src/target"), "target\n").expect("write target");
    }
    std::os::unix::fs::symlink("target", root.join("src/link")).expect("symlink");
    let shim = write_rsh_shim(&root);
    Fixture {
        _temp: temp,
        root,
        shim,
    }
}

/// Runs `-rL` with the remote side spelled on `src` (pull) or `dst` (push), so
/// the oc sender under test is the remote `--server` or the local client.
fn run(fx: &Fixture, pull: bool) -> (Option<i32>, String) {
    let binary = oc_rsync_binary();
    let src = format!("{}/src/", fx.root.display());
    let dst = format!("{}/dst/", fx.root.display());
    let (src, dst) = if pull {
        (format!("lhost:{src}"), dst)
    } else {
        (src, format!("lhost:{dst}"))
    };
    let out = test_support::OcRsyncCliRunner::new()
        .binary(&binary)
        .args(["-rL"])
        .arg("--rsh")
        .arg(&fx.shim)
        .arg("--rsync-path")
        .arg(&binary)
        .arg(src)
        .arg(dst)
        .run()
        .expect("transfer did not finish");
    (
        out.status,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn assert_no_referent(pull: bool) {
    let fx = fixture(true);
    let (status, stderr) = run(&fx, pull);
    assert!(
        stderr.contains("symlink has no referent: ") && stderr.contains("link"),
        "stderr was: {stderr}"
    );
    assert!(
        !stderr.contains("file has vanished"),
        "stderr was: {stderr}"
    );
    assert_eq!(status, Some(23), "stderr was: {stderr}");
    // The rest of the transfer still happens.
    assert!(fx.root.join("dst/file").exists(), "stderr was: {stderr}");
}

#[test]
fn pull_from_an_oc_sender_reports_a_dangling_link_as_no_referent() {
    assert_no_referent(true);
}

#[test]
fn push_by_an_oc_sender_reports_a_dangling_link_as_no_referent() {
    assert_no_referent(false);
}

/// Non-vacuity: the same fixture with the target present dereferences the link
/// into a regular file and exits 0, so 23 above comes from the dangling link.
#[test]
fn a_link_with_a_referent_is_copied_as_its_target() {
    let fx = fixture(false);
    let (status, stderr) = run(&fx, true);
    assert_eq!(status, Some(0), "stderr was: {stderr}");
    let copied = fs::symlink_metadata(fx.root.join("dst/link")).expect("stat copy");
    assert!(copied.file_type().is_file(), "stderr was: {stderr}");
}
