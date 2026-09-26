//! The destination root's own attributes are applied, and a failure to apply
//! them is never manufactured by the receiver's kernel sandbox.
//!
//! Upstream enters the destination with a plain `change_dir()` and applies the
//! root's attributes to `"."` (main.c:778, rsync.c `set_file_attrs()`), so it
//! never opens the root's parent. oc resolved every entry's parent from the
//! absolute path, and for the root that parent lies outside the tree Landlock
//! grants the receiver: the open failed with `EACCES`, so the root's owner,
//! times, and mode were never applied. Reported as upstream's `FERROR_XFER`,
//! that became `chgrp "<dst>" failed: Permission denied (13)` and exit 23 on
//! any push into a destination outside the sandbox's read set, such as `/tmp`.
//!
//! Each cell asserts exit 0 AND that the root and a nested directory really
//! carry the source's mtime and mode, so a silent skip cannot pass.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// `rsync` invokes `$RSYNC_RSH <host> <command...>`; drop the host and exec the
/// command locally, so the receiver really is a separate `--server` process.
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
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod shim");
    script
}

fn backdate(path: &Path) {
    let status = Command::new("touch")
        .args(["-h", "-t", "202001010000"])
        .arg(path)
        .status()
        .expect("spawn touch");
    assert!(status.success(), "touch failed: {status}");
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    Local,
    Push,
    Pull,
}

/// Copies `src/` into an existing `dst/` under a fresh temp dir, from a working
/// directory outside it, and checks the root and a nested directory.
///
/// `dst/` must exist beforehand: a destination the receiver creates itself
/// widens the sandbox to its existing ancestor, which hides the defect.
fn assert_root_attrs_applied(mode: Mode) {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = fs::canonicalize(temp.path()).expect("canonicalize tempdir");
    let src = root.join("src");
    let nested = src.join("A weird)name");
    fs::create_dir_all(&nested).expect("create src tree");
    fs::write(nested.join("f"), b"x\n").expect("write src file");
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o750)).expect("chmod nested");
    fs::set_permissions(&src, fs::Permissions::from_mode(0o750)).expect("chmod src");
    backdate(&nested);
    backdate(&src);
    let shim = write_rsh_shim(&root);
    let dst = root.join("dst");
    fs::create_dir(&dst).expect("create dst");

    let binary = oc_rsync_binary();
    let from = format!("{}/", src.display());
    let to = format!("{}/", dst.display());
    let mut runner = test_support::OcRsyncCliRunner::new()
        .binary(&binary)
        .cwd(std::env::temp_dir().parent().unwrap_or(Path::new("/")))
        .arg("-a");
    runner = match mode {
        Mode::Local => runner.arg(&from).arg(&to),
        Mode::Push => runner
            .arg("--rsh")
            .arg(&shim)
            .arg("--rsync-path")
            .arg(&binary)
            .arg(&from)
            .arg(format!("h:{to}")),
        Mode::Pull => runner
            .arg("--rsh")
            .arg(&shim)
            .arg("--rsync-path")
            .arg(&binary)
            .arg(format!("h:{from}"))
            .arg(&to),
    };
    let out = runner.run().expect("transfer did not finish");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status,
        Some(0),
        "{mode:?}: applying the destination root's attributes must not fail\nstderr:\n{stderr}"
    );

    for (source, dest) in [(&src, dst.clone()), (&nested, dst.join("A weird)name"))] {
        let want = fs::metadata(source).expect("stat source");
        let got = fs::metadata(&dest).expect("stat destination");
        assert_eq!(
            got.mtime(),
            want.mtime(),
            "{mode:?}: {} must carry the source mtime (set_file_attrs times arm)",
            dest.display()
        );
        assert_eq!(
            got.mode() & 0o7777,
            want.mode() & 0o7777,
            "{mode:?}: {} must carry the source mode (set_file_attrs chmod arm)",
            dest.display()
        );
    }
}

#[test]
fn local_copy_applies_the_destination_root_attrs() {
    assert_root_attrs_applied(Mode::Local);
}

#[test]
fn push_applies_the_destination_root_attrs() {
    assert_root_attrs_applied(Mode::Push);
}

#[test]
fn pull_applies_the_destination_root_attrs() {
    assert_root_attrs_applied(Mode::Pull);
}
