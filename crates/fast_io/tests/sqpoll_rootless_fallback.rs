//! SQP-LAND.6: integration test exercising SQPOLL graceful fallback in a
//! simulated rootless Podman container.
//!
//! In a real rootless container, `IORING_SETUP_SQPOLL` fails with `EPERM`
//! because `CAP_SYS_NICE` is structurally unavailable in the user
//! namespace. SQP-LAND.3 added [`fast_io::detect_rootless_container`] to
//! short-circuit that doomed setup syscall, SQP-LAND.4 wired the helper
//! into [`fast_io::IoUringConfig::build_ring`], and SQP-LAND.7 added the
//! [`fast_io::rootless_signal`] accessor for logging. This test verifies
//! the end-to-end contract: with the rootless override env var set, the
//! public io_uring writer factory returns a working writer (graceful
//! fallback to a non-SQPOLL ring on Linux, standard-I/O on any platform)
//! without panicking and without surfacing an error to the caller.
//!
//! The test uses [`fast_io::FORCE_ROOTLESS_ENV`] to simulate the
//! container so the same code path is exercised on CI runners that are
//! not actually rootless. The env-var override is documented in
//! `docs/deployment-guide.md` (SQP-LAND.8) as a test-only hook; setting
//! it in production is operator-visible and harmless on host systems.
//!
//! This whole file is Linux-only because the SQPOLL safety story is
//! Linux-only. On non-Linux targets the same writer factory path uses
//! [`fast_io::IoUringPolicy::Disabled`] dispatch internally, so there is
//! nothing platform-specific to assert.

#![cfg(target_os = "linux")]

use std::env;
use std::ffi::{OsStr, OsString};
use std::sync::{Mutex, MutexGuard};

use fast_io::{
    FORCE_ROOTLESS_ENV, IoUringPolicy, RootlessSignal, detect_rootless_container,
    is_sqpoll_disabled_by_policy, rootless_signal, writer_from_file,
};
use tempfile::tempdir;

/// Serialises tests in this binary so concurrent env mutations do not
/// race the rootless-detection probe. Cargo gives every `tests/*.rs`
/// file its own binary, isolating this lock from other tests.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Restores a single environment variable to its prior value on drop.
///
/// Mirrors the pattern used in `iouring_probe_fallback_mock.rs` so test
/// authors moving between the two files have a single mental model.
struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &OsStr) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let previous = env::var_os(key);
        // SAFETY: serialised via ENV_LOCK; no other thread mutates this
        // variable for the lifetime of the returned guard.
        #[allow(unsafe_code)]
        unsafe {
            env::set_var(key, value);
        }
        Self {
            key,
            previous,
            _lock: lock,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: ENV_LOCK is still held via `_lock` until this Drop
        // returns, so no other thread observes the in-flight restore.
        #[allow(unsafe_code)]
        unsafe {
            match &self.previous {
                Some(v) => env::set_var(self.key, v),
                None => env::remove_var(self.key),
            }
        }
    }
}

/// With the override set, [`detect_rootless_container`] must report
/// `true` and [`rootless_signal`] must classify the verdict as
/// [`RootlessSignal::NonIdentityUidMap`]. This is the precondition that
/// makes the SQPOLL fall-back branch reachable from the integration
/// test without booting a real rootless container.
#[test]
fn force_rootless_env_reports_rootless_signal() {
    let _g = EnvGuard::set(FORCE_ROOTLESS_ENV, OsStr::new("1"));

    assert!(
        detect_rootless_container(),
        "{FORCE_ROOTLESS_ENV}=1 must make detect_rootless_container() return true"
    );
    assert_eq!(
        rootless_signal(),
        RootlessSignal::NonIdentityUidMap,
        "{FORCE_ROOTLESS_ENV}=1 must surface NonIdentityUidMap so the fall-back \
         site can log a precise reason"
    );
}

/// The override hook must accept the same truthy spellings as the
/// existing `OC_RSYNC_DISABLE_IOURING` gate. Mismatched accepted-values
/// across env hooks would surprise operators following the deployment
/// guide.
#[test]
fn force_rootless_env_accepts_common_truthy_spellings() {
    for value in ["1", "true", "TRUE", "yes", "YES", "on", "On"] {
        let _g = EnvGuard::set(FORCE_ROOTLESS_ENV, OsStr::new(value));
        assert!(
            detect_rootless_container(),
            "{FORCE_ROOTLESS_ENV}={value} must trigger the rootless verdict"
        );
    }
}

/// Falsy values must NOT trip the rootless override. The override is a
/// one-way opt-in; reading it as default-on would silently downgrade
/// SQPOLL on every production host whose operator accidentally exported
/// `OC_RSYNC_FORCE_ROOTLESS_CONTAINER=0` or similar.
///
/// We cannot directly assert `RootlessSignal::NotRootless` because the
/// fall-through path consults the cached host probe, and a CI runner
/// that is itself containerised legitimately fires the probe. The
/// invariant we DO assert is that a falsy override yields the same
/// signal as the very-first call before the override is touched - i.e.
/// the env hook stays inactive.
#[test]
fn force_rootless_env_ignores_falsy_values() {
    // Establish the cached baseline first. The probe runs at most once
    // per process, so subsequent reads return this value regardless of
    // any falsy override.
    let baseline = {
        let _g = EnvGuard::set(FORCE_ROOTLESS_ENV, OsStr::new(""));
        rootless_signal()
    };

    for value in ["0", "false", "FALSE", "no", "off", ""] {
        let _g = EnvGuard::set(FORCE_ROOTLESS_ENV, OsStr::new(value));
        assert_eq!(
            rootless_signal(),
            baseline,
            "{FORCE_ROOTLESS_ENV}={value} must behave the same as an unset/empty value"
        );
    }
}

/// The end-to-end contract: with the override set, the public io_uring
/// writer factory must succeed without panicking, even when the caller
/// asks for [`IoUringPolicy::Auto`] (the production default). The ring
/// must be built without SQPOLL because the rootless check fires before
/// the kernel `EPERM`.
///
/// We cannot directly observe the SQPOLL flag on the resulting ring
/// (the type is opaque), but we can prove the graceful-fallback
/// contract: `writer_from_file` returns `Ok(_)` with the rootless
/// override active. A failure here would mean the rootless skip branch
/// surfaces an error instead of degrading silently - the regression
/// SQP-LAND was designed to prevent.
#[test]
fn writer_from_file_succeeds_under_rootless_override() {
    let _g = EnvGuard::set(FORCE_ROOTLESS_ENV, OsStr::new("1"));

    // Confirm the precondition: the override is in effect.
    assert!(detect_rootless_container());

    let tmpdir = tempdir().expect("tempdir for sqpoll rootless fallback");
    let path = tmpdir.path().join("rootless-fallback.bin");
    let file = std::fs::File::create(&path).expect("create destination file");

    let writer = writer_from_file(file, 4096, IoUringPolicy::Auto)
        .expect("writer_from_file must succeed when rootless override skips SQPOLL");
    drop(writer);

    // The explicit policy gate is a separate mechanism (SQP-K8S.3,
    // --no-io-uring-sqpoll). Confirm we did not accidentally tip it on
    // through the rootless detection path: the gate is for explicit
    // operator opt-out, the rootless skip is automatic.
    assert!(
        !is_sqpoll_disabled_by_policy(),
        "rootless skip must not flip the explicit --no-io-uring-sqpoll gate"
    );
}

/// Even the most aggressive policy that requests SQPOLL must downgrade
/// cleanly under the rootless override. [`IoUringPolicy::SqpollOff`]
/// already routes to a regular ring; the regression-prone path is
/// `Auto`, which would request SQPOLL on a kernel that supports it.
/// Re-running the writer factory under `SqpollOff` proves the
/// override does not destabilise the explicit-opt-out path either.
#[test]
fn writer_from_file_succeeds_under_rootless_with_sqpoll_off_policy() {
    let _g = EnvGuard::set(FORCE_ROOTLESS_ENV, OsStr::new("1"));

    let tmpdir = tempdir().expect("tempdir for sqpoll rootless + policy-off");
    let path = tmpdir.path().join("rootless-sqpoll-off.bin");
    let file = std::fs::File::create(&path).expect("create destination file");

    let writer = writer_from_file(file, 4096, IoUringPolicy::SqpollOff)
        .expect("writer_from_file must succeed under rootless override + SqpollOff policy");
    drop(writer);
}
