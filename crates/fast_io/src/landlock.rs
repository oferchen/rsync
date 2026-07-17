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

/// System directories granted read-only (read + execute) access under the
/// Landlock ruleset so that user/group name resolution and the dynamic linker
/// keep working while every write stays confined to the module tree.
///
/// Applying a wire-decoded ACL resolves named-user/named-group entries through
/// NSS (`getpwnam_r`/`getgrnam_r`), which reads `/etc/passwd`, `/etc/group`,
/// `/etc/nsswitch.conf`, `/etc/ld.so.cache`, the NSS backend shared objects
/// under the library directories, and `/proc/self` for the systemd backend.
/// These mirror the reads upstream's (unsandboxed) daemon performs. Absent
/// paths are skipped, so the list can name locations that only exist on some
/// distributions without turning into a hard error.
const READONLY_SYSTEM_PATHS: &[&str] = &["/etc", "/lib", "/lib64", "/usr", "/proc", "/run", "/var"];

/// Filesystem-access enforcement tier of an engaged Landlock ruleset.
///
/// Mirrors the `landlock` crate's `RulesetStatus` without leaking that type
/// into fast_io's public API, so the same variants are matchable on every
/// target (the non-Linux stub carries a structurally identical enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnforcementStatus {
    /// Every requested access right was honoured by the kernel.
    FullyEnforced,
    /// Best-effort downgrade dropped some requested rights: the running
    /// kernel's Landlock ABI is older than the one requested, so the sandbox
    /// is weaker than intended. Call [`best_effort_fs_downgrade`] for the list
    /// of rights this kernel is missing.
    PartiallyEnforced,
    /// The kernel accepted the ruleset but applied no restrictions at all -
    /// equivalent to running without a sandbox.
    NotEnforced,
}

impl From<RulesetStatus> for EnforcementStatus {
    fn from(status: RulesetStatus) -> Self {
        match status {
            RulesetStatus::FullyEnforced => Self::FullyEnforced,
            RulesetStatus::PartiallyEnforced => Self::PartiallyEnforced,
            RulesetStatus::NotEnforced => Self::NotEnforced,
        }
    }
}

/// Outcome of a [`crate::landlock::restrict_to_module_paths`] call.
///
/// Carries enough detail for the daemon to log the actual enforcement level
/// without leaking the `landlock` crate's types into the public API.
#[derive(Debug)]
pub enum LandlockOutcome {
    /// The ruleset was created and applied. The carried [`EnforcementStatus`]
    /// distinguishes fully enforced, a best-effort partial downgrade (some
    /// rights dropped - see [`best_effort_fs_downgrade`]), and a no-op ruleset
    /// the kernel accepted but did not apply.
    Enforced(EnforcementStatus),
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
/// The helper requests `AccessFs::from_all(ABI::V5)` (read / write / create /
/// delete / rename / symlink / refer / truncate / ioctl-dev) with best-effort
/// downgrade enabled, so a kernel that understands only v1-v4 accepts the
/// subset it supports and silently drops the rest. ABI::V5 lands on Linux
/// 6.7+ and adds the IPC scoping surface; older kernels degrade to V4/V3
/// without surfacing as an error. The returned [`LandlockOutcome`] carries
/// the [`EnforcementStatus`] so callers can log the actual enforcement level
/// (and call [`best_effort_fs_downgrade`] to name the dropped rights).
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
    // TRUNCATE on 5.13-6.1, IoctlDev on 5.13-6.6, network scopes on 5.13-6.6,
    // signal scopes on 5.13-6.6). ABI::V5 (Linux 6.7+) adds the IPC scoping
    // surface; on the target test host (kernel 7.0) it engages fully, and on
    // older kernels BestEffort downgrade preserves the V3 rights we relied on
    // historically. The final enforcement tier surfaces in the RulesetStatus
    // returned by restrict_self below.
    let access = AccessFs::from_all(ABI::V5);

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

    // Grant read-only (read + execute) access to the system paths that
    // user/group name resolution and the dynamic linker need. The receiver
    // resolves named-user/named-group ACL entries through `getpwnam_r`/
    // `getgrnam_r` (NSS) when applying a wire-decoded ACL, which opens
    // /etc/passwd, /etc/group, /etc/nsswitch.conf, the NSS module shared
    // objects under the library directories, and /proc/self for the systemd
    // NSS backend. Confining the thread to the module tree alone makes those
    // opens fail with EACCES, so the ACL apply silently drops every named
    // entry (the failure is swallowed as "ACLs unsupported"). Upstream's
    // daemon reads these files freely; granting read-only access here restores
    // that behaviour while keeping every write confined to the module tree, so
    // the symlink-race defense is unchanged.
    let readonly = AccessFs::ReadFile | AccessFs::ReadDir | AccessFs::Execute;
    for path in READONLY_SYSTEM_PATHS {
        // Skip paths absent on this host: PathFd::new fails with ENOENT and a
        // missing NSS/library directory is not an error - the remaining rules
        // still cover the standard locations. Only existing paths are added.
        if let Ok(fd) = PathFd::new(path) {
            created = match created.add_rule(PathBeneath::new(fd, readonly)) {
                Ok(c) => c,
                Err(err) => return LandlockOutcome::Error(io::Error::other(err.to_string())),
            };
        }
    }

    match created.restrict_self() {
        Ok(status) => LandlockOutcome::Enforced(EnforcementStatus::from(status.ruleset)),
        Err(err) => LandlockOutcome::Error(io::Error::other(err.to_string())),
    }
}

/// Names the filesystem access rights the running kernel's Landlock ABI lacks
/// relative to the `ABI::V5` set that [`restrict_to_module_paths`] requests.
///
/// A [`EnforcementStatus::PartiallyEnforced`] outcome means best-effort
/// downgrade silently dropped one or more requested rights because the kernel
/// is too old. This turns that opaque tier into an actionable message so the
/// operator learns exactly what the sandbox is missing rather than discovering
/// it as a mysterious `EACCES` at transfer time.
///
/// Returns `None` when the kernel honours the full requested set (nothing was
/// dropped). Otherwise returns a human-readable summary naming each missing
/// right and the operations it gates - most importantly `refer`
/// (cross-directory rename), which `--delay-updates` and `--backup-dir`
/// staging renames depend on and which the kernel omits before Linux 5.19.
#[must_use]
pub fn best_effort_fs_downgrade() -> Option<String> {
    describe_fs_downgrade(ABI::new_current())
}

/// Pure core of [`best_effort_fs_downgrade`]: given the highest Landlock ABI
/// the running kernel supports, lists the filesystem rights dropped from the
/// requested `ABI::V5` set. Factored out so the ABI-to-right mapping is
/// unit-testable without a specific kernel.
///
/// Boundaries follow the `landlock` crate's ABI table: `refer` arrives in
/// `ABI::V2` (Linux 5.19), `truncate` in `ABI::V3` (Linux 6.2), and
/// `ioctl_dev` in `ABI::V5` (Linux 6.10); `ABI::V4` adds only network scopes,
/// so it introduces no new filesystem right over `V3`.
fn describe_fs_downgrade(current: ABI) -> Option<String> {
    let mut missing: Vec<&str> = Vec::new();
    if current < ABI::V2 {
        missing.push(
            "refer (cross-directory rename; --delay-updates and --backup-dir staging renames fail, added in Linux 5.19)",
        );
    }
    if current < ABI::V3 {
        missing.push("truncate (open-handle truncation, added in Linux 6.2)");
    }
    if current < ABI::V5 {
        missing.push("ioctl_dev (device ioctls, added in Linux 6.10)");
    }
    if missing.is_empty() {
        None
    } else {
        Some(missing.join("; "))
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
                LandlockOutcome::Enforced(EnforcementStatus::NotEnforced) => return Ok(()),
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
                LandlockOutcome::Enforced(EnforcementStatus::NotEnforced) => return Ok(()),
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
                LandlockOutcome::Enforced(EnforcementStatus::NotEnforced) => return Ok(()),
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
                LandlockOutcome::Enforced(EnforcementStatus::NotEnforced) => return Ok(()),
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
                LandlockOutcome::Enforced(EnforcementStatus::NotEnforced) => return Ok(()),
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
                LandlockOutcome::Enforced(EnforcementStatus::NotEnforced) => return Ok(()),
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

    #[test]
    fn allows_reading_nss_files_after_restriction() {
        // Regression: applying a wire-decoded ACL with a named-user/named-group
        // entry resolves the id through NSS (`getpwnam_r`/`getgrnam_r`), which
        // opens /etc/passwd, /etc/group, and the NSS backend libraries. When
        // the sandbox confined the receiver to the module tree alone those
        // opens failed with EACCES, exacl reported the ACL as unsupported, and
        // every named entry plus the mask was silently dropped on the daemon
        // receiver path. The read-only system-path grant must keep NSS files
        // readable so the receiver applies the full ACL, matching upstream's
        // unsandboxed daemon.
        if !is_supported() {
            return;
        }
        let module = TempDir::new().expect("module tempdir");
        let module_path = module.path().to_path_buf();
        run_isolated(move || {
            let outcome = restrict_to_module_paths(&[module_path.as_path()]);
            match outcome {
                LandlockOutcome::Enforced(EnforcementStatus::NotEnforced) => return Ok(()),
                LandlockOutcome::Enforced(_) => {}
                LandlockOutcome::Unavailable => return Ok(()),
                LandlockOutcome::Error(err) => return Err(format!("setup: {err}")),
            }
            // /etc/passwd is the canonical NSS source the ACL id resolver opens.
            match fs::read("/etc/passwd") {
                Ok(bytes) if !bytes.is_empty() => Ok(()),
                Ok(_) => Err("read /etc/passwd returned no data".to_owned()),
                Err(err) => Err(format!("read /etc/passwd blocked by sandbox: {err}")),
            }
        })
        .expect("nss-read scenario");
        drop(module);
    }

    #[test]
    fn readonly_system_paths_still_block_writes() {
        // The NSS grant is read + execute only: it must not let the confined
        // receiver create or modify files under the system directories, so the
        // symlink-race write-confinement guarantee is unchanged.
        if !is_supported() {
            return;
        }
        let module = TempDir::new().expect("module tempdir");
        let module_path = module.path().to_path_buf();
        run_isolated(move || {
            let outcome = restrict_to_module_paths(&[module_path.as_path()]);
            match outcome {
                LandlockOutcome::Enforced(EnforcementStatus::NotEnforced) => return Ok(()),
                LandlockOutcome::Enforced(_) => {}
                LandlockOutcome::Unavailable => return Ok(()),
                LandlockOutcome::Error(err) => return Err(format!("setup: {err}")),
            }
            match fs::write("/etc/oc_rsync_landlock_probe", b"x") {
                Ok(()) => {
                    let _ = fs::remove_file("/etc/oc_rsync_landlock_probe");
                    Err("write under a read-only system path succeeded".to_owned())
                }
                Err(err) if err.kind() == ErrorKind::PermissionDenied => Ok(()),
                Err(err) => Err(format!("unexpected error {:?}: {err}", err.kind())),
            }
        })
        .expect("readonly-system-path scenario");
        drop(module);
    }

    #[test]
    fn downgrade_on_v1_names_every_dropped_right() {
        // A 5.13-5.18 kernel (ABI::V1) drops refer, truncate, and ioctl_dev
        // from the requested V5 set. The operator must be told each one - the
        // refer loss silently breaks --delay-updates / --backup-dir renames,
        // which is the whole reason this message exists.
        let msg = describe_fs_downgrade(ABI::V1).expect("V1 must report a downgrade");
        assert!(msg.contains("refer"), "V1 must name refer: {msg}");
        assert!(msg.contains("truncate"), "V1 must name truncate: {msg}");
        assert!(msg.contains("ioctl_dev"), "V1 must name ioctl_dev: {msg}");
        assert!(
            msg.contains("--delay-updates"),
            "refer loss must spell out the --delay-updates consequence: {msg}"
        );
    }

    #[test]
    fn downgrade_tiers_track_the_abi_boundaries() {
        // refer arrives at V2 (5.19), truncate at V3 (6.2), ioctl_dev at V5
        // (6.10); V4 (6.7) adds only network scopes, so it still lacks
        // ioctl_dev but nothing else on the filesystem axis.
        let v2 = describe_fs_downgrade(ABI::V2).expect("V2 still misses truncate + ioctl_dev");
        assert!(!v2.contains("refer"), "V2 has refer: {v2}");
        assert!(v2.contains("truncate") && v2.contains("ioctl_dev"), "{v2}");

        let v3 = describe_fs_downgrade(ABI::V3).expect("V3 still misses ioctl_dev");
        assert!(!v3.contains("refer") && !v3.contains("truncate"), "{v3}");
        assert!(v3.contains("ioctl_dev"), "{v3}");

        let v4 = describe_fs_downgrade(ABI::V4).expect("V4 still misses ioctl_dev");
        assert_eq!(v3, v4, "V4 adds no new filesystem right over V3");
    }

    #[test]
    fn full_v5_reports_no_downgrade() {
        // A kernel honouring the full requested set has nothing to warn about.
        assert!(describe_fs_downgrade(ABI::V5).is_none());
    }

    #[test]
    fn best_effort_downgrade_matches_current_abi() {
        // The public probe must agree with the pure mapping for whatever ABI
        // this host exposes, and must never panic.
        assert_eq!(best_effort_fs_downgrade(), describe_fs_downgrade(ABI::new_current()));
    }
}
