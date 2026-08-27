//! The ownership walk must be able to TRAVERSE under a Landlock sandbox.
//!
//! # The shape this stands for
//!
//! Exactly two sites in the workspace install a Landlock ruleset:
//!
//! | installer | shapes it covers | session confinement root |
//! |---|---|---|
//! | `crates/daemon/.../transfer/sandbox.rs` `engage_landlock_sandbox` | daemon over TCP, over inetd, over a remote shell | `Some(<module dir>)` |
//! | `crates/cli/src/frontend/server/run.rs` non-daemon `--server` receiver | rsh/ssh `--server` | **`None`** |
//!
//! The second row is the one an enumeration of "which shapes are daemons"
//! misses, and it is the harder case: Landlock enforces while
//! `session_confinement_root()` is absent. This test reproduces that
//! combination directly - a ruleset granting one directory, no session
//! installed - so no future change can make the walk conditional on a
//! confinement root without turning this red.
//!
//! # Why it fails without `O_PATH`
//!
//! `LANDLOCK_RULE_PATH_BENEATH` grants rights beneath the module root and
//! nothing above it. An absolute walk starts at `/` and steps through `/tmp`,
//! `/tmp/.tmpXXXXXX`, ... - none granted. Under Landlock an
//! `openat(O_RDONLY|O_DIRECTORY)` is an ACCESS needing
//! `LANDLOCK_ACCESS_FS_READ_DIR`, so every one of those steps is `EACCES` and
//! the walk dies before reaching a leaf plainly inside the granted tree.
//! `O_PATH` names a location and requires no right, which is the minimum
//! privilege traversal actually needs.
//!
//! upstream: `rsync-3.5.0/syscall.c:493` opens intermediates
//! `O_RDONLY|O_DIRECTORY`. Upstream confines with chroot, which RELOCATES `/`
//! so its walk starts inside the jail; Landlock does not relocate `/`. The
//! divergence is forced by the mechanism, not by a difference in policy.

#![cfg(all(target_os = "linux", feature = "landlock"))]

use fast_io::landlock::{LandlockOutcome, is_supported, restrict_to_module_paths};
use fast_io::operator_read_to_string;
use std::path::PathBuf;
use std::thread;
use tempfile::TempDir;

/// Landlock restricts the calling THREAD irreversibly, so each scenario runs
/// on a worker that exits immediately afterwards. The `TempDir` is owned by
/// the caller and dropped here, outside the sandbox - a restricted thread
/// cannot unlink its own parent directory.
fn in_sandbox<F>(scenario: F) -> Result<(), String>
where
    F: FnOnce() -> Result<(), String> + Send + 'static,
{
    thread::Builder::new()
        .name("landlock-owner-walk".into())
        .spawn(scenario)
        .map_err(|e| format!("spawn worker: {e}"))?
        .join()
        .map_err(|_| "worker thread panicked".to_owned())?
}

#[test]
fn the_walk_reaches_a_leaf_inside_the_only_granted_root() {
    if !is_supported() {
        return;
    }
    let temp = TempDir::new().expect("tempdir");
    let module = temp.path().join("module");
    std::fs::create_dir(&module).expect("mkdir module");
    let payload = module.join("payload");
    std::fs::write(&payload, b"INSIDE").expect("write payload");
    let outside = temp.path().join("secret");
    std::fs::write(&outside, b"OUTSIDE").expect("write secret");

    let (module, payload, outside): (PathBuf, PathBuf, PathBuf) = (module, payload, outside);
    in_sandbox(move || {
        // No confinement session is installed: this is the non-daemon
        // `--server` shape, where Landlock enforces and the session root is
        // absent.
        match restrict_to_module_paths(&[module.as_path()]) {
            LandlockOutcome::Enforced(_) => {}
            other => return Err(format!("sandbox did not engage: {other:?}")),
        }

        // Non-vacuity: the ruleset must genuinely be in force. Without this,
        // a sandbox that silently failed to install would make the assertion
        // below pass for the wrong reason.
        let refused = operator_read_to_string(&outside)
            .err()
            .ok_or_else(|| "a path outside the granted root was READ".to_owned())?;
        if refused.raw_os_error() != Some(libc::EACCES) {
            return Err(format!("expected EACCES outside the root, got {refused}"));
        }

        // The assertion. Every interior component of this absolute path is
        // ungranted; only the leaf's own tree is. With `O_RDONLY|O_DIRECTORY`
        // intermediates the walk cannot get past `/`.
        match operator_read_to_string(&payload) {
            Ok(body) if body == "INSIDE" => Ok(()),
            Ok(body) => Err(format!("wrong content: {body:?}")),
            Err(e) => Err(format!(
                "the walk could not traverse to a leaf inside the granted root: {e}"
            )),
        }
    })
    .expect("sandboxed scenario");

    drop(temp);
}
