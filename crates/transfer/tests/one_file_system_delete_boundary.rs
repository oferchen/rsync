//! `--delete -x` leaves a destination directory on a foreign filesystem
//! unscanned, on a local copy and on a pull.
//!
//! upstream: generator.c:316-326 delete_in_dir() records each top directory's
//! device and returns before get_dirlist() for any other directory whose
//! device is not one of them. A destination subdirectory that is a mount point
//! therefore keeps its extraneous files, even though its source counterpart is
//! an ordinary directory. rsync 3.5.1 keeps `sub/extra` and `sub/deep/extra`
//! in exactly this fixture.
//!
//! Mounting needs a mount namespace: the scenario runs under `unshare -rm`,
//! and the tests skip, saying so, where unprivileged user namespaces are off.
#![cfg(target_os = "linux")]

use std::fs;
use std::path::Path;
use std::process::Command;

use test_support::{LSH_STUB_BIN, LshRunnerStub, create_tempdir, require_binaries};

/// Whether this host lets an unprivileged process create a mount namespace.
fn mount_namespaces_available() -> bool {
    Command::new("unshare")
        .args(["-rm", "true"])
        .status()
        .is_ok_and(|status| status.success())
}

/// Runs `transfer` (the oc-rsync argv after `-r --delete -x`) inside a fresh
/// mount namespace in which `dest/sub` is a tmpfs, and returns which of the
/// destination-only files survived.
fn run_with_mounted_subdir(root: &Path, transfer: &[String]) -> String {
    let src = root.join("src");
    for dir in ["sub", "plain"] {
        fs::create_dir_all(src.join(dir)).expect("mkdir src");
        fs::write(src.join(dir).join("keep"), dir).expect("write src file");
        fs::create_dir_all(root.join("dest").join(dir)).expect("mkdir dest");
    }
    fs::write(root.join("dest/plain/extra"), b"x").expect("write extra");
    let script = r#"
        set -e
        dest="$1"; shift
        mount -t tmpfs boundary "$dest/sub"
        echo x > "$dest/sub/extra"
        mkdir "$dest/sub/deep" && echo x > "$dest/sub/deep/extra"
        "$@"
        for f in plain/extra sub/extra sub/deep/extra sub/keep plain/keep; do
            [ -e "$dest/$f" ] && echo "kept $f"
        done
        true
    "#;
    let output = Command::new("unshare")
        .args(["-rm", "sh", "-c", script, "sh"])
        .arg(root.join("dest"))
        .args(transfer)
        .output()
        .expect("unshare runs");
    assert!(
        output.status.success(),
        "scenario failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8 report")
}

fn assert_mount_left_unscanned(report: &str) {
    for kept in ["sub/extra", "sub/deep/extra", "sub/keep", "plain/keep"] {
        assert!(
            report.contains(&format!("kept {kept}\n")),
            "{kept} must survive: {report}"
        );
    }
    assert!(
        !report.contains("kept plain/extra"),
        "the same-filesystem extraneous file is still deleted: {report}"
    );
}

#[test]
fn local_delete_leaves_a_foreign_filesystem_unscanned() {
    require_binaries!("oc-rsync");
    if !mount_namespaces_available() {
        eprintln!("SKIP: unprivileged mount namespaces are unavailable");
        return;
    }
    let tmp = create_tempdir();
    let root = tmp.path();
    let oc = test_support::oc_rsync_bin();
    let report = run_with_mounted_subdir(
        root,
        &[
            oc.display().to_string(),
            "-r".into(),
            "--delete".into(),
            "-x".into(),
            format!("{}/", root.join("src").display()),
            format!("{}/", root.join("dest").display()),
        ],
    );
    assert_mount_left_unscanned(&report);
}

#[test]
fn pull_delete_leaves_a_foreign_filesystem_unscanned() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    if !mount_namespaces_available() {
        eprintln!("SKIP: unprivileged mount namespaces are unavailable");
        return;
    }
    let tmp = create_tempdir();
    let root = tmp.path();
    let oc = test_support::oc_rsync_bin();
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let report = run_with_mounted_subdir(
        root,
        &[
            oc.display().to_string(),
            "-r".into(),
            "--delete".into(),
            "-x".into(),
            format!("--rsh={}", stub.path().display()),
            format!("--rsync-path={}", oc.display()),
            format!("localhost:{}/", root.join("src").display()),
            format!("{}/", root.join("dest").display()),
        ],
    );
    assert_mount_left_unscanned(&report);
}
