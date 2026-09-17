//! Cross-implementation regression: oc-rsync RECEIVER pulling from a REAL
//! upstream rsync 3.5.0 SENDER when the sender declines a file it cannot open.
//!
//! # Why this test exists
//!
//! The sibling `msg_no_send_unsendable_file.rs` drives oc-rsync against oc-rsync
//! and places its single unreadable file so it sorts LAST. Against oc's own
//! sender the decline (`MSG_NO_SEND`) then always names the file at the FRONT of
//! the receiver's in-flight window, so the FIFO-positional match succeeds and
//! the out-of-order path is never exercised.
//!
//! A real upstream sender declines a file the instant it fails to open it, and
//! (as observed on the wire) can emit `MSG_NO_SEND` for a LATER index while an
//! EARLIER request is still at the receiver's window front. oc's receiver then
//! saw `d.ndx != awaited` and aborted the whole transfer as a desync, dropping
//! EVERY readable file (exit 23, `request stream desynchronised`). Upstream's
//! generator is NDX-addressed and retires the declined entry by index regardless
//! of order (`io.c:1207-1256` `got_flist_entry_status(FES_NO_SEND, ndx)`), so it
//! skips only the one file and lands the rest.
//!
//! # Fixture design
//!
//! The unreadable file is named to sort in the MIDDLE. rsync transfers in sorted
//! order, so a middle-sorting decline is what makes the declined index land
//! out-of-window-order: readable requests both before and after it are still
//! outstanding when the decline arrives. An unreadable file sorting first or
//! last would reproduce only the in-window case the oc-to-oc test already
//! covers.
//!
//! # The contract (Rule 9)
//!
//! A source file the sender cannot open is skipped, the rest of the transfer
//! completes, and the run reports the I/O error with exit 23 - byte-for-byte the
//! outcome a real upstream RECEIVER produces from the same daemon. The bug this
//! pins is not a wrong byte; it is DROPPED FILES plus a spurious desync abort.
//! Exit 23 is correct and preserved (upstream also exits 23 - the sender set
//! `io_error`); the defect was the data loss, not the code.
//!
//! # Gating
//!
//! Self-skips when `OC_RSYNC_UPSTREAM_COMPAT` is unset (keeps the standard PR
//! nextest cell costless), when no upstream rsync 3.5.0 binary is available, and
//! when the running user can read a mode-000 file (root / `CAP_DAC_OVERRIDE`),
//! which is the real precondition and is probed behaviourally. Once the gate is
//! set and a 3.5.0 binary is present the test must RUN: a hang, a wrong exit
//! code, a desync abort, or a missing readable file is a real regression.
//!
//! # Upstream references
//!
//! - `sender.c:669,723,751` - the three `MSG_NO_SEND` emitters; each `continue`s
//!   without writing a response.
//! - `io.c:1809-1818` -> `got_flist_entry_status(FES_NO_SEND, ndx)` retires the
//!   declined entry by index, not by any window front (`io.c:1207-1256`).
//! - `sender.c:668,719` - `io_error |= IOERR_GENERAL`, which travels via
//!   `MSG_IO_ERROR` and makes the run exit 23.

#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};
use test_support::{UpstreamVersion, require_upstream_rsync, upstream_compat_enabled};

/// Upper bound on the client run. The transfer moves a few kilobytes over
/// loopback; anything approaching this is a hang, not slowness.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// Upstream `RERR_PARTIAL`: "some files could not be transferred".
const EXIT_PARTIAL_TRANSFER: i32 = 23;

/// Readable sources. `m_unreadable.bin` sorts between them so the decline lands
/// out-of-window-order - see the fixture note in the module docs.
const READABLE: [&str; 2] = ["a_readable.bin", "z_readable.bin"];
const UNREADABLE: &str = "m_unreadable.bin";

/// Kills the daemon child on drop so a panicking test never leaks the listener.
struct DaemonGuard {
    child: Child,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Write an `rsyncd.conf` exposing one read-only module rooted at
/// `module_root`. `use chroot = false` keeps the unprivileged test process from
/// needing `CAP_SYS_CHROOT`.
fn write_daemon_config(
    config_path: &Path,
    pid_path: &Path,
    log_path: &Path,
    module_root: &Path,
) -> io::Result<()> {
    // `lock file` must be writable: upstream's default `/var/run/rsyncd.lock`
    // is out of reach for the unprivileged test process (exit 5), so anchor it
    // beside the config in the tempdir.
    let lock_path = config_path.with_file_name("rsyncd.lock");
    let body = format!(
        "pid file = {pid}\n\
         lock file = {lock}\n\
         log file = {log}\n\
         use chroot = false\n\
         max connections = 4\n\
         \n\
         [nosendmod]\n\
         path = {root}\n\
         comment = upstream-compat MSG_NO_SEND out-of-order regression\n\
         read only = true\n\
         list = true\n",
        pid = pid_path.display(),
        lock = lock_path.display(),
        log = log_path.display(),
        root = module_root.display(),
    );
    fs::write(config_path, body)
}

/// Spawn `<upstream> --daemon` on a free port against `config_path`.
fn spawn_upstream_daemon(rsync_bin: &Path, config_path: &Path) -> io::Result<(DaemonGuard, u16)> {
    let (child, port) = test_support::spawn_daemon_on_free_port(|port| {
        Command::new(rsync_bin)
            .arg("--daemon")
            .arg("--no-detach")
            .arg("--address=127.0.0.1")
            .arg(format!("--port={port}"))
            .arg(format!("--config={}", config_path.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    })?;
    Ok((DaemonGuard { child }, port))
}

/// Run one `oc-rsync` invocation under a deadline, returning `None` if it had to
/// be killed. Both pipes are drained by dedicated threads before waiting so a
/// full pipe buffer cannot masquerade as the hang under test.
fn run_under_deadline(bin: &Path, args: &[&OsStr]) -> io::Result<Option<(ExitStatus, String)>> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");
    let out_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr.read_to_string(&mut buf);
        buf
    });

    let deadline = Instant::now() + RUN_TIMEOUT;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };

    let combined = format!(
        "{}{}",
        out_reader.join().unwrap_or_default(),
        err_reader.join().unwrap_or_default()
    );
    Ok(status.map(|s| (s, combined)))
}

/// Whether this user is subject to file permissions at all. Probed rather than
/// derived from a uid: root and any holder of `CAP_DAC_OVERRIDE` read a mode-000
/// file happily, and the fixture's precondition is exactly "opening this file
/// fails".
fn mode_000_is_unreadable(probe: &Path) -> bool {
    fs::write(probe, b"probe").is_ok()
        && fs::set_permissions(probe, fs::Permissions::from_mode(0o000)).is_ok()
        && fs::File::open(probe).is_err()
}

/// Seed the module root. Every file gets a distinct size so a quick-check cannot
/// skip it on a re-run.
fn seed_module(module_root: &Path) {
    fs::create_dir_all(module_root).expect("create module root");
    for (i, name) in READABLE.iter().enumerate() {
        let body: Vec<u8> = (0..(4096 + i * 512)).map(|b| (b % 251) as u8).collect();
        fs::write(module_root.join(name), &body).expect("seed readable source");
    }
    let body: Vec<u8> = (0..8192).map(|b| (b % 241) as u8).collect();
    fs::write(module_root.join(UNREADABLE), &body).expect("seed unreadable source");
}

/// A source file the upstream sender cannot open must be skipped out of window
/// order, the readable files must still transfer, and oc must exit 23 - matching
/// what a real upstream receiver produces from the same daemon.
#[test]
fn oc_receiver_survives_upstream_out_of_order_no_send() {
    if !upstream_compat_enabled() {
        return;
    }

    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_0).expect(
        "OC_RSYNC_UPSTREAM_COMPAT=1 selected this test but upstream rsync 3.5.0 is not \
         installed; build it with `bash tools/ci/run_interop.sh` or point \
         OC_RSYNC_UPSTREAM_BIN_3_5_0 at it",
    );
    let oc_bin = test_support::oc_rsync_bin();

    let tmp = tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();

    if !mode_000_is_unreadable(&root.join("perm_probe")) {
        eprintln!("skipping: this user can read a mode-000 file (root or CAP_DAC_OVERRIDE)");
        return;
    }

    let module_root = root.join("source");
    seed_module(&module_root);
    fs::set_permissions(
        module_root.join(UNREADABLE),
        fs::Permissions::from_mode(0o000),
    )
    .expect("make source unreadable");

    let config = root.join("rsyncd.conf");
    write_daemon_config(
        &config,
        &root.join("rsyncd.pid"),
        &root.join("rsyncd.log"),
        &module_root,
    )
    .expect("write daemon config");

    let (_daemon, port) = spawn_upstream_daemon(upstream.binary(), &config)
        .expect("start upstream rsync 3.5.0 --daemon");

    // Control arm: a real upstream receiver against the same daemon. This is the
    // oracle the oc receiver must match - it lands both readable files and exits
    // 23. It also proves the fixture genuinely triggers a decline rather than
    // silently transferring everything.
    let up_dest = root.join("dest_upstream");
    fs::create_dir_all(&up_dest).expect("create upstream dest");
    let up_src = OsString::from(format!("rsync://127.0.0.1:{port}/nosendmod/"));
    let mut up_dest_arg = up_dest.clone().into_os_string();
    up_dest_arg.push("/");
    let up_out = upstream
        .command()
        .arg("--recursive")
        .arg("--times")
        .arg(&up_src)
        .arg(&up_dest_arg)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn upstream receiver control arm");
    assert_eq!(
        up_out.status.code(),
        Some(EXIT_PARTIAL_TRANSFER),
        "upstream receiver control arm must exit {EXIT_PARTIAL_TRANSFER}; stderr:\n{}",
        String::from_utf8_lossy(&up_out.stderr),
    );
    for name in READABLE {
        assert!(
            up_dest.join(name).is_file(),
            "control: upstream receiver must land {name}"
        );
    }

    // The arm under test: oc-rsync receiver pulling from the upstream sender.
    let oc_dest = root.join("dest_oc");
    fs::create_dir_all(&oc_dest).expect("create oc dest");
    let oc_src = OsString::from(format!("rsync://127.0.0.1:{port}/nosendmod/"));
    let mut oc_dest_arg = oc_dest.clone().into_os_string();
    oc_dest_arg.push("/");
    let args = vec![
        OsStr::new("--recursive"),
        OsStr::new("--times"),
        oc_src.as_os_str(),
        oc_dest_arg.as_os_str(),
    ];

    let outcome = run_under_deadline(&oc_bin, &args).expect("spawn oc-rsync client");
    let Some((status, output)) = outcome else {
        panic!(
            "oc-rsync receiver did not exit within {RUN_TIMEOUT:?} pulling from an upstream \
             3.5.0 sender that declined a file"
        );
    };

    // The desync abort is the exact pre-fix failure. If it recurs, oc dropped
    // every file - name it explicitly so a regression is unambiguous.
    assert!(
        !output.contains("request stream desynchronised"),
        "oc receiver must not abort as desync on an out-of-order upstream decline; \
         output:\n{output}"
    );

    assert_eq!(
        status.code(),
        Some(EXIT_PARTIAL_TRANSFER),
        "oc receiver must exit {EXIT_PARTIAL_TRANSFER} (RERR_PARTIAL), matching upstream; \
         output:\n{output}"
    );

    // The load-bearing assertion: the readable files must land, byte-identical.
    // Before the fix oc dropped all of them.
    for name in READABLE {
        let landed = oc_dest.join(name);
        assert!(
            landed.is_file(),
            "readable source {name} must still transfer after the out-of-order skip; \
             output:\n{output}"
        );
        assert_eq!(
            fs::read(&landed).expect("read transferred file"),
            fs::read(module_root.join(name)).expect("read source file"),
            "{name} transferred with the wrong contents"
        );
    }

    assert!(
        !oc_dest.join(UNREADABLE).exists(),
        "the file the sender could not open must not be created at the destination; \
         output:\n{output}"
    );
}

/// Non-vacuity companion: the identical fixture with every file readable must
/// transfer all three and exit 0. Without it the pin above would also pass if
/// the module were simply unreachable or the daemon refused every file.
#[test]
fn oc_receiver_transfers_every_file_when_all_readable() {
    if !upstream_compat_enabled() {
        return;
    }

    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_0).expect(
        "OC_RSYNC_UPSTREAM_COMPAT=1 selected this test but upstream rsync 3.5.0 is not \
         installed; build it with `bash tools/ci/run_interop.sh` or point \
         OC_RSYNC_UPSTREAM_BIN_3_5_0 at it",
    );
    let oc_bin = test_support::oc_rsync_bin();

    let tmp: TempDir = tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let module_root = root.join("source");
    seed_module(&module_root);

    let config = root.join("rsyncd.conf");
    write_daemon_config(
        &config,
        &root.join("rsyncd.pid"),
        &root.join("rsyncd.log"),
        &module_root,
    )
    .expect("write daemon config");

    let (_daemon, port) = spawn_upstream_daemon(upstream.binary(), &config)
        .expect("start upstream rsync 3.5.0 --daemon");

    let dest = root.join("dest");
    fs::create_dir_all(&dest).expect("create dest");
    let src = OsString::from(format!("rsync://127.0.0.1:{port}/nosendmod/"));
    let mut dest_arg = dest.clone().into_os_string();
    dest_arg.push("/");
    let args = vec![
        OsStr::new("--recursive"),
        OsStr::new("--times"),
        src.as_os_str(),
        dest_arg.as_os_str(),
    ];

    let outcome = run_under_deadline(&oc_bin, &args).expect("spawn oc-rsync client");
    let Some((status, output)) = outcome else {
        panic!("oc-rsync client did not exit within {RUN_TIMEOUT:?} on the all-readable fixture");
    };
    assert_eq!(
        status.code(),
        Some(0),
        "all-readable fixture must exit 0; output:\n{output}"
    );
    for name in READABLE.iter().chain(std::iter::once(&UNREADABLE)) {
        assert!(
            dest.join(name).is_file(),
            "{name} must transfer when readable; output:\n{output}"
        );
    }
}
