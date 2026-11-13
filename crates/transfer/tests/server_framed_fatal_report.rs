//! A `--server` at protocol 28 frames its fatal report for the client instead
//! of writing it to its own stderr.
//!
//! upstream: compat.c:653-654 clears `msgs2stderr` for a server below protocol
//! 29, so log.c:357-367 rwrite() queues every FERROR line for the client
//! (io.c:1088 send_msg()). When the client refuses `-FF` at protocol 28
//! (exclude.c:1924-1926) and exits, the server meets end-of-file; its
//! "connection unexpectedly closed" report and code-12 trailer are queued and
//! never flushed (cleanup.c:241-243), so the client's two lines are all the
//! terminal shows. Measured with 3.5.1 at both ends: strace shows the server
//! write nothing to fd 2 before exit_group(12).
#![cfg(unix)]
use std::fs;
use std::path::Path;
use test_support::{
    CliOutput, LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, UpstreamVersion, create_tempdir,
    require_binaries, require_upstream_rsync,
};

/// Runs `client` over the lsh stub against `server` with `--protocol=28 -FF`.
fn run(client: &Path, server: &Path, root: &Path, push: bool) -> CliOutput {
    let src = root.join("src");
    fs::create_dir_all(&src).expect("mkdir src");
    fs::write(src.join("f"), b"x\n").expect("write f");
    let dest = root.join("dest");
    fs::create_dir_all(&dest).expect("mkdir dest");
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let runner = OcRsyncCliRunner::new()
        .binary(client)
        .args(["-r", "-FF", "--protocol=28"])
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!("--rsync-path={}", server.display()));
    let runner = if push {
        runner
            .arg(format!("{}/", src.display()))
            .arg(format!("localhost:{}/", dest.display()))
    } else {
        runner
            .arg(format!("localhost:{}/", src.display()))
            .arg(format!("{}/", dest.display()))
    };
    runner.run().expect("transfer finishes")
}

/// The `error:` trailers on stderr; the client's RERR_PROTOCOL trailer is the
/// only one when the server's report stays off the terminal.
fn trailers(out: &CliOutput) -> Vec<String> {
    out.stderr_str()
        .lines()
        .filter(|line| line.contains("error: ") && line.contains("(code "))
        .map(str::to_owned)
        .collect()
}

fn assert_server_stays_silent(push: bool) {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let oc = test_support::oc_rsync_bin();
    let tmp = create_tempdir();
    let out = run(&oc, &oc, tmp.path(), push);
    out.assert_exit(2);
    let trailers = trailers(&out);
    assert_eq!(trailers.len(), 1, "stderr:\n{}", out.stderr_str());
    assert!(
        trailers[0].contains("(code 2)"),
        "stderr:\n{}",
        out.stderr_str()
    );
    if let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) {
        let against_oc = run(upstream.binary(), &oc, tmp.path(), push);
        let reference = run(upstream.binary(), upstream.binary(), tmp.path(), push);
        assert_eq!(against_oc.status, reference.status);
        assert_eq!(
            against_oc.stderr_str(),
            reference.stderr_str(),
            "3.5.1 client against an oc server and against a 3.5.1 server"
        );
    }
}

#[test]
fn push_server_receiver_keeps_its_eof_report_off_stderr() {
    assert_server_stays_silent(true);
}

#[test]
fn pull_server_sender_keeps_its_eof_report_off_stderr() {
    assert_server_stays_silent(false);
}
