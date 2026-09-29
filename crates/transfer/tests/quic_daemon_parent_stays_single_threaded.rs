//! The daemon parent stays single-threaded while it serves QUIC.
//!
//! Every session is forked from the daemon parent, and `fork` is only sound
//! from a single-threaded process: a child forked while another thread held
//! the allocator or the log sink's mutex inherits that lock with no thread left
//! to release it. QUIC needs threads (the endpoint driver), so they live in a
//! separate front process that relays each stream to the parent over a socket.
//!
//! This test pins the invariant from outside, through `/proc`: once the QUIC
//! listener is serving and after a QUIC pull, the parent reports exactly one
//! thread, and the process that logs "QUIC listener serving" is a different
//! pid whose parent is the daemon. Serving QUIC inside the parent again would
//! add the endpoint threads to it and fail the thread count.
//!
//! upstream: socket.c:761-773 `start_accept_loop()` forks per connection from
//! a single-threaded parent.
#![cfg(all(target_os = "linux", feature = "quic"))]

use std::fs;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::tempdir;

/// Kills the daemon on drop so a failing assertion never leaks the listener.
struct DaemonGuard(Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Reads one `Name:\tvalue` field from `/proc/<pid>/status`.
fn proc_status_field(pid: u32, field: &str) -> Option<u32> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix(field)?.strip_prefix(':'))
        .and_then(|value| value.trim().parse().ok())
}

/// Returns the bracketed pid of the first log line containing `needle`.
fn logged_pid(log: &Path, needle: &str) -> Option<u32> {
    let text = fs::read_to_string(log).ok()?;
    let line = text.lines().find(|line| line.contains(needle))?;
    let start = line.find('[')? + 1;
    let end = start + line[start..].find(']')?;
    line[start..end].parse().ok()
}

/// Polls the daemon log until `needle` appears, or the timeout elapses.
fn wait_for_log(log: &Path, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if fs::read_to_string(log).is_ok_and(|text| text.contains(needle)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

#[test]
fn daemon_parent_has_one_thread_while_serving_quic() {
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
    fs::write(
        module.join("file.txt"),
        b"relayed through the front process\n",
    )
    .expect("module file");
    let pem = test_support::quic_cert::localhost_self_signed();
    let (cert, key, log) = (
        root.join("cert.pem"),
        root.join("key.pem"),
        root.join("d.log"),
    );
    fs::write(&cert, pem.cert_pem).expect("cert");
    fs::write(&key, pem.key_pem).expect("key");
    let config = root.join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "pid file = {pid}\nlock file = {lock}\nlog file = {log}\nuse chroot = false\n\
             quic cert file = {cert}\nquic key file = {key}\n\n\
             [m]\npath = {module}\nread only = true\n",
            pid = root.join("d.pid").display(),
            lock = root.join("d.lock").display(),
            log = log.display(),
            cert = cert.display(),
            key = key.display(),
            module = module.display(),
        ),
    )
    .expect("config");

    let (child, port) = test_support::spawn_daemon_on_free_port(|port| {
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
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    })
    .expect("start oc-rsync --daemon");
    let _daemon = DaemonGuard(child);
    assert!(
        wait_for_log(&log, "QUIC listener serving on", Duration::from_secs(10)),
        "QUIC listener never reported serving"
    );

    let parent = logged_pid(&log, "starting, listening on port").expect("daemon parent pid");
    let front = logged_pid(&log, "QUIC listener serving on").expect("front pid");
    assert_ne!(
        front, parent,
        "QUIC must be served outside the daemon parent"
    );
    assert_eq!(
        proc_status_field(front, "PPid"),
        Some(parent),
        "the QUIC front process must be the daemon's child"
    );
    assert_eq!(
        proc_status_field(parent, "Threads"),
        Some(1),
        "daemon parent must be single-threaded once QUIC is serving"
    );

    let dest = root.join("dest");
    let pull = Command::new(&oc_bin)
        .args(["-a", "-4", "--quic-ca"])
        .arg(&cert)
        .arg(format!("quic://localhost:{port}/m/"))
        .arg(&dest)
        .output()
        .expect("run QUIC pull");
    assert!(
        pull.status.success(),
        "QUIC pull failed: {}",
        String::from_utf8_lossy(&pull.stderr)
    );
    assert_eq!(
        fs::read(dest.join("file.txt")).expect("pulled file"),
        b"relayed through the front process\n"
    );
    assert_eq!(
        proc_status_field(parent, "Threads"),
        Some(1),
        "daemon parent must stay single-threaded after serving a QUIC session"
    );
}
