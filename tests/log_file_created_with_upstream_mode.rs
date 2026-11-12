//! A newly created log file carries upstream's `0644`, on both log sinks.
//!
//! Upstream opens every log file through one call:
//!
//! ```c
//! logfile_fp = fdopen(open_no_attacker_symlinks(logfile_name,
//!                     O_WRONLY|O_APPEND|O_CREAT, 0644), "a");
//! ```
//!
//! (`log.c:169-170`, reached by `log_init()` for the client `--log-file` and by
//! the daemon for `log file =`). Two things are being pinned here, and one test
//! shape settles both:
//!
//! - the **mode**: `0644`, not the `0666` an ordinary `open(2)` default carries;
//! - the **routing**: an open that passes no mode at all - which is what
//!   `OpenOptions::new().create(true).append(true)` does - cannot produce
//!   `0644` once the umask is out of the way.
//!
//! The umask is what makes that discriminating, and it has to be pinned or the
//! assertion is worthless: `umask(2)` can only *clear* bits, so under the usual
//! `022` a `0666` request lands as `0644` too and the two spellings become
//! indistinguishable. Every child below is therefore spawned with its umask
//! pinned to `0`, where `0666` stays `0666` and only a genuine `0644` request
//! reads back as `0644`. [`pinning_the_child_umask_takes_effect`] is the
//! control for that claim.
//!
//! Upstream reaches the same fixed point by a different route - it wraps the
//! open in `umask(022 | orig_umask)` (`log.c:163`) so a permissive umask can
//! never yield a group- or world-writable log. Requesting `0644` needs no such
//! dance, because `0644` holds no group or other write bit for a umask to have
//! to clear.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use test_support::oc_daemon::{OcDaemon, OcDaemonBuilder};

/// upstream: log.c:170 - the literal mode argument at the single log-file open.
const UPSTREAM_LOG_FILE_MODE: u32 = 0o644;

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Builds a command whose child runs under umask `0`.
///
/// Pinned on the child rather than by mutating this process's umask around the
/// spawn: `umask(2)` is process-global, and the test harness runs cases in
/// parallel threads, so an in-process pin would leak into whatever else is
/// creating files at that moment. `pre_exec` runs after `fork(2)` and before
/// `exec(2)`, where only async-signal-safe calls are permitted - `umask(2)` is
/// one of them.
fn command_with_open_umask(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    // SAFETY-equivalent reasoning lives in the doc comment above; `pre_exec` is
    // safe to use here because `umask(2)` is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            libc::umask(0);
            Ok(())
        });
    }
    command
}

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path)
        .unwrap_or_else(|error| panic!("stat {}: {error}", path.display()))
        .permissions()
        .mode()
        & 0o777
}

fn free_port() -> Option<u16> {
    test_support::inetd_server::reserve_port().ok()
}

/// Validates the instrument the two assertions below depend on.
///
/// If the `pre_exec` pin silently did not apply, the children would run under
/// the ambient umask - typically `022`, which masks a `0666` request down to
/// `0644` and makes both mode assertions pass no matter which constant the
/// production code carries. This is exactly that failure mode, checked
/// directly: the child reports its own umask, and it must be `0`.
#[test]
fn pinning_the_child_umask_takes_effect() {
    let output = command_with_open_umask("/bin/sh")
        .args(["-c", "umask"])
        .output()
        .expect("run /bin/sh");
    assert!(output.status.success(), "shell failed: {:?}", output.status);
    let reported = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(
        u32::from_str_radix(&reported, 8).expect("octal umask"),
        0,
        "the child umask was not pinned (reported {reported:?}), so a 0666 \
         request would be masked to 0644 and the mode assertions would hold \
         vacuously"
    );
}

/// upstream: log.c:170 - the client `--log-file` is created `0644`.
#[test]
fn client_log_file_is_created_with_upstream_mode() {
    let tmp = TempDir::new().expect("tempdir");
    let src = tmp.path().join("src");
    fs::create_dir_all(&src).expect("create src");
    fs::write(src.join("payload"), b"contents\n").expect("write source");
    let log = tmp.path().join("client.log");

    let status = command_with_open_umask(oc_binary())
        .arg("-a")
        .arg(format!("--log-file={}", log.display()))
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", tmp.path().join("dst").display()))
        .status()
        .expect("run oc-rsync");
    assert!(status.success(), "transfer failed: {status:?}");

    assert_eq!(
        mode_of(&log),
        UPSTREAM_LOG_FILE_MODE,
        "the client --log-file must be created with upstream's 0644"
    );
}

/// upstream: log.c:170 - the daemon's log sink takes the same open, so it takes
/// the same mode. The two sinks are separate call sites in oc, and a correction
/// applied to only one of them leaves the other on `0666`.
///
/// The path is given as `--log-file` rather than a `log file =` config line
/// because only the option is consumed at daemon startup: the config directive
/// is applied per connection, when the selected module reopens the sink, so a
/// config-driven fixture would have to drive a whole client session to observe
/// the same open. Both spellings land on the same `open_log_sink`.
#[test]
fn daemon_log_file_is_created_with_upstream_mode() {
    let Some(port) = free_port() else {
        eprintln!("skipping: loopback TCP unavailable");
        return;
    };
    let tmp = TempDir::new().expect("tempdir");
    let module = tmp.path().join("module");
    fs::create_dir_all(&module).expect("create module root");
    let log = tmp.path().join("daemon.log");
    let conf = tmp.path().join("oc-rsyncd.conf");
    fs::write(
        &conf,
        format!(
            "port = {port}\nuse chroot = no\n\n[data]\n\tpath = {}\n\tread only = yes\n",
            module.display()
        ),
    )
    .expect("write config");

    // The forked daemon takes this process's umask at the fork, so it is
    // pinned to 0 around the spawn alone; nextest runs each test in a process
    // of its own.
    // SAFETY: umask(2) only swaps the process file-mode mask and cannot fail.
    let previous = unsafe { libc::umask(0) };
    let daemon = OcDaemon::builder(&conf)
        .arg(format!("--log-file={}", log.display()))
        .reserved_port(port)
        .and_then(OcDaemonBuilder::spawn);
    // SAFETY: as above, restoring the mask this process had.
    unsafe { libc::umask(previous) };
    let daemon = daemon.expect("spawn daemon");
    // The daemon opens `--log-file` at startup, before it serves anything, so
    // once a module listing has been served the file exists.
    let listing = Command::new(oc_binary())
        .arg(format!("rsync://127.0.0.1:{port}/"))
        .output()
        .expect("list modules");
    assert!(
        listing.status.success(),
        "the daemon must serve its listing"
    );
    let appeared = log.exists();
    let mode = appeared.then(|| mode_of(&log));
    drop(daemon);

    assert!(appeared, "the daemon never created its log file");
    assert_eq!(
        mode,
        Some(UPSTREAM_LOG_FILE_MODE),
        "the daemon `log file` must be created with upstream's 0644"
    );
}
