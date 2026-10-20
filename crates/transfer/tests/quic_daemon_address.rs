//! `quic address` end to end: a daemon whose TCP listener stays on the default
//! wildcard binds its QUIC listener on exactly the configured loopback
//! addresses, logs exactly those, and serves a QUIC pull through them.
//!
//! `::1` is added only when this host can bind it, probed with a real bind
//! rather than assumed. `#![cfg(all(unix, feature = "quic"))]` - the daemon
//! QUIC listener is Unix-only. The test skips loudly only when the binary lacks
//! QUIC or no daemon can be started.

#![cfg(all(unix, feature = "quic"))]

use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::tempdir;
use test_support::{DirDiff, DirDiffOptions};

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

/// Returns the daemon log line containing `needle`, waiting until it appears
/// or the timeout elapses.
fn wait_for_log_line(log_path: &Path, needle: &str, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(text) = fs::read_to_string(log_path)
            && let Some(line) = text.lines().find(|line| line.contains(needle))
        {
            return Some(line.to_owned());
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The loopback addresses to configure: IPv4 always, IPv6 when it binds here.
fn loopback_addresses() -> Vec<IpAddr> {
    let mut addresses = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
    let v6 = IpAddr::V6(Ipv6Addr::LOCALHOST);
    if UdpSocket::bind(SocketAddr::new(v6, 0)).is_ok() {
        addresses.push(v6);
    } else {
        eprintln!("note: this host cannot bind ::1; testing 127.0.0.1 only");
    }
    addresses
}

/// `quic address = 127.0.0.1 ::1` puts QUIC on exactly those addresses while
/// TCP keeps its own default, and a QUIC pull through 127.0.0.1 succeeds.
#[test]
fn quic_address_binds_exactly_the_configured_loopback_addresses() {
    let oc_bin = test_support::oc_rsync_bin();
    if !binary_supports_quic(&oc_bin) {
        eprintln!("skipping: {} was built without QUIC", oc_bin.display());
        return;
    }
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
    fs::write(source.join("a.txt"), b"quic address test\n").expect("write a");
    fs::write(source.join("sub/b.bin"), vec![3u8; 32 * 1024]).expect("write b");

    let pem = test_support::quic_cert::localhost_self_signed();
    fs::write(&cert, pem.cert_pem.as_bytes()).expect("write cert");
    fs::write(&key, pem.key_pem.as_bytes()).expect("write key");
    let addresses = loopback_addresses();
    let listed: Vec<String> = addresses.iter().map(IpAddr::to_string).collect();
    fs::write(
        &config,
        format!(
            "pid file = {pid}\nlock file = {lock}\nlog file = {log}\nuse chroot = false\n\
             quic cert file = {cert}\nquic key file = {key}\nquic address = {listed}\n\
             [mod]\npath = {src}\nread only = true\n",
            pid = root.join("rsyncd.pid").display(),
            lock = root.join("rsyncd.lock").display(),
            log = log.display(),
            cert = cert.display(),
            key = key.display(),
            listed = listed.join(" "),
            src = source.display(),
        ),
    )
    .expect("write config");

    let (child, port) = match test_support::spawn_daemon_on_free_port(|port| {
        Command::new(&oc_bin)
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
            return;
        }
    };
    let _daemon = DaemonGuard(child);

    let bound_line = wait_for_log_line(&log, "QUIC listener bound on", Duration::from_secs(10))
        .unwrap_or_else(|| {
            panic!(
                "QUIC listener never bound:\n{}",
                fs::read_to_string(&log).unwrap_or_default()
            )
        });
    let expected: Vec<String> = addresses
        .iter()
        .map(|ip| SocketAddr::new(*ip, port).to_string())
        .collect();
    let bound = bound_line
        .split_once("QUIC listener bound on ")
        .map(|(_, rest)| rest.trim())
        .expect("bound line names the addresses");
    assert_eq!(
        bound,
        expected.join(" and "),
        "QUIC must bind exactly the configured addresses"
    );
    assert!(
        wait_for_log_line(&log, "QUIC listener serving on", Duration::from_secs(10)).is_some(),
        "QUIC listener never started serving"
    );

    let mut dest_arg = dest.clone().into_os_string();
    dest_arg.push("/");
    let output = Command::new(&oc_bin)
        .args(["-a", "-4", "--quic-ca"])
        .arg(&cert)
        .arg(format!("quic://localhost:{port}/mod/"))
        .arg(&dest_arg)
        .stdin(Stdio::null())
        .output()
        .expect("run client");
    assert!(
        output.status.success(),
        "QUIC pull via 127.0.0.1 failed: {:?}\nstderr:\n{}\ndaemon log:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(&log).unwrap_or_default()
    );
    match DirDiff::compare(&source, &dest, DirDiffOptions::structural()) {
        Ok(Ok(())) => {}
        Ok(Err(mismatch)) => panic!("destination differs:\n{}", mismatch.into_panic_message()),
        Err(e) => panic!("DirDiff infrastructure error: {e}"),
    }
}
