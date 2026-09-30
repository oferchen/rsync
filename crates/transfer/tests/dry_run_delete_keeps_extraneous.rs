//! A `--dry-run` transfer with a delete mode must leave the destination alone.
//!
//! Upstream reports every deletion a dry run would make and performs none of
//! them: `delete_item()` still logs and counts the victim, but the unlink and
//! rmdir underneath are no-ops (upstream: syscall.c:779-781 `do_unlink()` and
//! syscall.c:1529-1531 `do_rmdir()` return 0 under `dry_run`). A receiver that
//! skips that gate destroys exactly the files the user asked to preview.
//!
//! Every delete mode is pinned, pushing and pulling over the lsh-stub
//! loopback. The opposed control runs the same transfer without `-n` and must
//! delete, so the fixture can tell a kept file from a mode that never deletes.
//! Verified against rsync 3.5.1 over a loopback rsh: in every mode, pushing
//! and pulling, the dry run exits 0, keeps all three extraneous entries and
//! prints a `deleting` line for each of them.

#![cfg(unix)]

use std::fs;
use std::path::Path;

use test_support::{
    LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};

const MODES: [&str; 5] = [
    "delete",
    "delete-during",
    "delete-delay",
    "delete-before",
    "delete-after",
];

/// Builds `root/src/d/keep` and a destination that also holds an extraneous
/// file and an extraneous directory with a child.
fn fixture(root: &Path) {
    fs::create_dir_all(root.join("src/d")).expect("mkdir src");
    fs::write(root.join("src/d/keep"), b"keep\n").expect("write keep");
    fs::create_dir_all(root.join("dst/d/gone_dir")).expect("mkdir dst");
    fs::write(root.join("dst/d/keep"), b"keep\n").expect("write dst keep");
    fs::write(root.join("dst/d/extra"), b"stale\n").expect("write extra");
    fs::write(root.join("dst/d/gone_dir/child"), b"stale\n").expect("write child");
}

fn run(root: &Path, mode: &str, push: bool, dry_run: bool) -> test_support::CliOutput {
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let src = format!("{}/", root.join("src").display());
    let dst = format!("{}/", root.join("dst").display());
    let (src, dst) = if push {
        (src, format!("localhost:{dst}"))
    } else {
        (format!("localhost:{src}"), dst)
    };
    let mut runner = OcRsyncCliRunner::new()
        .arg("-rv")
        .arg(format!("--{mode}"))
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ));
    if dry_run {
        runner = runner.arg("--dry-run");
    }
    runner.arg(src).arg(dst).run().expect("transfer run")
}

fn check(push: bool) {
    for mode in MODES {
        let tmp = create_tempdir();
        fixture(tmp.path());
        let out = run(tmp.path(), mode, push, true);
        out.assert_exit(0);
        let stdout = out.stdout_str();
        for (victim, line) in [
            ("d/extra", "deleting d/extra"),
            ("d/gone_dir", "deleting d/gone_dir/"),
            ("d/gone_dir/child", "deleting d/gone_dir/child"),
        ] {
            assert!(
                tmp.path().join("dst").join(victim).exists(),
                "--dry-run --{mode} push={push} removed {victim}"
            );
            assert!(
                stdout.contains(line),
                "--dry-run --{mode} push={push} must still report `{line}`: {stdout}"
            );
        }

        // Opposed control: the same transfer without --dry-run deletes both.
        let out = run(tmp.path(), mode, push, false);
        out.assert_exit(0);
        for victim in ["d/extra", "d/gone_dir"] {
            assert!(
                !tmp.path().join("dst").join(victim).exists(),
                "--{mode} push={push} kept {victim}: the fixture does not discriminate"
            );
        }
    }
}

#[test]
fn dry_run_pull_keeps_every_extraneous_entry() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    check(false);
}

#[test]
fn dry_run_push_keeps_every_extraneous_entry() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    check(true);
}
