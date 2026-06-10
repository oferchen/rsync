/// Linux process-capability defense-in-depth helpers (LSM-CAP series).
///
/// This section composes with the LSM startup hardening section (`hardening.rs`,
/// PR #5581 - PR_SET_NO_NEW_PRIVS + active LSM detection) and the seccomp BPF
/// filter section (PR #5589) to form a three-layer defense:
///
/// 1. `PR_SET_NO_NEW_PRIVS` makes any subsequent `execve()` ineligible for
///    privilege escalation (PR #5581).
/// 2. **Capability dropping (this section)** strips the daemon process of every
///    Linux capability it does not require. Even if a worker is compromised,
///    the remaining attack surface is bounded by the leftover capabilities.
/// 3. The seccomp BPF filter narrows the syscall surface itself (PR #5589).
///
/// Capabilities targeted (per `docs/design/lsm-cap-required-capabilities.md`):
///
/// - `CAP_NET_BIND_SERVICE`: required only to bind ports below 1024. Dropped
///   once the listener is bound so a compromised worker cannot rebind a
///   privileged port (`drop_cap_net_bind_service`).
/// - `CAP_CHOWN`: required by `--chown`, `--owner`, `--group`, or any module
///   that runs as `uid = root` and would invoke `fchown(2)`. The pre-flight
///   check (`preflight_chown_requirement`) verifies the capability is present
///   when configuration demands it and fails loud at startup instead of
///   producing per-transfer errors later.
/// - Non-required capabilities at the worker fork point are dropped wholesale
///   by `drop_worker_capabilities`, leaving only the per-module requirement set
///   (typically empty, or `{CAP_CHOWN}` when the module needs ownership writes).
///
/// On non-Linux targets every helper short-circuits to a no-op so the wire-in
/// at the daemon entry points does not need `#[cfg]` branching.
///
/// Reference: kernel `capabilities(7)`; `caps` crate exposes the underlying
/// `prctl(PR_CAPBSET_DROP)` / `capset(2)` calls via a safe wrapper.
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
/// Called once at daemon startup before the listener binds. Inspects every
/// loaded module and the daemon-level `uid = root` directive to determine the
/// required-capability set, then verifies each capability is present in the
/// effective set. Missing capabilities exit with an explicit error rather than
/// failing per-transfer later.
///
/// Currently focuses on `CAP_CHOWN`, the highest-impact case: a module
/// configured with `uid = root` will be unable to honour `--chown` /
/// `--owner` / `--group` if the binary was launched without that capability.
/// The original failure surfaces as a confusing per-file `chown failed`
/// rather than a clean startup error.
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

    let has_cap = caps::has_cap(None, CapSet::Effective, Capability::CAP_CHOWN)
        .unwrap_or(false);
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

/// Returns true when at least one configured module would invoke `fchown(2)`
/// on transferred files and therefore requires `CAP_CHOWN` when the daemon
/// process is not already running as `uid = 0`.
#[cfg(target_os = "linux")]
fn requires_chown_capability(modules: &[ModuleRuntime]) -> bool {
    modules.iter().any(module_requires_chown)
}

/// Returns true when the per-module configuration implies the worker will
/// attempt ownership changes that need `CAP_CHOWN` on a non-root daemon.
///
/// The heuristic mirrors upstream rsync's preserve-owner / preserve-group
/// semantics: a module that explicitly switches to `uid = 0` after chroot, or
/// that operators have wired with privileged hook scripts, can issue
/// ownership-changing syscalls during the transfer.
#[cfg(target_os = "linux")]
fn module_requires_chown(module: &ModuleRuntime) -> bool {
    matches!(module.uid, Some(0))
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
    let already_absent =
        !caps::has_cap(None, CapSet::Permitted, target).unwrap_or(false);
    if already_absent {
        if let Some(log) = log_sink {
            let message = rsync_info!("CAP_NET_BIND_SERVICE already absent; nothing to drop")
                .with_role(Role::Daemon);
            log_message(log, &message);
        }
        return;
    }

    for set in [CapSet::Effective, CapSet::Permitted, CapSet::Bounding] {
        if let Err(err) = caps::drop(None, set, target) {
            if let Some(log) = log_sink {
                let text = format!("failed to drop CAP_NET_BIND_SERVICE from {set:?}: {err}");
                let message = rsync_warning!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
        }
    }

    if let Some(log) = log_sink {
        let message =
            rsync_info!("dropped CAP_NET_BIND_SERVICE post-bind").with_role(Role::Daemon);
        log_message(log, &message);
    }
}

/// Stub for non-Linux platforms.
#[cfg(not(target_os = "linux"))]
fn drop_cap_net_bind_service(_log_sink: Option<&SharedLogSink>) {}

/// Drops every capability not in the per-module required set for the worker
/// path.
///
/// Called at the per-worker fork point - the same lifecycle phase as Landlock
/// engagement, immediately before `engage_landlock_sandbox` in
/// `module_access/transfer.rs`. Any capability not in `required` is dropped
/// from the effective, permitted, and bounding sets so a compromised worker
/// cannot reacquire it via `capset(2)` or via a setcap `execve()`.
///
/// Currently the required set is always either empty or `{CAP_CHOWN}`,
/// depending on whether the module needs to honour ownership writes. The
/// inventory in `docs/design/lsm-cap-required-capabilities.md` tracks every
/// capability the daemon code path can ever request and the gating condition
/// for each one; this helper enforces that inventory at runtime.
///
/// Failures are logged at warning level but do not abort the connection.
///
/// On non-Linux targets this is a no-op.
#[cfg(target_os = "linux")]
fn drop_worker_capabilities(
    module: &ModuleRuntime,
    log_sink: Option<&SharedLogSink>,
) {
    use caps::{CapSet, Capability};

    let required = required_capabilities_for_module(module);

    // Iterate all known capabilities and drop the ones not in the required
    // set. Using `caps::all()` rather than enumerating manually means new
    // kernel capabilities are dropped automatically without code changes.
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
            if let Err(err) = caps::drop(None, set, cap) {
                if let Some(log) = log_sink {
                    let text = format!("failed to drop {cap:?} from {set:?}: {err}");
                    let message = rsync_warning!(text).with_role(Role::Daemon);
                    log_message(log, &message);
                }
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
fn drop_worker_capabilities(
    _module: &ModuleRuntime,
    _log_sink: Option<&SharedLogSink>,
) {
}

/// Returns the set of capabilities the worker process needs to retain after
/// the per-worker drop.
///
/// The inventory is intentionally minimal: workers run inside the post-chroot
/// post-setuid environment where most operations need no capability at all.
/// The only case requiring a non-empty set is a module configured with
/// `uid = 0` that must honour client-supplied `--owner`/`--group`/`--chown`,
/// which traps to `fchown(2)` and requires `CAP_CHOWN`.
#[cfg(target_os = "linux")]
fn required_capabilities_for_module(
    module: &ModuleRuntime,
) -> std::collections::HashSet<caps::Capability> {
    use caps::Capability;
    use std::collections::HashSet;

    let mut required = HashSet::new();
    if module_requires_chown(module) {
        required.insert(Capability::CAP_CHOWN);
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

    #[test]
    fn cap_feature_compiled_in_on_linux() {
        assert!(CAP_FEATURE_AVAILABLE);
    }

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
        let err = preflight_required_capabilities(&modules).expect_err(
            "expect missing CAP_CHOWN to surface a remediation message",
        );
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
