//! A pulling client prints its own `*deleting` rows under `--itemize-changes`.
//!
//! On a client, upstream's `log_delete()` renders the row locally through
//! `log_formatted(FCLIENT, ...)` (upstream: log.c:919-921). Only a server
//! forwards it, as `MSG_DELETED` (log.c:913-916). An oc client receiver that
//! sent the row to the remote sender as `MSG_INFO` hung an oc-to-oc pull in
//! the goodbye: an oc sender does not expect `MSG_INFO` from its peer. Against
//! an upstream sender the row only appeared because that sender echoed it
//! back.
//!
//! Verified with rsync 3.5.1 over a loopback rsh: pulling `-ri --delete`
//! prints `*deleting   d/extra` then `>f+++++++++ d/keep` and exits 0, and so
//! does an upstream client pulling from an oc server.

#![cfg(unix)]

use std::fs;
use std::time::Duration;

use test_support::{
    LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};

#[test]
fn itemized_delete_pull_from_an_oc_sender_completes_and_prints_locally() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    for mode in [
        "--delete-during",
        "--delete-delay",
        "--delete-before",
        "--delete-after",
    ] {
        let tmp = create_tempdir();
        let root = tmp.path();
        fs::create_dir_all(root.join("src/d")).expect("mkdir src");
        fs::create_dir_all(root.join("dst/d")).expect("mkdir dst");
        fs::write(root.join("src/d/keep"), b"keep\n").expect("write keep");
        fs::write(root.join("dst/d/extra"), b"stale\n").expect("write extra");

        let stub = LshRunnerStub::locate().expect("lsh-stub located");
        let out = OcRsyncCliRunner::new()
            .arg("-ri")
            .arg(mode)
            .arg(format!("--rsh={}", stub.path().display()))
            .arg(format!(
                "--rsync-path={}",
                test_support::oc_rsync_bin().display()
            ))
            .arg(format!("localhost:{}/", root.join("src").display()))
            .arg(format!("{}/", root.join("dst").display()))
            .timeout(Duration::from_secs(20))
            .run()
            .unwrap_or_else(|e| panic!("{mode}: pull did not finish: {e}"));
        out.assert_exit(0);
        assert_eq!(
            out.stdout_str().matches("*deleting   d/extra\n").count(),
            1,
            "{mode}: the client must print the row exactly once: {}",
            out.stdout_str()
        );
        assert!(!root.join("dst/d/extra").exists(), "{mode}: extra deleted");
        assert!(root.join("dst/d/keep").exists(), "{mode}: keep arrives");
    }
}

/// The pulling client prints its rows in generator order: a sweep that runs
/// before the generate loop (`--delete-before`, `--delete-during` without
/// INC_RECURSE) itemizes the deletion first, while the late sweep of
/// `--delete-after` and `--delete-delay` follows every transfer row
/// (upstream: generator.c:2899-2902). Verified against rsync 3.5.1 on the same
/// fixture for all four modes.
#[test]
fn itemized_delete_pull_rows_follow_the_generator_order() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    const DELETED: &str = "*deleting   d/extra\n";
    const SENT: &str = ">f+++++++++ d/keep\n";
    for (mode, deletion_first) in [
        ("--delete-before", true),
        ("--delete-during", true),
        ("--delete-after", false),
        ("--delete-delay", false),
    ] {
        let tmp = create_tempdir();
        let root = tmp.path();
        fs::create_dir_all(root.join("src/d")).expect("mkdir src");
        fs::create_dir_all(root.join("dst/d")).expect("mkdir dst");
        fs::write(root.join("src/d/keep"), b"keep\n").expect("write keep");
        fs::write(root.join("dst/d/extra"), b"stale\n").expect("write extra");

        let stub = LshRunnerStub::locate().expect("lsh-stub located");
        let out = OcRsyncCliRunner::new()
            .arg("-ri")
            .arg(mode)
            .arg(format!("--rsh={}", stub.path().display()))
            .arg(format!(
                "--rsync-path={}",
                test_support::oc_rsync_bin().display()
            ))
            .arg(format!("localhost:{}/", root.join("src").display()))
            .arg(format!("{}/", root.join("dst").display()))
            .timeout(Duration::from_secs(20))
            .run()
            .unwrap_or_else(|e| panic!("{mode}: pull did not finish: {e}"));
        out.assert_exit(0);
        let stdout = out.stdout_str();
        let deleted = stdout
            .find(DELETED)
            .unwrap_or_else(|| panic!("{mode}: no deletion row: {stdout}"));
        let sent = stdout
            .find(SENT)
            .unwrap_or_else(|| panic!("{mode}: no transfer row: {stdout}"));
        assert_eq!(
            deleted < sent,
            deletion_first,
            "{mode}: rows out of generator order: {stdout}"
        );
    }
}
