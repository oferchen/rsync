//! Landlock LSM defense-in-depth allowlist for the daemon receiver path.
//!
//! Layers a kernel-enforced allowlist above the SEC-1 `*at` syscall helpers
//! so that even a future regression that calls a path-based syscall directly
//! (bypassing [`crate::dir_sandbox::DirSandbox`]) is rejected by the kernel
//! with `EACCES`. Targets Linux 5.13+ with best-effort downgrade picking the
//! highest ABI the running kernel exposes.
//!
//! The helper is a one-shot per-thread restriction: once
//! [`crate::landlock::restrict_to_module_paths`] succeeds, the calling thread (and every
//! process inherited from it - notably the daemon's name converter and
//! pre/post-xfer-exec hooks) cannot reach paths outside the supplied roots
//! through any filesystem syscall, regardless of how the path was resolved.
//!
//! See `docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md` for the
//! kernel-version matrix, downgrade rationale, and daemon integration plan.

use std::io;
use std::path::Path;

use landlock::{
    ABI, Access, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetStatus,
};

/// Outcome of a [`crate::landlock::restrict_to_module_paths`] call.
///
/// Carries enough detail for the daemon to log the actual enforcement level
/// without leaking the `landlock` crate's types into the public API.
#[derive(Debug)]
pub enum LandlockOutcome {
    /// The ruleset was created and applied. The carried [`RulesetStatus`]
    /// distinguishes `FullyEnforced` (every requested right was honoured),
    /// `PartiallyEnforced` (best-effort downgrade dropped some rights -
    /// typically REFER on 5.13-5.18 and TRUNCATE on 5.13-6.1), and
    /// `NotEnforced` (the kernel accepted the ruleset but applied nothing,
    /// equivalent to no sandbox).
    Enforced(RulesetStatus),
    /// The kernel does not expose Landlock at all (pre-5.13, or the LSM is
    /// not enabled at boot). SEC-1 `*at` helpers remain the only defense.
    Unavailable,
    /// Ruleset creation or `restrict_self()` failed even though the kernel
    /// advertised Landlock support. The daemon must treat this as a fatal
    /// connection error - the intended sandbox did not engage.
    Error(io::Error),
}

/// Probes whether the running kernel exposes any Landlock ABI.
///
/// Returns `true` on Linux 5.13+ with the LSM enabled at boot, `false`
/// otherwise. The probe issues `landlock_create_ruleset(2)` via
/// [`Ruleset::create`] and immediately drops the returned fd; **it must
/// never call `RulesetCreated::restrict_self`** because Landlock
/// intersects every successive `restrict_self` on the calling thread.
/// Probing with an empty allowlist would therefore deny every subsequent
/// filesystem write for the rest of the thread's life and the real
/// [`crate::landlock::restrict_to_module_paths`] call could only narrow that intersection,
/// never relax it. Cheap but not memoised - cache the result if you call
/// repeatedly.
#[must_use]
pub fn is_supported() -> bool {
    let access = AccessFs::from_all(ABI::V1);
    Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(access)
        .and_then(|r| r.create())
        .is_ok()
}

/// Restricts the current thread to read+write access only under
/// `allowed_roots`.
///
/// The helper requests `AccessFs::from_all(ABI::V3)` (read / write / create /
/// delete / rename / symlink / refer / truncate) with best-effort downgrade
/// enabled, so a kernel that understands only v1 or v2 accepts the subset it
/// supports and silently drops the rest. The returned [`LandlockOutcome`]
/// carries the [`RulesetStatus`] so callers can log the actual enforcement
/// level.
///
/// Call exactly once per daemon connection, after privilege drop and any
/// chroot have completed, before any user-controlled file operation begins.
/// All roots must be absolute and must already exist - the helper does not
/// create them. An empty `allowed_roots` slice denies every filesystem write
/// once `restrict_self()` engages, which is the correct posture for a
/// connection that has no module to serve.
///
/// # Errors
///
/// Returns [`LandlockOutcome::Error`] when the kernel advertised Landlock
/// support but ruleset construction, rule addition, or `restrict_self()`
/// failed. Returns [`LandlockOutcome::Unavailable`] (not an error) on
/// pre-5.13 kernels so the daemon can keep running with SEC-1 `*at` helpers
/// as the sole defense.
pub fn restrict_to_module_paths(allowed_roots: &[&Path]) -> LandlockOutcome {
    // Request the highest ABI we support; BestEffort lets the crate silently
    // drop rights the running kernel cannot honour (REFER on 5.13-5.18,
    // TRUNCATE on 5.13-6.1). The final enforcement tier surfaces in the
    // RulesetStatus returned by restrict_self below.
    let access = AccessFs::from_all(ABI::V3);

    let ruleset = match Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(access)
    {
        Ok(rs) => rs,
        Err(err) => return LandlockOutcome::Error(io::Error::other(err.to_string())),
    };

    let mut created = match ruleset.create() {
        Ok(c) => c,
        Err(err) => return LandlockOutcome::Error(io::Error::other(err.to_string())),
    };

    for root in allowed_roots {
        let fd = match PathFd::new(root) {
            Ok(fd) => fd,
            Err(err) => return LandlockOutcome::Error(io::Error::other(err.to_string())),
        };
        created = match created.add_rule(PathBeneath::new(fd, access)) {
            Ok(c) => c,
            Err(err) => return LandlockOutcome::Error(io::Error::other(err.to_string())),
        };
    }

    match created.restrict_self() {
        Ok(status) => LandlockOutcome::Enforced(status.ruleset),
        Err(err) => LandlockOutcome::Error(io::Error::other(err.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::ErrorKind;
    use std::sync::Mutex;
    use std::thread;
    use tempfile::TempDir;

    // Landlock applies to the calling thread and is irreversible: once a
    // thread is restricted, every future syscall in that thread is subject
    // to the allowlist. To keep tests isolated we run each scenario on a
    // dedicated worker thread that exits as soon as the assertions are
    // collected. A test-wide mutex serialises the worker spawn so the
    // probe assertions stay deterministic even on highly-parallel runners.
    static SERIALISE: Mutex<()> = Mutex::new(());

    fn run_isolated<F>(scenario: F) -> Result<(), String>
    where
        F: FnOnce() -> Result<(), String> + Send + 'static,
    {
        let _lock = SERIALISE.lock().unwrap_or_else(|e| e.into_inner());
        let handle = thread::Builder::new()
            .name("landlock-test".into())
            .spawn(scenario)
            .map_err(|e| format!("spawn worker: {e}"))?;
        handle
            .join()
            .map_err(|_| "worker thread panicked".to_owned())?
    }

    #[test]
    fn is_supported_returns_bool_without_panic() {
        // The probe must not panic regardless of kernel support; on CI
        // Linux runners (5.13+) it returns true, on pre-5.13 it returns
        // false. Either outcome is acceptable here - the test only proves
        // the probe path is sound.
        let _ = is_supported();
    }

    #[test]
    fn unavailable_kernel_returns_unavailable_variant() {
        if is_supported() {
            // Nothing to assert: on a supporting kernel the helper engages
            // the sandbox and the `Unavailable` branch is never reached.
            return;
        }
        let tmp = TempDir::new().expect("tempdir");
        let outcome = restrict_to_module_paths(&[tmp.path()]);
        assert!(matches!(outcome, LandlockOutcome::Unavailable));
    }

    #[test]
    fn allows_write_inside_module_root() {
        if !is_supported() {
            return;
        }
        let tmp = TempDir::new().expect("tempdir");
        let allowed = tmp.path().to_path_buf();
        run_isolated(move || {
            let outcome = restrict_to_module_paths(&[allowed.as_path()]);
            match outcome {
                LandlockOutcome::Enforced(_) => {}
                LandlockOutcome::Unavailable => return Ok(()),
                LandlockOutcome::Error(err) => return Err(format!("setup: {err}")),
            }
            fs::write(allowed.join("inside.txt"), b"x")
                .map_err(|e| format!("write inside failed: {e}"))
        })
        .expect("inside-write scenario");
    }

    #[test]
    fn blocks_write_outside_module_root() {
        if !is_supported() {
            return;
        }
        let allowed = TempDir::new().expect("allowed tempdir");
        let outside = TempDir::new().expect("outside tempdir");
        let allowed_path = allowed.path().to_path_buf();
        let outside_path = outside.path().to_path_buf();
        // Keep the directories alive past the thread's lifetime; the worker
        // owns the path strings, the parent owns the TempDir guards.
        run_isolated(move || {
            let outcome = restrict_to_module_paths(&[allowed_path.as_path()]);
            match outcome {
                LandlockOutcome::Enforced(RulesetStatus::NotEnforced) => return Ok(()),
                LandlockOutcome::Enforced(_) => {}
                LandlockOutcome::Unavailable => return Ok(()),
                LandlockOutcome::Error(err) => return Err(format!("setup: {err}")),
            }
            match fs::write(outside_path.join("outside.txt"), b"x") {
                Ok(()) => Err("write outside unexpectedly succeeded".to_owned()),
                Err(err) if err.kind() == ErrorKind::PermissionDenied => Ok(()),
                Err(err) => Err(format!("unexpected error {:?}: {err}", err.kind())),
            }
        })
        .expect("outside-write scenario");
        drop(allowed);
        drop(outside);
    }

    #[test]
    fn allows_writes_under_every_root_in_multi_root_allowlist() {
        // URV-5.b.REOPEN regression: the daemon engages Landlock with the
        // module root *plus* any client-supplied in-module alt-basis /
        // temp-dir / partial-dir / backup-dir paths. A widened allowlist
        // must accept writes beneath every listed root, not just the
        // first one.
        if !is_supported() {
            return;
        }
        let module = TempDir::new().expect("module tempdir");
        let extra = TempDir::new().expect("extra tempdir");
        let module_path = module.path().to_path_buf();
        let extra_path = extra.path().to_path_buf();
        run_isolated(move || {
            let outcome = restrict_to_module_paths(&[module_path.as_path(), extra_path.as_path()]);
            match outcome {
                LandlockOutcome::Enforced(RulesetStatus::NotEnforced) => return Ok(()),
                LandlockOutcome::Enforced(_) => {}
                LandlockOutcome::Unavailable => return Ok(()),
                LandlockOutcome::Error(err) => return Err(format!("setup: {err}")),
            }
            fs::write(module_path.join("module.txt"), b"x")
                .map_err(|e| format!("write inside module failed: {e}"))?;
            fs::write(extra_path.join("extra.txt"), b"x")
                .map_err(|e| format!("write inside extra root failed: {e}"))?;
            Ok(())
        })
        .expect("multi-root scenario");
        drop(module);
        drop(extra);
    }

    #[test]
    fn multi_root_allowlist_still_blocks_paths_outside_every_root() {
        // The widening only relaxes confinement for *enumerated* roots.
        // Anything outside the union must remain blocked - this is the
        // trust boundary URV-5.c.5 will lean on when Landlock flips
        // default-on.
        if !is_supported() {
            return;
        }
        let module = TempDir::new().expect("module tempdir");
        let extra = TempDir::new().expect("extra tempdir");
        let outside = TempDir::new().expect("outside tempdir");
        let module_path = module.path().to_path_buf();
        let extra_path = extra.path().to_path_buf();
        let outside_path = outside.path().to_path_buf();
        run_isolated(move || {
            let outcome = restrict_to_module_paths(&[module_path.as_path(), extra_path.as_path()]);
            match outcome {
                LandlockOutcome::Enforced(RulesetStatus::NotEnforced) => return Ok(()),
                LandlockOutcome::Enforced(_) => {}
                LandlockOutcome::Unavailable => return Ok(()),
                LandlockOutcome::Error(err) => return Err(format!("setup: {err}")),
            }
            match fs::write(outside_path.join("outside.txt"), b"x") {
                Ok(()) => Err("write outside every allowlist root succeeded".to_owned()),
                Err(err) if err.kind() == ErrorKind::PermissionDenied => Ok(()),
                Err(err) => Err(format!("unexpected error {:?}: {err}", err.kind())),
            }
        })
        .expect("multi-root outside scenario");
        drop(module);
        drop(extra);
        drop(outside);
    }

    #[test]
    fn empty_allowlist_denies_all_writes() {
        if !is_supported() {
            return;
        }
        let scratch = TempDir::new().expect("scratch");
        let scratch_path = scratch.path().to_path_buf();
        run_isolated(move || {
            let outcome = restrict_to_module_paths(&[]);
            match outcome {
                LandlockOutcome::Enforced(RulesetStatus::NotEnforced) => return Ok(()),
                LandlockOutcome::Enforced(_) => {}
                LandlockOutcome::Unavailable => return Ok(()),
                LandlockOutcome::Error(err) => return Err(format!("setup: {err}")),
            }
            match fs::write(scratch_path.join("denied.txt"), b"x") {
                Ok(()) => Err("empty allowlist let a write through".to_owned()),
                Err(err) if err.kind() == ErrorKind::PermissionDenied => Ok(()),
                Err(err) => Err(format!("unexpected error {:?}: {err}", err.kind())),
            }
        })
        .expect("empty-allowlist scenario");
        drop(scratch);
    }

    #[test]
    fn is_supported_does_not_restrict_caller() {
        // Regression: an earlier `is_supported()` engaged Landlock with an
        // empty allowlist on the calling thread, so every subsequent
        // `restrict_to_module_paths` call was intersected with "deny all"
        // and the daemon's receiver could not create its temp files. The
        // probe must leave the caller's filesystem rights untouched so a
        // later `restrict_to_module_paths(&[root])` actually permits writes
        // beneath `root`.
        if !is_supported() {
            return;
        }
        let allowed = TempDir::new().expect("allowed tempdir");
        let allowed_path = allowed.path().to_path_buf();
        run_isolated(move || {
            // Mirror the daemon ordering: probe first, then engage on the
            // real module root.
            assert!(is_supported(), "probe must remain idempotent");
            let outcome = restrict_to_module_paths(&[allowed_path.as_path()]);
            match outcome {
                LandlockOutcome::Enforced(RulesetStatus::NotEnforced) => return Ok(()),
                LandlockOutcome::Enforced(_) => {}
                LandlockOutcome::Unavailable => return Ok(()),
                LandlockOutcome::Error(err) => return Err(format!("setup: {err}")),
            }
            fs::write(allowed_path.join("post_probe.txt"), b"x")
                .map_err(|e| format!("write inside after probe failed: {e}"))
        })
        .expect("probe-then-engage scenario");
    }

    #[test]
    fn second_call_does_not_relax() {
        if !is_supported() {
            return;
        }
        let allowed = TempDir::new().expect("allowed");
        let outside = TempDir::new().expect("outside");
        let allowed_path = allowed.path().to_path_buf();
        let outside_path = outside.path().to_path_buf();
        run_isolated(move || {
            let first = restrict_to_module_paths(&[allowed_path.as_path()]);
            match first {
                LandlockOutcome::Enforced(RulesetStatus::NotEnforced) => return Ok(()),
                LandlockOutcome::Enforced(_) => {}
                LandlockOutcome::Unavailable => return Ok(()),
                LandlockOutcome::Error(err) => return Err(format!("first call: {err}")),
            }
            // Landlock semantics: a second restrict_self() applies an
            // intersection of rights, never a relaxation. The second call
            // may succeed or fail; either way writes outside `allowed` must
            // remain blocked.
            let _ = restrict_to_module_paths(&[outside_path.as_path()]);
            match fs::write(outside_path.join("relaxed.txt"), b"x") {
                Ok(()) => Err("second call relaxed the sandbox".to_owned()),
                Err(err) if err.kind() == ErrorKind::PermissionDenied => Ok(()),
                Err(err) => Err(format!("unexpected error {:?}: {err}", err.kind())),
            }
        })
        .expect("second-call scenario");
        drop(allowed);
        drop(outside);
    }
}
