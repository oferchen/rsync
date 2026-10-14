// Linux process-capability defense-in-depth helpers.
//
// Deviation from upstream: rsync never touches Linux capabilities. This
// section is one of three oc-only hardening layers:
//
// 1. `PR_SET_NO_NEW_PRIVS` (`hardening.rs`) makes any later `execve()`
//    ineligible for privilege escalation.
// 2. Capability dropping (this section) strips the daemon of every Linux
//    capability it does not need.
// 3. The seccomp BPF filter (`seccomp.rs`) narrows the syscall surface.
//
// Capabilities handled (see `docs/design/lsm-cap-required-capabilities.md`):
//
// - `CAP_NET_BIND_SERVICE`: needed only to bind ports below 1024. Dropped once
//   the listener is bound (`drop_cap_net_bind_service`).
// - `CAP_CHOWN`: needed by a module with `uid = 0`. The startup check
//   (`preflight_required_capabilities`) fails loudly when it is missing
//   instead of producing per-file `chown` errors later.
// - Everything else is dropped before a non-root worker serves a module
//   (`drop_worker_capabilities`). A root worker keeps its full set.
//
// On non-Linux targets every helper is a no-op, so call sites need no `#[cfg]`.
// The `caps` crate wraps `prctl(PR_CAPBSET_DROP)` and `capset(2)`.

/// Whether Linux capability support is compiled in.
#[cfg(target_os = "linux")]
#[allow(dead_code)]
const CAP_FEATURE_AVAILABLE: bool = true;

/// Stub flag for non-Linux platforms.
#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
const CAP_FEATURE_AVAILABLE: bool = false;

/// Pre-flight check: verify the daemon holds the capabilities its configuration
/// requires.
///
/// Called once at daemon startup, before the listener binds. Only `CAP_CHOWN`
/// is checked: when any loaded module has `uid = 0` the capability must be in
/// the effective set. Without it `--owner` / `--group` / `--chown` would fail
/// later as confusing per-file `chown` errors instead of a clean startup error.
///
/// Returns `Ok(())` when the daemon either does not need elevated
/// capabilities or holds every capability it needs. Returns `Err(reason)` with
/// a multi-line message that includes operator-facing remediation steps when a
/// required capability is missing.
///
/// On non-Linux targets this is a no-op that always returns `Ok(())`.
#[cfg(target_os = "linux")]
fn preflight_required_capabilities(modules: &[ModuleRuntime]) -> Result<(), String> {
    use caps::{CapSet, Capability};

    if !requires_chown_capability(modules) {
        return Ok(());
    }

    let has_cap = caps::has_cap(None, CapSet::Effective, Capability::CAP_CHOWN).unwrap_or(false);
    if has_cap {
        return Ok(());
    }

    let names: Vec<&str> = modules
        .iter()
        .filter(|module| module_requires_chown(module))
        .map(|module| module.name.as_str())
        .collect();
    let modules_listing = if names.is_empty() {
        String::from("daemon-level uid=root configuration")
    } else {
        format!("module(s) {}", names.join(", "))
    };

    Err(format!(
        "rsyncd.conf {modules_listing} requires CAP_CHOWN but this capability is not granted.\n\
         Grant via:\n\
           - systemd: AmbientCapabilities=CAP_CHOWN\n\
           - setcap:  setcap cap_chown=eip /usr/sbin/oc-rsyncd\n\
           - docker:  --cap-add=CHOWN"
    ))
}

/// Stub for platforms without Linux capabilities.
#[cfg(not(target_os = "linux"))]
fn preflight_required_capabilities(_modules: &[ModuleRuntime]) -> Result<(), String> {
    Ok(())
}

/// Returns true when at least one configured module requires `CAP_CHOWN`.
#[cfg(target_os = "linux")]
fn requires_chown_capability(modules: &[ModuleRuntime]) -> bool {
    modules.iter().any(module_requires_chown)
}

/// Returns true when the module is configured with `uid = 0`, so its worker
/// may change file ownership and therefore needs `CAP_CHOWN`.
#[cfg(target_os = "linux")]
fn module_requires_chown(module: &ModuleRuntime) -> bool {
    matches!(module.uid, Some(0))
}

/// Returns whether the calling process currently has an effective uid of 0.
///
/// Used to detect "module has no explicit `uid` directive and the daemon
/// process is running as root" - the case where the per-module privilege
/// drop leaves the worker inheriting root from the daemon. Calling
/// `nix::unistd::geteuid` here avoids racing with any partial setuid the
/// privilege-drop path may have just performed.
#[cfg(target_os = "linux")]
fn current_euid_is_zero() -> bool {
    nix::unistd::geteuid().is_root()
}

/// Drops `CAP_NET_BIND_SERVICE` from the daemon's effective and permitted
/// capability sets.
///
/// Called once after the listener has bound to its port(s). A compromised
/// worker therefore cannot rebind another privileged port (rebinding 80, 443,
/// or 22 to intercept traffic is a classic post-exploitation move). The call
/// also drops the capability from the bounding set so any later `execve()` of
/// a setcap binary cannot regain it.
///
/// Failures are logged at warning level but do not abort startup: the
/// daemon's primary defenses (Landlock, seccomp, chroot) still apply.
///
/// On non-Linux targets this is a no-op.
#[cfg(target_os = "linux")]
fn drop_cap_net_bind_service(log_sink: Option<&SharedLogSink>) {
    use caps::{CapSet, Capability};

    let target = Capability::CAP_NET_BIND_SERVICE;
    let already_absent = !caps::has_cap(None, CapSet::Permitted, target).unwrap_or(false);
    if already_absent {
        if let Some(log) = log_sink {
            let message = rsync_info!("CAP_NET_BIND_SERVICE already absent; nothing to drop")
                .with_role(Role::Daemon);
            log_message(log, &message);
        }
        return;
    }

    for set in [CapSet::Effective, CapSet::Permitted, CapSet::Bounding] {
        if let Err(err) = caps::drop(None, set, target)
            && let Some(log) = log_sink
        {
            let text = format!("failed to drop CAP_NET_BIND_SERVICE from {set:?}: {err}");
            let message = rsync_warning!(text).with_role(Role::Daemon);
            log_message(log, &message);
        }
    }

    if let Some(log) = log_sink {
        let message = rsync_info!("dropped CAP_NET_BIND_SERVICE post-bind").with_role(Role::Daemon);
        log_message(log, &message);
    }
}

/// Stub for non-Linux platforms.
#[cfg(not(target_os = "linux"))]
fn drop_cap_net_bind_service(_log_sink: Option<&SharedLogSink>) {}

/// Drops every capability not in the per-module required set for the worker
/// path.
///
/// Called once per session after the privilege drop and before
/// `engage_landlock_sandbox` (`module_access/transfer/orchestration.rs`).
/// Every permitted capability not in the required set is dropped from the
/// effective, permitted and bounding sets, so the worker cannot reacquire it
/// via `capset(2)` or a setcap `execve()`.
///
/// A root worker (module `uid = 0`, or no module `uid` with a root daemon)
/// skips the drop entirely, so in practice the required set is empty
/// whenever a drop happens. The inventory in
/// `docs/design/lsm-cap-required-capabilities.md` lists every capability the
/// daemon can request and its gating condition.
///
/// Failures are logged at warning level but do not abort the connection.
///
/// On non-Linux targets this is a no-op.
#[cfg(target_os = "linux")]
fn drop_worker_capabilities(module: &ModuleRuntime, log_sink: Option<&SharedLogSink>) {
    use caps::{CapSet, Capability};

    // Skip the per-worker drop whenever the worker is still running as
    // root - either because the module is configured with `uid = 0`, or
    // because no per-module `uid` was set and the daemon-wide identity
    // (which the worker inherits when the per-module drop is skipped) is
    // root. The kernel grants root all capabilities implicitly; dropping
    // them here would strip CAP_DAC_OVERRIDE / CAP_DAC_READ_SEARCH /
    // CAP_FOWNER / CAP_FSETID and leave a root worker EACCES-ing module
    // trees that live under unprivileged user homedirs (the runtests.py
    // scratch under `/home/<user>/...` with mode 750 was the smoking gun).
    // Upstream rsync does not drop capabilities from a root daemon, so
    // skipping the drop here preserves behavioural parity. LSM-CAP is a
    // defense-in-depth layer for the unprivileged-uid case; for uid=0
    // the operator has explicitly chosen the root identity and the
    // capability surface that comes with it.
    let worker_is_root = match module.uid {
        Some(0) => true,
        None => current_euid_is_zero(),
        Some(_) => false,
    };
    if worker_is_root {
        if let Some(log) = log_sink {
            let text = format!(
                "module '{}': skipping worker capability drop (root worker keeps the kernel's implicit capability set)",
                module.name,
            );
            let message = rsync_info!(text).with_role(Role::Daemon);
            log_message(log, &message);
        }
        return;
    }

    let required = required_capabilities_for_module(module);

    // Drop every capability in the permitted set that is not required. Reading
    // the live set rather than enumerating names means capabilities added by
    // newer kernels are dropped without code changes.
    let all_caps = match caps::read(None, CapSet::Permitted) {
        Ok(caps) => caps,
        Err(err) => {
            if let Some(log) = log_sink {
                let text =
                    format!("failed to read permitted capability set for worker drop: {err}");
                let message = rsync_warning!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
            return;
        }
    };

    let mut dropped: Vec<Capability> = Vec::new();
    for cap in all_caps {
        if required.contains(&cap) {
            continue;
        }
        for set in [CapSet::Effective, CapSet::Permitted, CapSet::Bounding] {
            if let Err(err) = caps::drop(None, set, cap)
                && let Some(log) = log_sink
            {
                let text = format!("failed to drop {cap:?} from {set:?}: {err}");
                let message = rsync_warning!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
        }
        dropped.push(cap);
    }

    if let Some(log) = log_sink {
        let text = format!(
            "module '{}': dropped {} capability/ies for worker (retained: {:?})",
            module.name,
            dropped.len(),
            required,
        );
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }
}

/// Stub for non-Linux platforms.
#[cfg(not(target_os = "linux"))]
fn drop_worker_capabilities(_module: &ModuleRuntime, _log_sink: Option<&SharedLogSink>) {}

/// Returns the set of capabilities the worker process needs to retain after
/// the per-worker drop.
///
/// Workers run after chroot and setuid, where most operations need no
/// capability, so the set is empty for a non-root module. A `uid = 0` module
/// keeps the file-system bypass set the kernel grants root implicitly:
/// dropping `CAP_DAC_OVERRIDE` / `CAP_DAC_READ_SEARCH` would leave a root
/// worker unable to write into directories owned by unprivileged users (every
/// push into a 750-mode home directory returned EACCES even though the worker
/// uid was 0). Upstream rsync drops no capabilities, so retaining them keeps
/// this a defense-in-depth layer rather than a behavioural break.
///
/// `CAP_FOWNER` covers `chmod` / `utime` against files the worker did not
/// create (mirroring upstream's metadata-restore path), and `CAP_FSETID` is
/// needed to preserve setuid/setgid bits across the chmod that follows a
/// fresh open. Together with `CAP_CHOWN` for `--owner`/`--group`/`--chown`,
/// this matches the implicit capability set a root daemon would have if no
/// drop happened at all.
#[cfg(target_os = "linux")]
fn required_capabilities_for_module(
    module: &ModuleRuntime,
) -> std::collections::HashSet<caps::Capability> {
    use caps::Capability;
    use std::collections::HashSet;

    let mut required = HashSet::new();
    if module_requires_chown(module) {
        required.insert(Capability::CAP_CHOWN);
        required.insert(Capability::CAP_DAC_OVERRIDE);
        required.insert(Capability::CAP_DAC_READ_SEARCH);
        required.insert(Capability::CAP_FOWNER);
        required.insert(Capability::CAP_FSETID);
    }
    required
}

#[cfg(all(test, target_os = "linux"))]
mod capabilities_tests {
    use super::*;

    fn module_with(name: &str, uid: Option<u32>) -> ModuleRuntime {
        let def = ModuleDefinition {
            name: name.to_owned(),
            path: std::path::PathBuf::from("/tmp"),
            uid,
            ..Default::default()
        };
        ModuleRuntime::new(def, None)
    }

    // CAP_FEATURE_AVAILABLE is a cfg-gated compile-time constant; the
    // surrounding `cfg(all(test, target_os = "linux"))` already proves the
    // feature is compiled in on Linux, so a runtime assertion against the
    // constant adds no signal and clippy strips it as dead code.

    #[test]
    fn preflight_passes_when_no_module_requires_chown() {
        let modules = vec![module_with("public", Some(1000))];
        assert!(preflight_required_capabilities(&modules).is_ok());
    }

    #[test]
    fn module_with_uid_root_requires_chown() {
        let module = module_with("uploads", Some(0));
        assert!(module_requires_chown(&module));
    }

    #[test]
    fn module_with_unprivileged_uid_does_not_require_chown() {
        let module = module_with("readonly", Some(1000));
        assert!(!module_requires_chown(&module));
    }

    #[test]
    fn required_capabilities_empty_for_unprivileged_module() {
        let module = module_with("readonly", Some(1000));
        let required = required_capabilities_for_module(&module);
        assert!(required.is_empty());
    }

    #[test]
    fn required_capabilities_include_chown_for_root_module() {
        use caps::Capability;
        let module = module_with("uploads", Some(0));
        let required = required_capabilities_for_module(&module);
        assert!(required.contains(&Capability::CAP_CHOWN));
    }

    /// A root-mode module must retain the kernel's implicit DAC bypass set so
    /// the worker can read and write into module trees that live under
    /// directories owned by unprivileged users. Without DAC_OVERRIDE the
    /// runtests.py scratch under `/home/<user>/...` (homedir mode 750) returns
    /// EACCES on the first `openat(O_PATH)`; the same scenario hits any
    /// daemon module configured with `path = /var/.../user/...`.
    #[test]
    fn required_capabilities_include_dac_bypass_for_root_module() {
        use caps::Capability;
        let module = module_with("uploads", Some(0));
        let required = required_capabilities_for_module(&module);
        assert!(required.contains(&Capability::CAP_DAC_OVERRIDE));
        assert!(required.contains(&Capability::CAP_DAC_READ_SEARCH));
        assert!(required.contains(&Capability::CAP_FOWNER));
        assert!(required.contains(&Capability::CAP_FSETID));
    }

    /// The `drop_cap_net_bind_service` helper must be safe to call even when
    /// the capability is already absent. CI runners and unprivileged developer
    /// environments fall into this category, so the helper logs and returns
    /// without error rather than panicking.
    #[test]
    fn drop_cap_net_bind_service_handles_absent_capability() {
        // The capability is unlikely to be held by the nextest harness; the
        // call must complete without panicking regardless of the initial set.
        drop_cap_net_bind_service(None);
    }

    /// The `drop_worker_capabilities` helper must complete without panicking
    /// when invoked from an unprivileged test process. Workers in CI almost
    /// never hold the full permitted set, so the function must tolerate
    /// `caps::drop` failing for capabilities that were never granted.
    #[test]
    fn drop_worker_capabilities_handles_unprivileged_caller() {
        let module = module_with("unprivileged", Some(1000));
        drop_worker_capabilities(&module, None);
    }

    /// Verifies the operator-facing pre-flight error contains all three
    /// remediation paths (systemd, setcap, docker) so packagers can grep for
    /// them in CI smoke tests without screen-scraping a free-form sentence.
    /// The test confirms the error shape that
    /// `docs/packaging/landlock-feature-guidance.md` advertises to operators.
    #[test]
    fn preflight_error_lists_three_remediation_paths_when_chown_required() {
        let modules = vec![module_with("uploads", Some(0))];
        // The check only fires when CAP_CHOWN is not held; tests run
        // unprivileged, so the error path is exercised reliably.
        let has_chown = caps::has_cap(None, caps::CapSet::Effective, caps::Capability::CAP_CHOWN)
            .unwrap_or(false);
        if has_chown {
            // Granted in the test environment (rare): skip the assertion.
            return;
        }
        let err = preflight_required_capabilities(&modules)
            .expect_err("expect missing CAP_CHOWN to surface a remediation message");
        assert!(err.contains("CAP_CHOWN"), "error must name the capability");
        assert!(err.contains("systemd: AmbientCapabilities=CAP_CHOWN"));
        assert!(err.contains("setcap:"));
        assert!(err.contains("docker:"));
        assert!(
            err.contains("uploads"),
            "error must name the offending module"
        );
    }
}
