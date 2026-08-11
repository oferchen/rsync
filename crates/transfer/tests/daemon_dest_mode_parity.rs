//! Pins upstream's `dest_mode()` permission rule on the daemon receiver.
//!
//! Upstream decides the destination's mode BEFORE it opens the output file, and
//! the decision turns on whether the destination already existed:
//!
//! ```text
//! int exists = fd1 != -1;                                      // receiver.c:955
//! file->mode = dest_mode(file->mode, st.st_mode, dflt_perms, exists);
//! if (inplace || one_inplace) { ... }                          // receiver.c:967
//! ```
//!
//! and `dest_mode()` itself (rsync.c:449-472):
//!
//! ```text
//! if (exists)  new_mode = (flist_mode & ~CHMOD_BITS) | (stat_mode & CHMOD_BITS);
//! else         new_mode = flist_mode & (~CHMOD_BITS | dflt_perms);
//! ```
//!
//! So without `-p`: an EXISTING destination keeps its own permission bits, and a
//! NEW destination gets the source mode masked by the receiver's default perms.
//! That holds for every write path - upstream picks the mode before it chooses
//! between temp+rename and inplace.
//!
//! oc applies metadata at commit time instead. For temp+rename that is
//! equivalent, because the final path is untouched until the rename. An inplace
//! write is not: it writes through the destination, so the pre-transfer stat has
//! to be captured before the open (see
//! `disk_commit::process::file_ops::inplace_pre_transfer_stat`). Without that
//! capture the commit took the brand-new-file branch and chmod'd an existing
//! destination to `flist_mode & (~CHMOD_BITS | dflt_perms)` - mode 000 whenever
//! the entry carried no permission bits, leaving the transferred file
//! unreadable.
//!
//! These cells therefore cover the write paths as a SET. A regression that
//! reintroduces the defect on any one of them fails here even if the others
//! still behave, which is the point: the rule is one rule, and every path that
//! commits a file has to land on it.

#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// Source mode deliberately differs from the destination mode so the two
/// `dest_mode()` branches produce DIFFERENT answers. With both at 0o644 the
/// test would pass under either branch and prove nothing.
const SOURCE_MODE: u32 = 0o600;
/// Pre-existing destination mode: distinct from `SOURCE_MODE`, and not a mode
/// the new-file formula could produce from it.
const DEST_MODE: u32 = 0o644;

struct DaemonGuard(Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn write_config(config: &Path, root: &Path, module: &str) -> io::Result<()> {
    fs::write(
        config,
        format!(
            "pid file = {pid}\n\
             log file = {log}\n\
             use chroot = false\n\
             \n\
             [{module}]\n\
             path = {root}\n\
             read only = false\n",
            pid = root.join("rsyncd.pid").display(),
            log = root.join("rsyncd.log").display(),
            module = module,
            root = root.display(),
        ),
    )
}

fn spawn_daemon(oc_bin: &Path, config: &Path) -> io::Result<(DaemonGuard, u16)> {
    let (child, port) = test_support::spawn_daemon_on_free_port(|port| {
        Command::new(oc_bin)
            .arg("--daemon")
            .arg("--no-detach")
            .arg("--port")
            .arg(port.to_string())
            .arg("--config")
            .arg(config)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    })?;
    Ok((DaemonGuard(child), port))
}

/// Pushes one file to a daemon module and returns the destination's mode bits.
///
/// `pre_existing` seeds the destination at [`DEST_MODE`] when set, exercising
/// upstream's `exists != 0` branch; otherwise the destination is absent and the
/// `exists == 0` branch applies.
fn dest_mode_after_push(extra_args: &[&str], pre_existing: bool) -> Option<u32> {
    let oc_bin = test_support::oc_rsync_bin();
    let tmp = test_support::create_tempdir();
    let root = tmp.path();
    let (source_dir, module_root) = (root.join("src"), root.join("dest"));
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&module_root).expect("create module root");

    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let source = source_dir.join("payload.bin");
    fs::write(&source, &payload).expect("seed source");
    fs::set_permissions(&source, fs::Permissions::from_mode(SOURCE_MODE)).expect("chmod source");

    let dest = module_root.join("payload.bin");
    if pre_existing {
        // A true prefix, so `--append-verify` appends rather than restarting.
        fs::write(&dest, &payload[..1024]).expect("seed destination");
        fs::set_permissions(&dest, fs::Permissions::from_mode(DEST_MODE)).expect("chmod dest");
    }

    let config = root.join("rsyncd.conf");
    write_config(&config, &module_root, "data").expect("write daemon config");

    let Ok((_daemon, port)) = spawn_daemon(&oc_bin, &config) else {
        // The harness could not start a daemon; report it rather than passing
        // vacuously. Callers treat `None` as "did not measure".
        return None;
    };

    let mut args: Vec<OsString> = extra_args.iter().map(OsString::from).collect();
    args.push(OsString::from("--ignore-times"));
    args.push(source.clone().into_os_string());
    args.push(OsString::from(format!(
        "rsync://127.0.0.1:{port}/data/payload.bin"
    )));

    let status = Command::new(&oc_bin)
        .args(args.iter().map(OsStr::new))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run oc-rsync client");
    assert!(
        status.status.success(),
        "push {extra_args:?} (pre_existing={pre_existing}) exited {:?}\nstderr:\n{}",
        status.status,
        String::from_utf8_lossy(&status.stderr),
    );

    Some(
        fs::metadata(&dest)
            .expect("destination exists after push")
            .permissions()
            .mode()
            & 0o7777,
    )
}

/// Upstream's `exists != 0` branch: the destination keeps its OWN permission
/// bits, whichever write path committed the file.
///
/// This is the cell the mode-000 defect failed: the inplace paths chmod'd the
/// destination to the new-file formula's result instead of preserving 0o644.
#[test]
fn existing_destination_keeps_its_mode_on_every_write_path() {
    for extra in [
        &[][..],                  // temp + rename
        &["--inplace"][..],       // writes through the destination
        &["--append-verify"][..], // implies --inplace (options.c:2400-2411)
    ] {
        let Some(mode) = dest_mode_after_push(extra, true) else {
            panic!("{extra:?}: could not start the daemon, so nothing was measured");
        };
        assert_eq!(
            mode, DEST_MODE,
            "{extra:?}: upstream dest_mode() keeps an existing destination's \
             permission bits (rsync.c:449-472, exists != 0); got {mode:04o}, \
             want {DEST_MODE:04o}",
        );
    }
}

/// Upstream's `exists == 0` branch: a brand-new destination takes the source
/// mode masked by the receiver's default permissions, on every write path.
///
/// Pinned alongside the existing-destination cell so a fix for one branch
/// cannot silently regress the other - the two share a single code path and a
/// single upstream rule.
///
/// This cell regressed under a workspace `--all-features` build, which is the
/// only configuration that enables `daemon-seccomp`. `umask(2)` is absent from
/// the worker allowlist, and a non-allowlisted syscall is answered with EPERM,
/// so the receiver's first (lazy) umask read returned -1. Cached as `u32::MAX`,
/// that made `dflt_perms` = `0o777 & !u32::MAX` = 0, collapsing this branch's
/// `flist_mode & (~CHMOD_BITS | dflt_perms)` to mode 000 on every write path.
/// The existing-destination cell was unaffected because its branch never reads
/// `dflt_perms`. Fixed by capturing the umask at startup as upstream does
/// (`main.c:1797`), before any sandbox is installed.
#[test]
fn new_destination_takes_the_masked_source_mode_on_every_write_path() {
    let expected = masked_source_mode();
    for extra in [&[][..], &["--inplace"][..], &["--append-verify"][..]] {
        let Some(mode) = dest_mode_after_push(extra, false) else {
            panic!("{extra:?}: could not start the daemon, so nothing was measured");
        };
        assert_eq!(
            mode, expected,
            "{extra:?}: upstream dest_mode() applies `flist_mode & (~CHMOD_BITS \
             | dflt_perms)` to a new destination (rsync.c:449-472, exists == 0); \
             got {mode:04o}, want {expected:04o}",
        );
    }
}

/// Returns `SOURCE_MODE` as the filesystem would create it, i.e. masked by the
/// process umask.
///
/// Observed rather than computed: creating a file with `SOURCE_MODE` applies the
/// umask exactly as the receiver's own `O_CREAT` does, which keeps this test
/// free of `unsafe` (`crates/transfer` denies it) and of a `libc` dependency.
fn masked_source_mode() -> u32 {
    let probe_dir = test_support::create_tempdir();
    let probe = probe_dir.path().join("umask-probe");
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(SOURCE_MODE)
        .open(&probe)
        .expect("create umask probe");
    fs::metadata(&probe)
        .expect("stat umask probe")
        .permissions()
        .mode()
        & 0o7777
}
