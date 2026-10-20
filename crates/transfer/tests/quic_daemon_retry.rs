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
//! `#![cfg(all(unix, feature = "quic"))]` - the daemon QUIC listener is
//! Unix-only. The test skips loudly only when the binary lacks QUIC or no
//! daemon can be started.

#![cfg(all(unix, feature = "quic"))]

use std::fs;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::tempdir;
use test_support::{DirDiff, DirDiffOptions};

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

/// Blocks until the daemon log contains `needle`, or the timeout elapses.
fn wait_for_log(log_path: &Path, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if fs::read_to_string(log_path).is_ok_and(|text| text.contains(needle)) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Pulls a small module over QUIC from a daemon with `quic retry = {retry}`
/// and returns the daemon log after the pull succeeded and the destination
/// matched the source.
fn pull_with_retry_policy(oc_bin: &Path, retry: &str) -> Option<String> {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path();
    let (config, log, cert, key) = (
        root.join("rsyncd.conf"),
        root.join("rsyncd.log"),
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
            "pid file = {pid}\nlock file = {lock}\nlog file = {log}\nuse chroot = false\n\
             quic cert file = {cert}\nquic key file = {key}\nquic retry = {retry}\n\
             [mod]\npath = {src}\nread only = true\n",
            pid = root.join("rsyncd.pid").display(),
            lock = root.join("rsyncd.lock").display(),
            log = log.display(),
            cert = cert.display(),
            key = key.display(),
            src = source.display(),
        ),
    )
    .expect("write config");

    let (child, port) = match test_support::spawn_daemon_on_free_port(|port| {
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
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    }) {
        Ok(spawned) => spawned,
        Err(e) => {
            eprintln!("skipping: could not start oc-rsync --daemon: {e}");
            return None;
        }
    };
    let _daemon = DaemonGuard(child);
    assert!(
        wait_for_log(&log, "QUIC listener serving on", Duration::from_secs(10)),
        "QUIC listener never came up:\n{}",
        fs::read_to_string(&log).unwrap_or_default()
    );

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
        fs::read_to_string(&log).unwrap_or_default()
    );
    match DirDiff::compare(&source, &dest, DirDiffOptions::structural()) {
        Ok(Ok(())) => {}
        Ok(Err(mismatch)) => panic!("destination differs:\n{}", mismatch.into_panic_message()),
        Err(e) => panic!("DirDiff infrastructure error: {e}"),
    }
    Some(fs::read_to_string(&log).unwrap_or_default())
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
    let Some(always) = pull_with_retry_policy(&oc_bin, "always") else {
        return;
    };
    assert!(
        always.contains(RETRY_LINE),
        "quic retry = always completed without a Retry:\n{always}"
    );
    let Some(never) = pull_with_retry_policy(&oc_bin, "never") else {
        return;
    };
    assert!(
        !never.contains(RETRY_LINE),
        "quic retry = never still sent a Retry:\n{never}"
    );
}
