//! A negative daemon `max verbosity` caps every output level to 0.
//!
//! ```c
//! /* options.c:547-573 limit_output_verbosity() - the limit rows come from
//!    for (j = 0; j <= level; j++), which runs zero times when level < 0. */
//! ```
//!
//! clientserver.c:1232 calls `limit_output_verbosity(lp_max_verbosity(module_id))`
//! and `max verbosity` is an `INTEGER` parameter (daemon-parm.txt:51), so
//! `max verbosity = -1` leaves every limit at its `memset` 0. A cap of 0 still
//! runs row 0, whose only word is `NONREG` (options.c:257), so the
//! `skipping non-regular file` notice survives `max verbosity = 0` and is
//! dropped only below it. oc skipped the clamp for a value that did not fit a
//! `u8`, so a negative cap left `NONREG` at 1.
//!
//! The cell pushes `/dev/null` with `-D` to a non-root daemon: the receiver
//! cannot `mknod` (generator.c:2073 needs `am_root`), so it reaches the
//! `INFO_GTE(NONREG, 1)` notice at generator.c:2150-2155 and sends it to the
//! client. The client is the same binary for every row, so only the daemon's
//! cap can change what it prints.
//!
//! Skip conditions (test passes with a printed reason, except under
//! `OC_RSYNC_UPSTREAM_COMPAT=1`, where a missing oracle fails it):
//! - Running as root: the receiver creates the device and prints nothing.
//! - Loopback TCP is unavailable.
//! - The oracle cell additionally needs a built upstream 3.5.2 binary.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const NOTICE: &str = "skipping non-regular file \"null\"";

/// `(max verbosity, client verbosity, whether the client prints the notice)`.
const CASES: &[(i32, &str, bool)] = &[
    (-1, "-q", false),
    (-1, "-vv", false),
    (-1, "--info=nonreg", false),
    // CONTROL: a cap of 0 keeps row 0, so the clamp alone cannot explain a
    // missing notice above.
    (0, "-vv", true),
    (1, "-vv", true),
];

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn upstream_binary() -> Option<PathBuf> {
    test_support::require_upstream_rsync(test_support::UpstreamVersion::V3_5_2)
        .map(|upstream| upstream.binary().to_path_buf())
}

fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "0")
}

/// Serves the daemon in inetd mode on the listener reserved for `port`, so a
/// client connect queues until its session starts: no readiness wait.
fn spawn_daemon(binary: &Path, conf: &Path, port: u16) -> test_support::inetd_server::InetdServer {
    let command = test_support::inetd_server::InetdCommand::discarding_stderr(binary)
        .expect("open /dev/null")
        .arg(format!("--config={}", conf.display()));
    test_support::inetd_server::InetdServer::start_reserved(port, command).expect("start daemon")
}

/// Pushes `/dev/null` to a module capped at `max_verbosity` and returns what
/// the client printed, or `None` when no loopback port is available.
fn client_output(daemon: &Path, max_verbosity: i32, verbosity: &str) -> Option<String> {
    let port = test_support::inetd_server::reserve_port().ok()?;
    let root = tempfile::tempdir().expect("temp dir");
    let module = root.path().join("mod");
    fs::create_dir(&module).expect("module dir");
    let conf = root.path().join("rsyncd.conf");
    fs::write(
        &conf,
        format!(
            "use chroot = no\n\n[m]\n\tpath = {}\n\tread only = no\n\tmax verbosity = {max_verbosity}\n",
            module.display()
        ),
    )
    .expect("config");
    let server = spawn_daemon(daemon, &conf, port);
    let output = Command::new(oc_binary())
        .args([
            "-D",
            verbosity,
            "/dev/null",
            &format!("rsync://127.0.0.1:{port}/m/"),
        ])
        .output()
        .expect("run client");
    server.finish().expect("daemon session ends");
    assert!(output.status.success(), "client failed: {output:?}");
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn assert_table(daemon: &Path, label: &str) {
    if is_root() {
        println!("SKIP: running as root, the receiver creates the device");
        return;
    }
    for &(max_verbosity, verbosity, expected) in CASES {
        let Some(stdout) = client_output(daemon, max_verbosity, verbosity) else {
            println!("SKIP: no loopback port available");
            return;
        };
        assert_eq!(
            stdout.contains(NOTICE),
            expected,
            "{label}: `max verbosity = {max_verbosity}` with client `{verbosity}` printed {stdout:?}",
        );
    }
}

#[test]
fn oc_daemon_negative_max_verbosity_drops_the_nonreg_notice() {
    assert_table(&oc_binary(), "oc");
}

/// CROSS-IMPLEMENTATION: the expected column is upstream's behaviour, so
/// assert it against upstream rather than trusting the transcription.
#[test]
fn upstream_daemon_negative_max_verbosity_drops_the_nonreg_notice() {
    let Some(upstream) = upstream_binary() else {
        return;
    };
    assert_table(&upstream, "upstream 3.5.2");
}
