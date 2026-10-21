//! `quic retry` end to end: a daemon told to send a stateless Retry to every
//! client still completes a pull, because the client follows the Retry inside
//! the QUIC handshake, and the daemon log proves the Retry was really sent.
//!
//! # Non-vacuity
//!
//! The proof is the server-side log line the QUIC front writes for a
//! connection it accepted only after the client echoed a Retry token. The
//! same pull against a `quic retry = never` daemon must not produce it, so the
//! line is caused by the directive and not by the transfer itself.
//!
//! The daemon logs to stderr and the test blocks on those lines as events, so
//! no wait depends on how fast the host is.
//!
//! `#![cfg(all(unix, feature = "quic"))]` - the daemon QUIC listener is
//! Unix-only. The test skips loudly only when the binary lacks QUIC or no
//! daemon can be started.

#![cfg(all(unix, feature = "quic"))]

use std::fs;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use tempfile::tempdir;
use test_support::{DirDiff, DirDiffOptions, LogLines};

/// The daemon log line written for a connection validated by a Retry.
const RETRY_LINE: &str = "validated by Retry";

/// Kills the daemon child on drop so a panicking test never leaks it.
struct DaemonGuard(Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Whether the binary was built with QUIC (`--help` lists `quic://`).
fn binary_supports_quic(oc_bin: &Path) -> bool {
    Command::new(oc_bin)
        .arg("--help")
        .stdin(Stdio::null())
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("quic://"))
}

/// Pulls a small module over QUIC from a daemon with `quic retry = {retry}`
/// and returns the daemon log up to the session's `connect from` line, after
/// the pull succeeded and the destination matched the source.
///
/// The daemon logs to stderr, read as events: the QUIC front writes the Retry
/// line before it hands the connection to the parent, and the session child
/// forked for that connection writes `connect from` after, so every Retry line
/// for the pull is in the log once `connect from` is.
fn pull_with_retry_policy(oc_bin: &Path, retry: &str) -> Option<Vec<String>> {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();
    let (config, cert, key) = (
        root.join("rsyncd.conf"),
        root.join("cert.pem"),
        root.join("key.pem"),
    );
    let source = root.join("source");
    let dest = root.join("dest");
    fs::create_dir_all(source.join("sub")).expect("mkdir source");
    fs::write(source.join("a.txt"), b"retry policy test\n").expect("write a");
    fs::write(source.join("sub/b.bin"), vec![7u8; 64 * 1024]).expect("write b");

    let pem = test_support::quic_cert::localhost_self_signed();
    fs::write(&cert, pem.cert_pem.as_bytes()).expect("write cert");
    fs::write(&key, pem.key_pem.as_bytes()).expect("write key");
    fs::write(
        &config,
        format!(
            "pid file = {pid}\nlock file = {lock}\nlog file = /dev/stderr\nuse chroot = false\n\
             quic cert file = {cert}\nquic key file = {key}\nquic retry = {retry}\n\
             [mod]\npath = {src}\nread only = true\n",
            pid = root.join("rsyncd.pid").display(),
            lock = root.join("rsyncd.lock").display(),
            cert = cert.display(),
            key = key.display(),
            src = source.display(),
        ),
    )
    .expect("write config");

    let (mut child, port) = match test_support::spawn_daemon_on_free_port(|port| {
        Command::new(oc_bin)
            .args([
                "--daemon",
                "--no-detach",
                "--port",
                &port.to_string(),
                "--config",
            ])
            .arg(&config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
    }) {
        Ok(spawned) => spawned,
        Err(e) => {
            eprintln!("skipping: could not start oc-rsync --daemon: {e}");
            return None;
        }
    };
    let mut log = LogLines::spawn(child.stderr.take().expect("daemon stderr"));
    let _daemon = DaemonGuard(child);
    // The client dials 127.0.0.1, which the IPv4 wildcard socket serves.
    log.wait_for(&format!("QUIC listener serving on 0.0.0.0:{port}"))
        .expect("QUIC listener never served IPv4");

    let mut dest_arg = dest.clone().into_os_string();
    dest_arg.push("/");
    let output = Command::new(oc_bin)
        .args(["-a", "-4", "--quic-ca"])
        .arg(&cert)
        .arg(format!("quic://localhost:{port}/mod/"))
        .arg(&dest_arg)
        .stdin(Stdio::null())
        .output()
        .expect("run client");
    assert!(
        output.status.success(),
        "QUIC pull with quic retry = {retry} failed: {:?}\nstderr:\n{}\ndaemon log:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
        log.transcript()
    );
    match DirDiff::compare(&source, &dest, DirDiffOptions::structural()) {
        Ok(Ok(())) => {}
        Ok(Err(mismatch)) => panic!("destination differs:\n{}", mismatch.into_panic_message()),
        Err(e) => panic!("DirDiff infrastructure error: {e}"),
    }
    log.wait_for("connect from").expect("session never logged");
    Some(log.seen().to_vec())
}

/// `quic retry = always` sends every client a Retry, the client follows it,
/// and the pull completes; `quic retry = never` sends none.
#[test]
fn quic_retry_always_completes_a_pull_through_a_real_retry() {
    let oc_bin = test_support::oc_rsync_bin();
    if !binary_supports_quic(&oc_bin) {
        eprintln!("skipping: {} was built without QUIC", oc_bin.display());
        return;
    }
    let retried = |log: &[String]| log.iter().any(|line| line.contains(RETRY_LINE));
    let Some(always) = pull_with_retry_policy(&oc_bin, "always") else {
        return;
    };
    assert!(
        retried(&always),
        "quic retry = always completed without a Retry:\n{}",
        always.join("\n")
    );
    let Some(never) = pull_with_retry_policy(&oc_bin, "never") else {
        return;
    };
    assert!(
        !retried(&never),
        "quic retry = never still sent a Retry:\n{}",
        never.join("\n")
    );
}
