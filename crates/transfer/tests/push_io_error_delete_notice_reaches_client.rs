//! A pushing client sees the remote receiver's skipped-deletion notice.
//!
//! When the sender reports a general I/O error, the receiving generator skips
//! every deletion and says so once with `rprintf(FINFO, "IO error encountered
//! -- skipping file deletion\n")` (upstream: generator.c:304-311). On a push
//! that generator is the server, and `rwrite()` frames its FINFO text as
//! `MSG_INFO` for the client to print (upstream: log.c:355-366). An oc server
//! receiver logged the notice only to its own thread-local sink, so the client
//! never saw why its `--delete` did nothing.
//!
//! Verified with rsync 3.5.1 over a loopback rsh: pushing `-rL --delete-after`
//! with a source holding a self-referencing symlink prints the notice on the
//! client's stdout, keeps the extraneous destination file and exits 23.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::time::Duration;

use test_support::{
    LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};

const NOTICE: &str = "IO error encountered -- skipping file deletion\n";

#[test]
fn io_error_delete_notice_from_an_oc_receiver_reaches_the_pushing_client() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    for mode in [
        "--delete-during",
        "--delete-delay",
        "--delete-before",
        "--delete-after",
    ] {
        let tmp = create_tempdir();
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).expect("mkdir src");
        fs::create_dir_all(root.join("dst")).expect("mkdir dst");
        fs::write(root.join("src/keep"), b"keep\n").expect("write keep");
        // A self-loop fails the sender's stat under -L even as root, which a
        // permission-based fixture would not.
        symlink("loop", root.join("src/loop")).expect("symlink loop");
        fs::write(root.join("dst/extra"), b"stale\n").expect("write extra");

        let stub = LshRunnerStub::locate().expect("lsh-stub located");
        let out = OcRsyncCliRunner::new()
            .arg("-rL")
            .arg(mode)
            .arg(format!("--rsh={}", stub.path().display()))
            .arg(format!(
                "--rsync-path={}",
                test_support::oc_rsync_bin().display()
            ))
            .arg(format!("{}/", root.join("src").display()))
            .arg(format!("localhost:{}/", root.join("dst").display()))
            .timeout(Duration::from_secs(20))
            .run()
            .unwrap_or_else(|e| panic!("{mode}: push did not finish: {e}"));
        out.assert_exit(23);
        assert_eq!(
            out.stdout_str().matches(NOTICE).count(),
            1,
            "{mode}: the client must print the receiver's notice exactly once: \
             stdout={} stderr={}",
            out.stdout_str(),
            out.stderr_str()
        );
        assert!(root.join("dst/extra").exists(), "{mode}: extra kept");
        assert!(root.join("dst/keep").exists(), "{mode}: keep arrives");
    }
}
