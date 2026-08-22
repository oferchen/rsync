//! `--write-batch` creates its two files with upstream's modes.
//!
//! Upstream opens all three batch files through one primitive, each with its
//! own literal mode:
//!
//! ```c
//! batch_sh_fd = open_no_attacker_symlinks(filename, O_WRONLY|O_CREAT|O_TRUNC|O_BINARY,
//!                                         S_IRUSR | S_IWUSR | S_IXUSR);   /* batch.c:254 */
//! batch_fd    = open_no_attacker_symlinks(batch_name, O_WRONLY|O_CREAT|O_TRUNC|O_BINARY,
//!                                         S_IRUSR | S_IWUSR);             /* batch.c:263 */
//! ```
//!
//! Owner-only on the batch file is load-bearing rather than incidental: the
//! batch stream carries the transferred file *contents*, so a world-readable
//! batch publishes everything the transfer moved, and a world-*writable* one
//! lets an attacker choose what a later `--read-batch` writes.
//!
//! Two independent things are pinned here, and they fail in opposite
//! directions:
//!
//! - **the create mode** - `0600` / `0700`, not the `0666 & ~umask` an ordinary
//!   `File::create` carries. The umask has to be pinned or this assertion is
//!   worthless: `umask(2)` can only *clear* bits, so under the usual `022` a
//!   `0666` request lands as `0644` and a `0600` request also lands as `0600` -
//!   close enough to look right while the request was wrong. Under umask `0`,
//!   `0666` stays `0666` and only a genuine `0600` request reads back as
//!   `0600`. [`pinning_the_child_umask_takes_effect`] is the control for that.
//!
//! - **that the mode is applied by the create and never re-applied** - upstream
//!   passes the mode to `O_CREAT` and issues no `chmod`, so re-running over an
//!   existing batch file truncates it and leaves its mode alone. Measured
//!   directly against rsync 3.5.0: pre-existing `0666` files stay `0666` on
//!   both. A post-hoc `chmod` would satisfy the first assertion and break this
//!   one, which is why both cells are here.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// upstream: batch.c:263 - `S_IRUSR | S_IWUSR` on the batch file itself.
const UPSTREAM_BATCH_FILE_MODE: u32 = 0o600;

/// upstream: batch.c:254 - `S_IRUSR | S_IWUSR | S_IXUSR` on the `.sh` companion.
const UPSTREAM_BATCH_SCRIPT_MODE: u32 = 0o700;

/// A mode with group and other bits set, used to seed the pre-existing-file
/// cell. Chosen so that a umask of `0` leaves it untouched and any re-applied
/// create mode is visible as a change.
const PERMISSIVE_SEED_MODE: u32 = 0o666;

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Builds a command whose child runs under umask `0`.
///
/// Pinned on the child rather than by mutating this process's umask around the
/// spawn: `umask(2)` is process-global, and the harness runs cases in parallel
/// threads, so an in-process pin would leak into whatever else is creating
/// files at that moment. `pre_exec` runs after `fork(2)` and before `exec(2)`,
/// where only async-signal-safe calls are permitted - `umask(2)` is one.
fn command_with_open_umask(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
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
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
        .permissions()
        .mode()
        & 0o7777
}

/// Lays out `src/` with one file plus an empty `dst/`, and returns both.
fn setup_tree(root: &Path) -> (PathBuf, PathBuf) {
    let src = root.join("src");
    let dst = root.join("dst");
    fs::create_dir_all(&src).expect("create src");
    fs::create_dir_all(&dst).expect("create dst");
    fs::write(src.join("payload.txt"), b"batch payload\n").expect("write payload");
    (src, dst)
}

/// Runs `oc-rsync -a --write-batch=<batch> src/ dst/` under umask 0.
fn run_write_batch(root: &Path, batch: &Path) {
    let (src, dst) = setup_tree(root);
    let status = command_with_open_umask(oc_binary())
        .current_dir(root)
        .arg("-a")
        .arg(format!("--write-batch={}", batch.display()))
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dst.display()))
        .status()
        .expect("spawn oc-rsync");
    assert!(status.success(), "--write-batch failed: {status:?}");
}

/// The control: without this, every mode assertion below could pass for the
/// wrong reason. A child under umask `0` that requests `0666` must read back
/// `0666`; if the pin did not take, the ambient `022` would clear the write
/// bits and `0644` would be indistinguishable from a correct `0644` request.
#[test]
fn pinning_the_child_umask_takes_effect() {
    let temp = TempDir::new().expect("temp dir");
    let probe = temp.path().join("probe");

    let status = command_with_open_umask("/bin/sh")
        .arg("-c")
        .arg(format!("> {}", probe.display()))
        .status()
        .expect("spawn sh");
    assert!(status.success(), "probe shell failed: {status:?}");

    assert_eq!(
        mode_of(&probe),
        PERMISSIVE_SEED_MODE,
        "child umask was not pinned to 0, so no mode assertion in this file \
         can discriminate a 0600 request from a 0666 one",
    );
}

#[test]
fn write_batch_creates_its_files_with_upstream_modes() {
    let temp = TempDir::new().expect("temp dir");
    let batch = temp.path().join("BATCH");

    run_write_batch(temp.path(), &batch);

    let script = temp.path().join("BATCH.sh");
    assert_eq!(
        mode_of(&batch),
        UPSTREAM_BATCH_FILE_MODE,
        "batch file must be owner-only (upstream batch.c:263); it carries the \
         transferred file contents",
    );
    assert_eq!(
        mode_of(&script),
        UPSTREAM_BATCH_SCRIPT_MODE,
        "batch .sh must be owner-only plus execute (upstream batch.c:254)",
    );
}

#[test]
fn rerunning_write_batch_leaves_an_existing_files_mode_alone() {
    let temp = TempDir::new().expect("temp dir");
    let batch = temp.path().join("BATCH");
    let script = temp.path().join("BATCH.sh");

    for path in [&batch, &script] {
        fs::write(path, b"").expect("seed file");
        fs::set_permissions(path, fs::Permissions::from_mode(PERMISSIVE_SEED_MODE))
            .expect("seed mode");
    }

    run_write_batch(temp.path(), &batch);

    // upstream passes the mode to `O_CREAT` and never chmods, so an existing
    // file is truncated with its mode untouched. Measured against rsync 3.5.0:
    // both files stay 0666.
    assert_eq!(
        mode_of(&batch),
        PERMISSIVE_SEED_MODE,
        "an existing batch file's mode must survive the rewrite - upstream \
         applies the mode only on create",
    );
    assert_eq!(
        mode_of(&script),
        PERMISSIVE_SEED_MODE,
        "an existing .sh's mode must survive the rewrite - a post-hoc chmod \
         here would diverge from upstream",
    );
}
