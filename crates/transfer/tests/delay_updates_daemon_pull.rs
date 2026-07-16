//! Regression coverage for `--delay-updates` on the receiver side of a daemon
//! pull (`oc-rsync rsync://host/mod/ DEST`).
//!
//! # Background
//!
//! `--delay-updates` is a plain receiver-side option (upstream `options.c:777`,
//! no `am_sender` gate). Upstream forwards `--delay-updates` to the remote only
//! on a push (`options.c:2886-2892`, `partial_dir && am_sender`); on a pull the
//! local client IS the receiver and the flag never travels over the wire, so
//! whichever side plays the receiver must apply it itself (`receiver.c:656`,
//! `1029-1050` - stage under the partial dir, then rename in the phase-2 sweep).
//!
//! On a daemon pull the client-receiver's `ServerConfig` is assembled by
//! `crates/core/src/client/remote/daemon_transfer/orchestration/server_config.rs::build_server_config_for_receiver`.
//! That builder threads the other long-form-only receiver flags (`--existing`,
//! `--list-only`, `--prune-empty-dirs`) onto the local half but historically
//! never set `write.delay_updates`, so the client-receiver updated every file
//! in place and the atomic phase-2 rename never fired - `--delay-updates` was
//! silently a no-op on this one transport/direction.
//!
//! # Why this matters (Rule 9)
//!
//! The contract is: an updated file is staged into the `.~tmp~/` partial dir
//! and only moved into place during the end-of-transfer sweep, never written
//! in place. A pure content check cannot pin this - the default (non-inplace)
//! receiver also uses a temp file and rename, so the destination comes out
//! correct with or without `--delay-updates`. The two paths differ only in
//! whether the `.~tmp~/` staging directory is engaged.
//!
//! This test pins the difference deterministically. It pre-creates an empty
//! `DEST/.~tmp~/` directory before the pull:
//!
//! - With `--delay-updates` honored, the receiver stages the update into
//!   `DEST/.~tmp~/payload.bin`, renames it into place in the phase-2 sweep, and
//!   removes the now-empty staging directory - so `.~tmp~` is gone afterwards.
//! - When the flag is dropped, the receiver never touches `.~tmp~`; the
//!   pre-seeded staging directory survives untouched. That surviving directory
//!   is the regression signal.
//!
//! The destination file is pre-seeded stale (different content and size, and a
//! backdated mtime) so rsync's quick-check cannot skip the transfer: the
//! delayed rename must actually run for the assertions to be meaningful.
//!
//! # Platform gate
//!
//! `#![cfg(unix)]` - matches the sibling daemon-spawning tests
//! (`uts_15_e_daemon_pull_write_batch.rs`, `uts_nextest_daemon_delete_stats.rs`);
//! the `use chroot = false` toggle needs Unix process semantics.
//!
//! # Skip semantics
//!
//! Self-skips (prints `skipping:` and returns) when the workspace `oc-rsync`
//! binary cannot be located, a loopback port cannot be allocated, or the daemon
//! fails to start. A non-zero exit, an un-updated destination, or a surviving
//! `.~tmp~` directory are real regressions.
//!
//! # Upstream References
//!
//! - `options.c:777` - `--delay-updates` is a plain option, not transport-gated.
//! - `options.c:2886-2892` - the flag is forwarded to the remote only on a push
//!   (`partial_dir && am_sender`); a pull applies it on the local receiver.
//! - `receiver.c:656,1029-1050` - stage under the partial dir, rename in phase 2.

#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use tempfile::{TempDir, tempdir};

/// Write an `rsyncd.conf` exposing one read-only module rooted at
/// `module_root`. `use chroot = false` keeps the unprivileged test process
/// from needing `CAP_SYS_CHROOT`.
fn write_daemon_config(
    config_path: &Path,
    pid_path: &Path,
    log_path: &Path,
    module_name: &str,
    module_root: &Path,
) -> io::Result<()> {
    let body = format!(
        "pid file = {pid}\n\
         log file = {log}\n\
         use chroot = false\n\
         max connections = 4\n\
         \n\
         [{module}]\n\
         path = {root}\n\
         comment = delay-updates daemon-pull regression\n\
         read only = true\n\
         list = true\n",
        pid = pid_path.display(),
        log = log_path.display(),
        module = module_name,
        root = module_root.display(),
    );
    fs::write(config_path, body)
}

/// Guard that kills the daemon child on drop so a panicking test never leaks
/// the listener.
struct DaemonGuard {
    child: Child,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn `oc-rsync --daemon` on a free loopback port against `config_path` and
/// wait until it accepts connections.
fn spawn_oc_daemon(oc_bin: &Path, config_path: &Path) -> io::Result<(DaemonGuard, u16)> {
    let (child, port) = test_support::spawn_daemon_on_free_port(|port| {
        Command::new(oc_bin)
            .arg("--daemon")
            .arg("--no-detach")
            .arg("--port")
            .arg(port.to_string())
            .arg("--config")
            .arg(config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    })?;
    Ok((DaemonGuard { child }, port))
}

/// Drive one `oc-rsync` invocation and return `(status, stdout, stderr)`.
fn run_oc_rsync_capture(
    bin: &Path,
    args: &[&OsStr],
) -> io::Result<(std::process::ExitStatus, String, String)> {
    let output = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    Ok((
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

/// Per-test scratch state: tempdir plus daemon log/pid/config paths.
struct Scratch {
    _tmp: TempDir,
    root: PathBuf,
    config: PathBuf,
    log: PathBuf,
    pid: PathBuf,
}

impl Scratch {
    fn new() -> Option<Self> {
        let tmp = tempdir().ok()?;
        let root = tmp.path().to_path_buf();
        Some(Self {
            config: root.join("rsyncd.conf"),
            log: root.join("rsyncd.log"),
            pid: root.join("rsyncd.pid"),
            root,
            _tmp: tmp,
        })
    }
}

/// A daemon pull with `--delay-updates` must stage the updated file through the
/// `.~tmp~/` partial dir and rename it in the end-of-transfer sweep, not update
/// it in place. Proven by pre-seeding an empty `DEST/.~tmp~/`: when the flag is
/// honored the receiver consumes and removes it; when the flag is dropped the
/// staging dir is never touched and survives.
#[test]
fn daemon_pull_delay_updates_stages_through_tmp_dir() {
    let Some(oc_bin) = test_support::locate_workspace_binary("oc-rsync") else {
        eprintln!("skipping: oc-rsync binary not found in target/");
        return;
    };
    let Some(scratch) = Scratch::new() else {
        eprintln!("skipping: tempdir allocation failed");
        return;
    };

    let module_root = scratch.root.join("source");
    let dest_dir = scratch.root.join("dest");

    // Source content the pull must install. Larger than the stale destination
    // so quick-check cannot skip on matching size.
    fs::create_dir_all(&module_root).expect("create module root");
    let fresh: Vec<u8> = (0..64 * 1024).map(|i| (i % 251) as u8).collect();
    fs::write(module_root.join("payload.bin"), &fresh).expect("seed source payload");

    // Stale destination: different content/size and a backdated mtime so the
    // delayed rename is forced to run rather than quick-check-skipped.
    fs::create_dir_all(&dest_dir).expect("create destination dir");
    fs::write(dest_dir.join("payload.bin"), b"stale").expect("seed stale dest payload");
    let old = filetime::FileTime::from_unix_time(946_684_800, 0); // 2000-01-01
    filetime::set_file_mtime(dest_dir.join("payload.bin"), old).expect("backdate dest");

    // The `.~tmp~/` staging directory the receiver must engage under
    // `--delay-updates`. Pre-created empty: honoring the flag consumes and
    // removes it; dropping the flag leaves it untouched.
    let staging_dir = dest_dir.join(".~tmp~");
    fs::create_dir(&staging_dir).expect("pre-create staging dir");

    write_daemon_config(
        &scratch.config,
        &scratch.pid,
        &scratch.log,
        "pullmod",
        &module_root,
    )
    .expect("write daemon config");

    let (_daemon, port) = match spawn_oc_daemon(&oc_bin, &scratch.config) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: could not start oc-rsync --daemon: {e}");
            return;
        }
    };

    let src_url = OsString::from(format!("rsync://127.0.0.1:{port}/pullmod/"));
    let mut dest_arg = dest_dir.clone().into_os_string();
    dest_arg.push("/");

    let args: &[&OsStr] = &[
        OsStr::new("--recursive"),
        OsStr::new("--times"),
        OsStr::new("--delay-updates"),
        &src_url,
        &dest_arg,
    ];

    let (status, stdout, stderr) =
        run_oc_rsync_capture(&oc_bin, args).expect("spawn oc-rsync client (pull + delay-updates)");

    assert!(
        status.success(),
        "daemon pull --delay-updates exited non-zero: {status:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );

    // The delayed rename must have installed the source content.
    assert_eq!(
        fs::read(dest_dir.join("payload.bin")).expect("read destination payload"),
        fresh,
        "the phase-2 rename must install the pulled content at the destination",
    );

    // The regression signal: with `--delay-updates` honored the receiver stages
    // the update into `.~tmp~/payload.bin` and removes the emptied staging dir
    // during the sweep. When the flag is dropped on the client-receiver the
    // partial dir is never engaged, so the pre-seeded `.~tmp~/` survives here.
    assert!(
        !staging_dir.exists(),
        "the pre-seeded .~tmp~/ staging directory must be consumed and removed by \
         a honored --delay-updates pull; its survival means the flag was dropped \
         and the file was updated in place at {}",
        dest_dir.display(),
    );
}
