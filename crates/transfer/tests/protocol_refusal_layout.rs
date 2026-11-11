//! A protocol refusal prints upstream's two-line layout: the refusal itself on
//! a bare line, then log_exit()'s `protocol incompatibility (code 2)` trailer.
//!
//! upstream: exclude.c:1924-1926 send_filter_list() - a rule that needs a
//! newer protocol than the one negotiated is `rprintf(FERROR, "filter rules are
//! too modern for remote rsync.\n")` then `exit_cleanup(RERR_PROTOCOL)`. `-FF`
//! adds a `dir-merge` rule, which has no protocol-28 spelling. Measured with
//! 3.5.1 at both ends over a remote shell and against a 3.5.1 daemon: the same
//! two lines on a push (`[sender=...]`) and a pull (`[Receiver=...]`), and
//! nothing from the server, whose messages are framed to the departed client
//! at protocol 28 (compat.c:653-654).
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

/// Each stderr line with the trailer's source position and version cut, and
/// the program name in upstream's spelling.
fn normalised(out: &CliOutput) -> Vec<String> {
    out.stderr_str()
        .lines()
        .map(|line| {
            let line = line.strip_prefix("oc-").unwrap_or(line);
            let trailer = line.split_once(" at ").and_then(|(message, rest)| {
                let (_, tag) = rest.rsplit_once(" [")?;
                let (role, _) = tag.split_once('=')?;
                Some(format!("{message} [{role}]"))
            });
            trailer.unwrap_or_else(|| line.to_owned())
        })
        .collect()
}

/// The client's own lines. An oc server still reports its EOF on its stderr
/// where 3.5.1 frames it at protocol 28 (compat.c:653-654), so its
/// `[server]` line is left out here; the 3.5.1-server cell holds the whole
/// stream.
fn client_lines(out: &CliOutput) -> Vec<String> {
    normalised(out)
        .into_iter()
        .filter(|line| !line.ends_with(" [server]"))
        .collect()
}

fn assert_refusal(push: bool, role: &str) {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let oc = test_support::oc_rsync_bin();
    let tmp = create_tempdir();
    let out = run(&oc, &oc, tmp.path(), push);
    out.assert_exit(2);
    assert_eq!(
        client_lines(&out),
        [
            "filter rules are too modern for remote rsync.".to_owned(),
            format!("rsync error: protocol incompatibility (code 2) [{role}]"),
        ],
        "stderr:\n{}",
        out.stderr_str()
    );
    if let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) {
        let against_upstream = run(&oc, upstream.binary(), tmp.path(), push);
        against_upstream.assert_exit(2);
        let reference = run(upstream.binary(), upstream.binary(), tmp.path(), push);
        reference.assert_exit(2);
        assert_eq!(
            normalised(&against_upstream),
            normalised(&reference),
            "oc client against 3.5.1, both at a 3.5.1 server"
        );
    }
}

#[test]
fn push_refuses_protocol_28_filter_rules_in_two_lines() {
    assert_refusal(true, "sender");
}

#[test]
fn pull_refuses_protocol_28_filter_rules_in_two_lines() {
    assert_refusal(false, "Receiver");
}
