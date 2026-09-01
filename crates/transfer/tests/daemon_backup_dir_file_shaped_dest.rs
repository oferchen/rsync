//! A daemon push whose destination operand names a FILE must honour a relative
//! `--backup-dir`, not fold the operand into it.
//!
//! # The defect
//!
//! The daemon resolved a client's relative `--backup-dir` eagerly, at
//! argument-parse time, by joining it onto the destination OPERAND. That agrees
//! with upstream only while the operand names a directory. Push a single file at
//! `rsync://host/mod/payload` and the operand names the destination FILE, so
//! `bak` became `<module>/payload/bak` and the backup `mkdir` failed `ENOTDIR`
//! on `payload` itself - an ordinary non-directory component, not a refusal.
//! The whole session died with the daemon reporting
//! `Not a directory (os error 20)` and the client exiting 23, leaving the
//! destination unmodified. Dropping `--backup-dir` from the same push made it
//! succeed.
//!
//! # What upstream does
//!
//! Upstream never anchors the value early. `sanitize_path(NULL, backup_dir,
//! NULL, 0, SP_DEFAULT)` re-roots only an ABSOLUTE value at `module_dir`
//! (`util1.c:1145-1151`, reached only inside `if (*p == '/')`); a relative one
//! is merely `..`-collapsed and stays relative. It is then resolved against the
//! receiver's cwd at backup time, and `get_local_name()` sets that cwd to the
//! operand's PARENT when the destination names a single file
//! (`main.c:832-859`: `change_dir()` on everything before the last slash,
//! return the basename). So `--backup-dir=bak` lands at `<module>/bak/payload`
//! for BOTH operand shapes.
//!
//! Measured against the real rsync 3.5.0 binary: all three cells below produce
//! exactly the state asserted here.
//!
//! # Why the `..` cell is here
//!
//! Leaving the value relative moves the join to the receiver, so the daemon's
//! `..`-collapse is the only thing left standing between a peer-supplied
//! `--backup-dir=../bak` and a write above the module root. Upstream passes the
//! literal depth `0` for this option (`options.c:2409`, unlike the
//! `curr_dir_depth` at `main.c:1239` for `--partial-dir`), so no leading `..`
//! survives at all and the backup stays in the module.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/options.c:2408-2409` - `backup_dir = sanitize_path(NULL,
//!   backup_dir, NULL, 0, SP_DEFAULT)` for a sanitizing (daemon) receiver.
//! - `rsync-3.5.0/util1.c:1145-1151` - the rootdir prefix is applied only when
//!   the value starts with `/`; a relative value keeps no prefix.
//! - `rsync-3.5.0/util1.c:1184-1197` - with `depth <= 0` every `..` component is
//!   collapsed away rather than kept at the start.
//! - `rsync-3.5.0/main.c:832-859` - `get_local_name()` mode 2: a single-file
//!   destination chdirs to the operand's parent and returns its basename.

#![cfg(unix)]

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// The destination's pre-transfer contents - what the backup must preserve.
const PRE_IMAGE: &str = "PRE-IMAGE-IN-MODULE";
/// What the client pushes over the destination.
const NEW_CONTENT: &str = "NEW-CONTENT-FROM-THE-PEER";

struct DaemonGuard(Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The shape of the destination operand the client writes.
#[derive(Clone, Copy)]
enum DestOperand {
    /// `rsync://host:port/data/payload` - names the destination FILE.
    File,
    /// `rsync://host:port/data/` - names the module directory.
    Directory,
}

/// The filesystem state the assertions are made against.
struct Outcome {
    /// Contents at `<module>/bak/payload`, where upstream places the backup.
    backup: io::Result<String>,
    /// Contents of the destination after the push.
    destination: io::Result<String>,
    /// Whether anything at all was created above the module root, which is what
    /// an uncollapsed `..` would produce.
    escaped_backup_exists: bool,
    /// The client's exit code, recorded beside every measurement so a
    /// "nothing moved" run cannot be mistaken for a success.
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

/// Stages a module holding one destination file, pushes a replacement over it
/// with `--backup --backup-dir=<backup_dir>`, and reports the resulting state.
///
/// Returns `None` when the daemon could not be started, so a harness failure is
/// reported by the caller rather than passing vacuously.
fn push_over_destination(operand: DestOperand, backup_dir: &str) -> Option<Outcome> {
    let oc_bin = test_support::oc_rsync_bin();
    let tmp = test_support::create_tempdir();
    let root = tmp.path();

    let source_dir = root.join("src");
    let module_root = root.join("module");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&module_root).expect("create module root");

    let source = source_dir.join("payload");
    fs::write(&source, NEW_CONTENT).expect("seed source");

    let destination = module_root.join("payload");
    fs::write(&destination, PRE_IMAGE).expect("seed destination");

    let config = root.join("rsyncd.conf");
    write_config(&config, &module_root, root).expect("write daemon config");
    let (_daemon, port) = spawn_daemon(&oc_bin, &config).ok()?;

    let url = match operand {
        DestOperand::File => format!("rsync://127.0.0.1:{port}/data/payload"),
        DestOperand::Directory => format!("rsync://127.0.0.1:{port}/data/"),
    };
    let output = Command::new(&oc_bin)
        .args([
            OsStr::new("--backup"),
            OsStr::new(&format!("--backup-dir={backup_dir}")),
            OsStr::new("--ignore-times"),
            source.as_os_str(),
            OsStr::new(&url),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run oc-rsync client");

    Some(Outcome {
        backup: fs::read_to_string(module_root.join("bak/payload")),
        destination: fs::read_to_string(&destination),
        escaped_backup_exists: root.join("bak").exists(),
        client_exit: output.status.code(),
        client_stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn measured(operand: DestOperand, backup_dir: &str, what: &str) -> Outcome {
    push_over_destination(operand, backup_dir)
        .unwrap_or_else(|| panic!("{what}: could not start the daemon, so nothing was measured"))
}

/// Asserts the one state upstream produces: the pre-image sits at
/// `<module>/bak/payload`, the destination carries the new bytes, and nothing
/// was written above the module root.
fn assert_backed_up_inside_the_module(outcome: &Outcome, what: &str) {
    assert_eq!(
        outcome.client_exit,
        Some(0),
        "{what}: the push must succeed\nstderr:\n{}",
        outcome.client_stderr,
    );
    assert_eq!(
        outcome.backup.as_deref().ok(),
        Some(PRE_IMAGE),
        "{what}: the backup must hold the destination's pre-transfer bytes at \
         <module>/bak/payload\nstderr:\n{}",
        outcome.client_stderr,
    );
    assert_eq!(
        outcome.destination.as_deref().ok(),
        Some(NEW_CONTENT),
        "{what}: the destination must carry the pushed bytes\nstderr:\n{}",
        outcome.client_stderr,
    );
    assert!(
        !outcome.escaped_backup_exists,
        "{what}: nothing may be created above the module root",
    );
}

/// THE PIN. The destination operand names a FILE. A relative `--backup-dir` is
/// the client's, so it must be anchored where upstream anchors it - the
/// operand's parent directory, which for this push is the module root - not
/// joined onto the destination file, which cannot hold a directory.
///
/// Before the fix this cell observes the client exiting 23 with the daemon
/// reporting `Not a directory (os error 20)`, no backup, and the destination
/// still holding [`PRE_IMAGE`].
#[test]
fn a_file_shaped_destination_honours_a_relative_backup_dir() {
    let outcome = measured(DestOperand::File, "bak", "file-shaped destination operand");
    assert_backed_up_inside_the_module(&outcome, "file-shaped destination operand");
}

/// POSITIVE CONTROL. The identical `--backup-dir` against a DIRECTORY-shaped
/// operand already worked and must keep working: this is the shape the existing
/// `--backup-dir` fixtures push into, and moving the anchor to the receiver
/// must not disturb it.
#[test]
fn a_directory_shaped_destination_still_honours_the_same_backup_dir() {
    let outcome = measured(
        DestOperand::Directory,
        "bak",
        "directory-shaped destination operand",
    );
    assert_backed_up_inside_the_module(&outcome, "directory-shaped destination operand");
}

/// CONTAINMENT. A relative value now reaches the receiver un-anchored, so the
/// daemon's `..`-collapse is what keeps a peer-supplied `--backup-dir=../bak`
/// from writing above the module root. Upstream collapses it to `bak` at depth
/// `0` and lands in the module; so must oc.
#[test]
fn a_relative_backup_dir_climbing_out_is_collapsed_into_the_module() {
    let outcome = measured(
        DestOperand::File,
        "../bak",
        "escaping relative --backup-dir",
    );
    assert_backed_up_inside_the_module(&outcome, "escaping relative --backup-dir");
}
