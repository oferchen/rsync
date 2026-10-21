//! A QUIC daemon whose certificate and private key do not match refuses to
//! start.
//!
//! A missing `quic cert file` or `quic key file` is already a fatal config
//! error. A pair that is present but inconsistent must be treated the same
//! way: if the daemon only logged it and kept serving TCP, the operator would
//! believe QUIC was up when no QUIC listener exists. The identity is therefore
//! loaded at startup, before any listener is bound, and a failure exits 1.
#![cfg(all(unix, feature = "quic"))]
use std::fs;
use std::net::TcpListener;
use std::process::Command;
use std::time::Duration;
use tempfile::tempdir;
use test_support::Deadlined;

#[test]
fn daemon_with_mismatched_quic_cert_and_key_exits_1_at_startup() {
    let oc_bin = test_support::oc_rsync_bin();
    let help = Command::new(&oc_bin).arg("--help").output();
    if !help.is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("quic://")) {
        eprintln!(
            "skipping: {} was built without the `quic` feature",
            oc_bin.display()
        );
        return;
    }
    let scratch = tempdir().expect("tempdir");
    let root = scratch.path();
    let module = root.join("module");
    fs::create_dir(&module).expect("module dir");
    // Two independent identities: the certificate of one and the key of the
    // other form a well-formed but inconsistent pair.
    let (cert_pem, key_pem) = (
        test_support::quic_cert::localhost_self_signed().cert_pem,
        test_support::quic_cert::localhost_self_signed().key_pem,
    );
    let (cert, key) = (root.join("cert.pem"), root.join("key.pem"));
    fs::write(&cert, cert_pem).expect("cert");
    fs::write(&key, key_pem).expect("key");
    let config = root.join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "pid file = {pid}\nlock file = {lock}\nuse chroot = false\n\
             quic cert file = {cert}\nquic key file = {key}\n\n\
             [m]\npath = {module}\nread only = true\n",
            pid = root.join("d.pid").display(),
            lock = root.join("d.lock").display(),
            cert = cert.display(),
            key = key.display(),
            module = module.display(),
        ),
    )
    .expect("config");
    let port = TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("free port")
        .port();
    let outcome = test_support::run_deadlined(
        Command::new(&oc_bin)
            .args(["--daemon", "--no-detach", "--port", &port.to_string()])
            .arg("--config")
            .arg(&config),
        Duration::from_secs(10),
    )
    .expect("run oc-rsync --daemon");
    match outcome {
        Deadlined::Finished { status, stderr, .. } => {
            let stderr = String::from_utf8_lossy(&stderr);
            assert_eq!(status.code(), Some(1), "stderr: {stderr}");
            assert!(
                stderr.contains("failed to load the QUIC server identity")
                    && stderr.contains("the private key does not match the certificate"),
                "stderr must name the identity failure: {stderr}"
            );
        }
        Deadlined::Expired { stderr, .. } => panic!(
            "daemon kept running with a mismatched QUIC cert/key: {}",
            String::from_utf8_lossy(&stderr)
        ),
    }
}
