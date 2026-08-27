//! A daemon's `--inplace --backup-dir` must not write outside the module root
//! through a *trusted-owned* symlink planted at the backup leaf.
//!
//! # Why the ownership walk is not enough here
//!
//! `--inplace --backup` bypasses the rename-to-backup mechanism: the
//! destination inode is rewritten in place, so its pre-transfer bytes must be
//! COPIED aside first. That copy opens the operator-named backup path with
//! `O_WRONLY|O_CREAT|O_TRUNC`, and the ownership walk deliberately FOLLOWS a
//! symlink owned by uid 0 or our own euid - that is the operator's own layout
//! and refusing it would break the ordinary case. A non-chrooted daemon writes
//! its `--backup-dir` entries as its own uid, so a backup entry left behind by
//! an earlier transfer is TRUSTED-owned by construction. Point one at a file
//! outside the module and the follow carries the in-module destination's
//! contents out of the tree.
//!
//! Upstream closes this with the second half of the rule: `make_backup()`
//! raises `operator_path_resolve` around the whole backup, and the in-place
//! backup - which never reaches `make_backup()` - raises it around its own
//! `copy_file()`. That flag is what arms `abspath_outside_confinement()`, which
//! refuses a resolved path that has left the module root. Ownership decides
//! whether to follow; the module root decides whether the landing site is
//! acceptable.
//!
//! # Deterministic, not a race
//!
//! The symlink is planted before the transfer starts, so nothing here depends
//! on timing. That is also the honest threat model: the escape needs only a
//! trusted-owned symlink to EXIST at the backup leaf, not to appear inside a
//! window.
//!
//! # Why these cells run unprivileged
//!
//! Upstream refuses only `st_uid != 0 && st_uid != trusted_uid`
//! (`syscall.c:406`), so uid 0 and the euid take one identical follow path into
//! one identical confinement check. The daemon spawned here runs as the test's
//! own uid, so a plant owned by that uid is exactly the root-owned case a
//! privileged daemon would see, and needs no privilege to stage. The distinct
//! third-uid REFUSAL arm stays with the root leg of the upstream testsuite,
//! matching `crates/fast_io/tests/operator_open_module_confinement.rs`.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/backup.c:443-449` `make_backup()` - `operator_path_resolve =
//!   1` around the entire backup.
//! - `rsync-3.5.0/generator.c:2281-2301` and `:2327-2349` - the in-place backup
//!   bypasses `make_backup()`, so the generator raises the flag around
//!   `get_backup_name()` and the `copy_file()` / `do_open_at()` that follow.
//! - `rsync-3.5.0/syscall.c:186-240` `abspath_outside_confinement()` - refuses a
//!   resolved path outside `confinement_root()`, but only while
//!   `operator_path_resolve` is set.
//! - `rsync-3.5.0/syscall.c:136-144` `confinement_root()` - a daemon's root is
//!   `module_dir`.

#![cfg(unix)]

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// The destination's pre-transfer contents - what a successful escape would
/// deposit outside the module root.
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

/// What, if anything, occupies the backup leaf before the push.
#[derive(Clone, Copy)]
enum Plant {
    /// Nothing: the backup leaf is created fresh by the transfer.
    None,
    /// A trusted-owned symlink pointing OUTSIDE the module root.
    SymlinkOutsideModule,
    /// A trusted-owned symlink pointing to another path INSIDE the module.
    SymlinkInsideModule,
}

/// The filesystem state the assertions are made against.
struct Outcome {
    /// Contents of the file outside the module root.
    outside: String,
    /// Contents of the in-module symlink target used by [`Plant::SymlinkInsideModule`].
    inside_target: io::Result<String>,
    /// Contents at the backup leaf, resolved through whatever it names.
    backup_leaf: io::Result<String>,
    /// Whether the backup leaf is still a symlink.
    backup_leaf_is_symlink: bool,
    /// Contents of the destination after the push.
    destination: io::Result<String>,
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

/// Stages the module, plants `plant` at the backup leaf, pushes one file over
/// the destination with `--inplace --backup --backup-dir=bak`, and reports the
/// resulting filesystem state.
///
/// Returns `None` when the daemon could not be started, so a harness failure is
/// reported by the caller rather than passing vacuously.
fn push_over_destination(plant: Plant) -> Option<Outcome> {
    let oc_bin = test_support::oc_rsync_bin();
    let tmp = test_support::create_tempdir();
    let root = tmp.path();

    let source_dir = root.join("src");
    let module_root = root.join("module");
    let outside_dir = root.join("outside");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(module_root.join("bak")).expect("create module root and backup dir");
    fs::create_dir_all(&outside_dir).expect("create the out-of-module dir");

    let outside = outside_dir.join("secret");
    fs::write(&outside, OUTSIDE_MARKER).expect("seed the out-of-module file");

    let source = source_dir.join("payload");
    fs::write(&source, NEW_CONTENT).expect("seed source");

    let destination = module_root.join("payload");
    fs::write(&destination, PRE_IMAGE).expect("seed destination");

    let inside_target = module_root.join("in-module-target");
    let backup_leaf = module_root.join("bak/payload");
    match plant {
        Plant::None => {}
        Plant::SymlinkOutsideModule => {
            symlink(&outside, &backup_leaf).expect("plant the out-of-module symlink");
        }
        Plant::SymlinkInsideModule => {
            fs::write(&inside_target, "IN-MODULE-PLACEHOLDER").expect("seed in-module target");
            symlink(&inside_target, &backup_leaf).expect("plant the in-module symlink");
        }
    }

    // The plant must be TRUSTED-owned, or a refusal would only be the ownership
    // arm firing and would prove nothing about the module root.
    if let Ok(meta) = fs::symlink_metadata(&backup_leaf) {
        assert!(
            fast_io::symlink_owner_is_trusted(meta.uid()),
            "the planted backup leaf must be owned by uid 0 or our euid; \
             got uid {}",
            meta.uid()
        );
    }

    let config = root.join("rsyncd.conf");
    write_config(&config, &module_root, root).expect("write daemon config");
    let (_daemon, port) = spawn_daemon(&oc_bin, &config).ok()?;

    let output = Command::new(&oc_bin)
        .args([
            OsStr::new("--inplace"),
            OsStr::new("--backup"),
            OsStr::new("--backup-dir=bak"),
            OsStr::new("--ignore-times"),
            source.as_os_str(),
            OsStr::new(&format!("rsync://127.0.0.1:{port}/data/")),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run oc-rsync client");

    Some(Outcome {
        outside: fs::read_to_string(&outside).expect("the out-of-module file must still exist"),
        inside_target: fs::read_to_string(&inside_target),
        backup_leaf: fs::read_to_string(&backup_leaf),
        backup_leaf_is_symlink: fs::symlink_metadata(&backup_leaf)
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false),
        destination: fs::read_to_string(&destination),
        client_exit: output.status.code(),
        client_stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn measured(plant: Plant, what: &str) -> Outcome {
    push_over_destination(plant)
        .unwrap_or_else(|| panic!("{what}: could not start the daemon, so nothing was measured"))
}

/// THE PIN. A trusted-owned symlink at the backup leaf points outside the
/// module root. The walk follows it - that part is correct and deliberate - so
/// the module root has to refuse the landing site, or the destination's
/// pre-transfer bytes are written to a file the operator never named and the
/// peer never had access to.
///
/// Without the confinement half this cell observes `outside` holding
/// [`PRE_IMAGE`]: an in-module file's contents, deposited outside the module.
#[test]
fn a_backup_symlink_leaving_the_module_must_not_be_written_through() {
    let outcome = measured(
        Plant::SymlinkOutsideModule,
        "out-of-module backup leaf plant",
    );

    assert_eq!(
        outcome.outside, OUTSIDE_MARKER,
        "the in-place backup escaped the module root: the destination's \
         pre-transfer bytes were written through a trusted-owned symlink to a \
         file outside it (client exit {:?})\nstderr:\n{}",
        outcome.client_exit, outcome.client_stderr,
    );
    assert!(
        outcome.backup_leaf_is_symlink,
        "the plant must survive as a symlink: a refusal that instead REPLACED \
         it would be a different resolver (client exit {:?})",
        outcome.client_exit,
    );
}

/// POSITIVE CONTROL for over-refusal. The same shape of trusted-owned symlink,
/// pointing at a path that stays INSIDE the module, must still be followed and
/// written through. Upstream follows an in-tree trusted symlink by design;
/// "refuse every symlink at the backup leaf" would satisfy the pin above while
/// breaking the operator layouts the walk exists to keep working.
#[test]
fn a_backup_symlink_staying_inside_the_module_is_still_followed() {
    let outcome = measured(Plant::SymlinkInsideModule, "in-module backup leaf plant");

    assert_eq!(
        outcome.client_exit,
        Some(0),
        "an in-module backup target must not fail the transfer\nstderr:\n{}",
        outcome.client_stderr,
    );
    assert_eq!(
        outcome.inside_target.as_deref().ok(),
        Some(PRE_IMAGE),
        "the in-module symlink target must receive the pre-transfer bytes: the \
         confinement applies to the LANDING SITE, not to symlinks as such\
         \nstderr:\n{}",
        outcome.client_stderr,
    );
    assert!(
        outcome.backup_leaf_is_symlink,
        "following the link must not replace it",
    );
}

/// NON-VACUITY companion. With nothing planted, the very same push creates an
/// ordinary backup holding the pre-transfer bytes and updates the destination
/// in place. Without this the two cells above would also hold if the fixture
/// never reached the backup path at all - if `--inplace --backup-dir` were
/// silently ignored, or the transfer never ran.
#[test]
fn an_unplanted_backup_leaf_gets_the_pre_transfer_bytes() {
    let outcome = measured(Plant::None, "unplanted backup leaf");

    assert_eq!(
        outcome.client_exit,
        Some(0),
        "the plain push must succeed\nstderr:\n{}",
        outcome.client_stderr,
    );
    assert_eq!(
        outcome.backup_leaf.as_deref().ok(),
        Some(PRE_IMAGE),
        "the backup must hold the destination's pre-transfer bytes, or this \
         fixture never exercised the in-place backup path\nstderr:\n{}",
        outcome.client_stderr,
    );
    assert_eq!(
        outcome.destination.as_deref().ok(),
        Some(NEW_CONTENT),
        "the destination must have been rewritten in place\nstderr:\n{}",
        outcome.client_stderr,
    );
    assert_eq!(
        outcome.outside, OUTSIDE_MARKER,
        "the unplanted run must leave the out-of-module file alone, or the \
         marker used by the pin above proves nothing",
    );
}
