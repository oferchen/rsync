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
//! WHY THIS CELL IS FEATURE-SENSITIVE, AND WHAT A RED HERE MEANS.
//!
//! The daemon worker runs behind a seccomp filter whose allowlist is built by
//! `worker_seccomp_allowlist()` (`daemon/sections/seccomp.rs:152`). That list
//! carries `SYS_unlinkat` and has no `SYS_unlink`. `std::fs::remove_file` calls
//! glibc `unlink()`, which is `__NR_unlink` on x86_64, so the kernel refuses
//! the removal outright:
//!
//! ```text
//! unlink(".../mod/f.txt") = -1 EPERM (Operation not permitted)
//! rsync: [sender] sender failed to remove <path>: Operation not permitted (1)
//! ```
//!
//! The filter is gated `cfg(all(target_os = "linux", feature =
//! "daemon-seccomp"))`, which is why the cell splits by FEATURE SET and not by
//! platform or by speed: a `--no-default-features` build never compiles the
//! filter in and the unlink succeeds, while `--all-features` engages it and the
//! unlink is refused. On a non-Linux host the filter does not exist at all, so
//! a green run there says nothing about CI.
//!
//! A red here is therefore a REFUSED SYSCALL, not a lost race, and it is
//! permanent - no amount of waiting changes it. Verify any green with the
//! filter provably engaged (the daemon log line `seccomp BPF filter engaged`),
//! using `OC_RSYNC_NO_SECCOMP=1` as the control leg.
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
    //
    // Observed, not waited for. A bounded wait here would be a waiver: the
    // removal either happens or is refused outright, and a wait cannot turn a
    // refusal into a pass - it can only spend the deadline and report the same
    // failure later, while hiding the shape of it from anyone reading the test.
    let source_remains = module.join("f.txt").exists();

    let _ = daemon.kill();
    let _ = daemon.wait();

    Some((
        status.success(),
        source_remains,
        dest.join("f.txt").exists(),
    ))
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
