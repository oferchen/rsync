//! A daemon SENDER must unlink its sources when the client sends
//! `--remove-source-files`.
//!
//! ```c
//! /* options.c:3153-3156 - server_options() */
//! if (remove_source_files == 1)
//!         args[ac++] = "--remove-source-files";
//! else if (remove_source_files)
//!         args[ac++] = "--remove-sent-files";
//! ```
//!
//! The option is forwarded to the SERVER because the removal happens where the
//! files are: on the sender, after the receiver acknowledges each file. On a
//! daemon pull the sender is the daemon, so the whole feature lives behind the
//! daemon's own parse of this argument.
//!
//! oc's `--server` argv parser (the remote-shell transport) recognised it; the
//! daemon's long-form parser did not, and that parser only ever ignores an
//! option it does not know - so `--remove-source-files` over `rsync://` copied
//! the files and silently left every source in place, exiting 0. The same
//! command over ssh removed them. A parser-level assertion cannot see that
//! split, because both parsers can be "correct" about a field nothing reads;
//! this test asserts the files are GONE from the module after a real transfer.
//!
//! WHY THIS WAITS INSTEAD OF STATTING IMMEDIATELY. The unlink is DEFERRED, not
//! done inline at send time: the sender marks the entry pending and only
//! removes the file once the receiver confirms the commit with `MSG_SUCCESS`
//! (`sender.c:131-182 successful_send()`, mirrored at
//! `generator/transfer/transfer_loop.rs:1145-1157`). The client process can
//! therefore exit before the daemon's removal has landed, so reading the
//! filesystem the instant `status()` returns is an UNSYNCHRONISED read of a
//! peer's work - it measures which process won a race, not what the daemon
//! did. Measured: it passed on macOS and on the musl cell and failed on the
//! Linux `--all-features` cell, where the client exits sooner.
//!
//! The bounded wait below is a synchronisation barrier, not a retry: nothing is
//! re-attempted, and a daemon that never removes the file still fails the test
//! when the deadline expires.
//!
//! Skip conditions (the test passes with a printed reason):
//! - Loopback TCP is unavailable.

#![cfg(unix)]

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
}

/// Starts a daemon and waits until its port answers.
///
/// `stdin` is `/dev/null` deliberately: with an inherited terminal or pipe the
/// daemon takes its single-connection inetd path and never listens, and the
/// client then reports "connection refused" instead of the behaviour under
/// test.
fn spawn_daemon(binary: &Path, conf: &Path, port: u16) -> Child {
    let child = Command::new(binary)
        .arg("--daemon")
        .arg("--no-detach")
        .arg(format!("--config={}", conf.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return child;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

/// Pulls `mod/` from a loopback daemon with `--remove-source-files` and reports
/// (transfer succeeded, source still present).
///
/// The module is deliberately NOT `read only`: upstream refuses a source
/// removal on a read-only module, so a read-only fixture would report the same
/// "source still there" as the bug and prove nothing.
fn pull_removing_sources(spelling: &str) -> Option<(bool, bool, bool)> {
    let root = tempfile::tempdir().expect("temp dir");
    let port = free_port()?;
    let module = root.path().join("mod");
    fs::create_dir_all(&module).expect("module dir");
    fs::write(module.join("f.txt"), b"payload\n").expect("module file");
    let dest = root.path().join("dest");
    fs::create_dir_all(&dest).expect("dest dir");

    let conf = format!(
        "port = {port}\n\
         use chroot = no\n\
         log file = {log}\n\
         \n\
         [m]\n\
         \tpath = {module}\n\
         \tread only = no\n",
        log = root.path().join("daemon.log").display(),
        module = module.display(),
    );
    let conf_path = root.path().join("rsyncd.conf");
    fs::write(&conf_path, conf).expect("config");

    let binary = oc_binary();
    let mut daemon = spawn_daemon(&binary, &conf_path, port);
    let status = Command::new(&binary)
        .args([
            "-q",
            "-r",
            spelling,
            &format!("rsync://127.0.0.1:{port}/m/"),
            &format!("{}/", dest.display()),
        ])
        .status()
        .expect("run client");
    // Observe BEFORE killing the daemon: it is the daemon that performs the
    // removal, so tearing it down first would guarantee the very absence of
    // work this test is trying to detect.
    let source_remains = source_still_present_after_settling(&module.join("f.txt"));

    let _ = daemon.kill();
    let _ = daemon.wait();

    Some((
        status.success(),
        source_remains,
        dest.join("f.txt").exists(),
    ))
}

/// Waits for the daemon's deferred source removal to land, and reports whether
/// the file is STILL there once the wait is over.
///
/// The deadline is generous because it bounds a failure, not a success: a
/// daemon that removes the file returns as soon as it has, and a daemon that
/// never removes it pays the full wait exactly once and then fails the test.
fn source_still_present_after_settling(source: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if !source.exists() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    source.exists()
}

/// Both spellings name the same option in `parse_arguments()`;
/// `--remove-sent-files` is upstream's deprecated alias (options.c:744-745) and
/// is what an older client sends, so a daemon that honours only the modern
/// spelling still drops the request from half its peers.
#[test]
fn daemon_sender_removes_sources_for_both_spellings() {
    for spelling in ["--remove-source-files", "--remove-sent-files"] {
        let Some((succeeded, source_remains, delivered)) = pull_removing_sources(spelling) else {
            println!("SKIP: no loopback port available");
            return;
        };
        assert!(succeeded, "{spelling}: the pull itself must succeed");
        assert!(
            delivered,
            "{spelling}: the file must reach the destination - without this the \
             source-removal assertion below would pass on a transfer that never ran"
        );
        assert!(
            !source_remains,
            "{spelling}: the daemon sender must unlink the source after the \
             receiver acknowledges it (options.c:3153-3156)"
        );
    }
}
