//! A daemon's `--backup-dir` must not move the destination's pre-transfer bytes
//! outside the module root through a *trusted-owned* directory symlink standing
//! where the operator named the backup area.
//!
//! # Why the ownership walk is not enough here
//!
//! `--backup` replaces the destination by renaming (or hard-linking) its
//! pre-image into the operator-named backup area first. Both tiers resolve that
//! area by path, and the ownership walk deliberately FOLLOWS a symlink owned by
//! uid 0 or our own euid - that is the operator's own layout and refusing it
//! would break the ordinary case. A non-chrooted daemon writes everything it
//! creates as its own uid, so a directory symlink a writable module already
//! contains is TRUSTED-owned by construction. Point it outside the module and
//! the follow carries the in-module destination's contents out of the tree,
//! with the client still exiting 0.
//!
//! Upstream closes this with the second half of the rule: `make_backup()`
//! raises `operator_path_resolve` around the WHOLE backup, which is what arms
//! `abspath_outside_confinement()` inside `owner_walk_parent()`. Ownership
//! decides whether to follow; the module root decides whether the landing site
//! is acceptable.
//!
//! # Why the plant is a DIRECTORY symlink, not a leaf one
//!
//! `renameat`/`linkat` never follow a symlink sitting at the final component -
//! they act on the name, replacing it. A leaf plant therefore cannot express
//! this escape at all; only a symlinked component of the backup DIRECTORY can,
//! and only the parent walk can refuse it. That makes this a different sink
//! from the `--inplace` pre-image COPY, whose `O_WRONLY|O_CREAT|O_TRUNC` does
//! follow its leaf.
//!
//! # Deterministic, not a race
//!
//! Everything is planted before the transfer starts. That is also the honest
//! threat model: the escape needs only a trusted-owned symlink to EXIST at the
//! backup directory, not to appear inside a window.
//!
//! # Why these cells run unprivileged
//!
//! Upstream refuses only `st_uid != 0 && st_uid != trusted_uid`
//! (`syscall.c:406`), so uid 0 and the euid take one identical follow path into
//! one identical confinement check. The daemon spawned here runs as the test's
//! own uid, so a plant owned by that uid is exactly the root-owned case a
//! privileged daemon would see. The distinct third-uid REFUSAL arm stays with
//! the root leg of the upstream testsuite.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/backup.c:443-449` `make_backup()` - `operator_path_resolve =
//!   1` around the entire backup.
//! - `rsync-3.5.0/backup.c:226-247` `link_or_rename()` - `do_link_at` first,
//!   `do_rename_at` on failure. Both tiers are inside the region above.
//! - `rsync-3.5.0/syscall.c:581-596` `owner_walk_parent()` - the resolved-leaf
//!   `abspath_outside_confinement()` check the region arms.
//! - `rsync-3.5.0/receiver.c:694-695` `handle_delayed_updates()` - a failed
//!   backup SKIPS the update rather than overwriting the pre-image anyway.

#![cfg(unix)]

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// The destination's pre-transfer contents - what a successful escape deposits
/// outside the module root.
const PRE_IMAGE: &str = "PRE-IMAGE-IN-MODULE";
/// What the client pushes over the destination.
const NEW_CONTENT: &str = "NEW-CONTENT-FROM-THE-PEER";
/// The out-of-module file's contents. Distinct from both of the above so an
/// overwrite is unmistakable rather than inferred from a size or an mtime.
const OUTSIDE_MARKER: &str = "OUTSIDE-THE-MODULE-UNTOUCHED";

struct DaemonGuard(Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// What stands at the `--backup-dir` before the push.
#[derive(Clone, Copy)]
enum Plant {
    /// A real directory: the ordinary layout, and the non-vacuity control.
    RealDir,
    /// A trusted-owned symlink to a directory OUTSIDE the module root.
    DirSymlinkOutsideModule,
    /// A trusted-owned symlink to another directory INSIDE the module root.
    DirSymlinkInsideModule,
}

/// The filesystem state the assertions are made against.
struct Outcome {
    /// Contents of the seeded file in the out-of-module directory - the one an
    /// escape lands on.
    outside: String,
    /// Contents the in-module backup directory received.
    inside_backup: io::Result<String>,
    /// Contents of the destination after the push.
    destination: io::Result<String>,
    /// Whether the `--backup-dir` is still a symlink.
    backup_dir_is_symlink: bool,
    /// The client's exit code, recorded beside every measurement so a
    /// "nothing moved" run cannot be mistaken for a refusal.
    client_exit: Option<i32>,
    /// The client's stderr, for the failure messages.
    client_stderr: String,
}

fn write_config(config: &Path, module_root: &Path, log_root: &Path) -> io::Result<()> {
    fs::write(
        config,
        format!(
            "pid file = {pid}\n\
             log file = {log}\n\
             use chroot = false\n\
             \n\
             [data]\n\
             path = {root}\n\
             read only = false\n",
            pid = log_root.join("rsyncd.pid").display(),
            log = log_root.join("rsyncd.log").display(),
            root = module_root.display(),
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

/// Stages the module, puts `plant` at the `--backup-dir`, pushes one file over
/// an existing destination with `--backup --backup-dir=bak` plus `extra`, and
/// reports the resulting filesystem state.
///
/// Returns `None` when the daemon could not be started, so a harness failure is
/// reported by the caller rather than passing vacuously.
fn push_over_destination(plant: Plant, extra: &[&str]) -> Option<Outcome> {
    let oc_bin = test_support::oc_rsync_bin();
    let tmp = test_support::create_tempdir();
    let root = tmp.path();

    let source_dir = root.join("src");
    let module_root = root.join("module");
    let outside_dir = root.join("outside");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&module_root).expect("create module root");
    fs::create_dir_all(&outside_dir).expect("create the out-of-module dir");

    // Seeded so an escape OVERWRITES a known marker rather than merely creating
    // a file: "the marker is gone" cannot be satisfied by a fixture that named
    // the wrong directory.
    let outside = outside_dir.join("payload");
    fs::write(&outside, OUTSIDE_MARKER).expect("seed the out-of-module file");

    let source = source_dir.join("payload");
    fs::write(&source, NEW_CONTENT).expect("seed source");

    let destination = module_root.join("payload");
    fs::write(&destination, PRE_IMAGE).expect("seed destination");

    let backup_dir = module_root.join("bak");
    let inside_dir = module_root.join("realbak");
    match plant {
        Plant::RealDir => fs::create_dir(&backup_dir).expect("create the real backup dir"),
        Plant::DirSymlinkOutsideModule => {
            symlink(&outside_dir, &backup_dir).expect("plant the out-of-module dir symlink");
        }
        Plant::DirSymlinkInsideModule => {
            fs::create_dir(&inside_dir).expect("create the in-module backup target");
            symlink(&inside_dir, &backup_dir).expect("plant the in-module dir symlink");
        }
    }

    // The plant must be TRUSTED-owned, or a refusal would only be the ownership
    // arm firing and would prove nothing about the module root.
    let meta = fs::symlink_metadata(&backup_dir).expect("the backup dir must exist");
    assert!(
        fast_io::symlink_owner_is_trusted(meta.uid()),
        "the planted backup dir must be owned by uid 0 or our euid; got uid {}",
        meta.uid()
    );

    let config = root.join("rsyncd.conf");
    write_config(&config, &module_root, root).expect("write daemon config");
    let (_daemon, port) = spawn_daemon(&oc_bin, &config).ok()?;

    let mut args: Vec<&OsStr> = vec![
        OsStr::new("--backup"),
        OsStr::new("--backup-dir=bak"),
        OsStr::new("--ignore-times"),
    ];
    args.extend(extra.iter().map(OsStr::new));
    let destination_url = format!("rsync://127.0.0.1:{port}/data/");
    args.push(source.as_os_str());
    args.push(OsStr::new(&destination_url));

    let output = Command::new(&oc_bin)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run oc-rsync client");

    Some(Outcome {
        outside: fs::read_to_string(&outside).expect("the out-of-module file must still exist"),
        inside_backup: fs::read_to_string(inside_dir.join("payload")),
        destination: fs::read_to_string(&destination),
        backup_dir_is_symlink: fs::symlink_metadata(&backup_dir)
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false),
        client_exit: output.status.code(),
        client_stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn measured(plant: Plant, extra: &[&str], what: &str) -> Outcome {
    push_over_destination(plant, extra)
        .unwrap_or_else(|| panic!("{what}: could not start the daemon, so nothing was measured"))
}

/// Asserts the escape did not happen, and that the pre-image the backup exists
/// to preserve was not destroyed instead.
fn assert_confined(outcome: &Outcome, what: &str) {
    assert_eq!(
        outcome.outside, OUTSIDE_MARKER,
        "{what}: the backup escaped the module root - the destination's \
         pre-transfer bytes were deposited outside it (client exit {:?})\
         \nstderr:\n{}",
        outcome.client_exit, outcome.client_stderr,
    );
    assert_eq!(
        outcome.destination.as_deref().ok(),
        Some(PRE_IMAGE),
        "{what}: a refused backup must leave the destination alone - the \
         pre-image is exactly what the backup could not save, so overwriting \
         it anyway destroys it (upstream receiver.c:694-695)\nstderr:\n{}",
        outcome.client_stderr,
    );
    assert!(
        outcome.backup_dir_is_symlink,
        "{what}: the plant must survive as a symlink; a refusal that instead \
         REPLACED it would be a different resolver",
    );
}

/// THE PIN, at the plain `--backup --backup-dir` shape. On a same-filesystem
/// destination the hard-link tier of `link_or_rename()` runs first and
/// succeeds, so this is the tier that actually escapes here; on a receiver
/// whose nested link is refused by its own beneath-confinement it is the rename
/// tier instead. Both are inside upstream's `make_backup()` region, so the cell
/// asserts the outcome rather than the tier.
#[test]
fn a_backup_dir_symlink_leaving_the_module_must_not_receive_the_pre_image() {
    let outcome = measured(
        Plant::DirSymlinkOutsideModule,
        &[],
        "out-of-module backup dir plant",
    );
    assert_confined(&outcome, "plain --backup-dir");
}

/// The same pin on the `--delay-updates` sweep, which is a SEPARATE
/// `make_backup()` site with its own rename: a fix applied only to the
/// disk-commit path would leave this one escaping.
///
/// upstream: `receiver.c:685-720` `handle_delayed_updates()`.
#[test]
fn a_delayed_update_backup_must_not_leave_the_module() {
    let outcome = measured(
        Plant::DirSymlinkOutsideModule,
        &["--delay-updates"],
        "out-of-module backup dir plant, --delay-updates",
    );
    assert_confined(&outcome, "--delay-updates --backup-dir");
}

/// POSITIVE CONTROL for over-refusal. A trusted-owned directory symlink whose
/// target stays INSIDE the module must still be followed and backed up
/// through. Upstream follows an in-tree trusted symlink by design; "refuse a
/// backup through any symlinked directory" would satisfy both pins above while
/// breaking the operator layouts the walk exists to keep working.
#[test]
fn a_backup_dir_symlink_staying_inside_the_module_is_still_followed() {
    let outcome = measured(
        Plant::DirSymlinkInsideModule,
        &[],
        "in-module backup dir plant",
    );

    assert_eq!(
        outcome.client_exit,
        Some(0),
        "an in-module backup dir must not fail the transfer\nstderr:\n{}",
        outcome.client_stderr,
    );
    assert_eq!(
        outcome.inside_backup.as_deref().ok(),
        Some(PRE_IMAGE),
        "the in-module symlink target must receive the pre-transfer bytes: the \
         confinement applies to the LANDING SITE, not to symlinks as such\
         \nstderr:\n{}",
        outcome.client_stderr,
    );
    assert_eq!(
        outcome.destination.as_deref().ok(),
        Some(NEW_CONTENT),
        "the destination must still have been updated\nstderr:\n{}",
        outcome.client_stderr,
    );
    assert!(
        outcome.backup_dir_is_symlink,
        "following the link must not replace it",
    );
}

/// NON-VACUITY companion. With an ordinary directory at the `--backup-dir` the
/// very same push backs the pre-image up and updates the destination. Without
/// this the pins above would also hold if the fixture never reached the backup
/// path at all - if `--backup-dir` were silently ignored, or the transfer never
/// ran.
#[test]
fn an_ordinary_backup_dir_gets_the_pre_transfer_bytes() {
    let outcome = measured(Plant::RealDir, &[], "ordinary backup dir");

    assert_eq!(
        outcome.client_exit,
        Some(0),
        "the plain push must succeed\nstderr:\n{}",
        outcome.client_stderr,
    );
    assert_eq!(
        outcome.destination.as_deref().ok(),
        Some(NEW_CONTENT),
        "the destination must have been updated, or this fixture never \
         exercised the backup path\nstderr:\n{}",
        outcome.client_stderr,
    );
    assert_eq!(
        outcome.outside, OUTSIDE_MARKER,
        "the unplanted run must leave the out-of-module file alone, or the \
         marker used by the pins above proves nothing",
    );
}

/// NON-VACUITY companion for the `--delay-updates` cell: the same sweep, with
/// an ordinary backup directory, must still complete. Without it, that cell
/// would also pass if `--delay-updates` simply aborted the transfer.
#[test]
fn an_ordinary_backup_dir_works_under_delay_updates() {
    let outcome = measured(
        Plant::RealDir,
        &["--delay-updates"],
        "ordinary backup dir, --delay-updates",
    );

    assert_eq!(
        outcome.client_exit,
        Some(0),
        "the --delay-updates push must succeed\nstderr:\n{}",
        outcome.client_stderr,
    );
    assert_eq!(
        outcome.destination.as_deref().ok(),
        Some(NEW_CONTENT),
        "the destination must have been updated by the delayed sweep\
         \nstderr:\n{}",
        outcome.client_stderr,
    );
    // The file an escape would overwrite must be untouched here too.
    assert_eq!(outcome.outside, OUTSIDE_MARKER);
}
