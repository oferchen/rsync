//! Test-only seams into fast_io's trust decisions.
//!
//! This module exists only in fast_io's own unit tests and in builds that
//! enable the `test-seams` feature. Crates turn that feature on from
//! `[dev-dependencies]` alone, and `tools/ci/test_seam_gate.py` fails CI if a
//! production dependency table, a `[features]` entry or non-test code ever
//! does.
//!
//! A shipped build cannot reach it: the release and packaging builds name
//! their features explicitly and never include `test-seams`. Even in a test
//! build that compiles it in (for example `--all-features`), the only way to
//! arm it is to call [`judge_symlink_owner`] from Rust. There is no
//! environment variable, command-line flag, configuration key or file that
//! does it, so no external input can change what the walk decides.

use std::ffi::{OsStr, OsString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Fast path for the walk: false whenever no override is armed, so an
/// unarmed walk never takes the lock.
static ARMED: AtomicBool = AtomicBool::new(false);

/// The armed override: a symlink component name and the owner to report for it.
static OVERRIDE: Mutex<Option<(OsString, u32)>> = Mutex::new(None);

fn slot() -> MutexGuard<'static, Option<(OsString, u32)>> {
    OVERRIDE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Keeps a symlink-owner override armed; dropping it restores real owners.
///
/// Test-only. See the module documentation for why it is inert in shipped
/// builds.
#[must_use = "the override is disarmed as soon as the guard is dropped"]
#[derive(Debug)]
pub struct SymlinkOwnerGuard {
    _private: (),
}

impl Drop for SymlinkOwnerGuard {
    fn drop(&mut self) {
        let mut armed = slot();
        *armed = None;
        ARMED.store(false, Ordering::Release);
    }
}

/// Makes every owner walk judge a symlink component named `component` as
/// owned by `uid`, until the returned guard is dropped.
///
/// The override is process-global, so it also applies to walks run on other
/// threads, such as a transfer's worker threads. nextest runs each test in its
/// own process, so tests do not see each other's overrides.
///
/// Only symlinks are affected: a planted symlink with that name is judged as
/// owned by `uid` instead of its real owner, which lets an unprivileged test
/// plant a "foreign-owned" link. Components that are not symlinks, and
/// symlinks with other names, are judged as before.
///
/// Test-only. See the module documentation for why it is inert in shipped
/// builds.
///
/// # Panics
///
/// Panics if an override is already armed. Drop the first guard before arming
/// a second one.
pub fn judge_symlink_owner(component: impl Into<OsString>, uid: u32) -> SymlinkOwnerGuard {
    let mut armed = slot();
    if armed.is_some() {
        drop(armed);
        panic!("a symlink-owner override is already armed; drop its guard before arming another");
    }
    *armed = Some((component.into(), uid));
    ARMED.store(true, Ordering::Release);
    SymlinkOwnerGuard { _private: () }
}

/// The owner armed for a symlink component named `name`, if any.
pub(crate) fn judged_owner(name: &OsStr) -> Option<u32> {
    if !ARMED.load(Ordering::Acquire) {
        return None;
    }
    slot()
        .as_ref()
        .filter(|(component, _)| component.as_os_str() == name)
        .map(|(_, uid)| *uid)
}

#[cfg(test)]
mod tests {
    use super::{judge_symlink_owner, judged_owner};
    use crate::owner_walk::operator_open_basis_read;
    use std::ffi::OsStr;
    use std::path::Path;

    fn foreign_uid() -> u32 {
        let euid = rustix::process::geteuid().as_raw();
        (1..=8u32)
            .find(|candidate| *candidate != euid)
            .expect("at least one of uids 1..=8 differs from the euid")
    }

    /// `root/alt` is a symlink to `root/real`, which holds `f`.
    fn plant(root: &Path) -> std::path::PathBuf {
        let real = root.join("real");
        std::fs::create_dir(&real).expect("real dir");
        std::fs::write(real.join("f"), b"DATA\n").expect("write");
        std::os::unix::fs::symlink(&real, root.join("alt")).expect("symlink");
        root.join("alt").join("f")
    }

    #[test]
    fn an_armed_override_reports_its_owner_for_the_named_component_only() {
        let guard = judge_symlink_owner("alt", 7);
        assert_eq!(judged_owner(OsStr::new("alt")), Some(7));
        assert_eq!(judged_owner(OsStr::new("other")), None);
        drop(guard);
    }

    /// Transfers walk on worker threads, so an override armed on the test
    /// thread must reach a walk run on another thread. A thread-local could
    /// not do this.
    #[test]
    fn a_walk_on_another_thread_sees_the_override() {
        let temp = tempfile::tempdir().expect("tempdir");
        let through_link = plant(temp.path());
        let _owner = judge_symlink_owner("alt", foreign_uid());

        let refused = std::thread::spawn(move || operator_open_basis_read(&through_link))
            .join()
            .expect("walk thread");

        let error = refused.expect_err("a foreign-owned parent symlink must be refused");
        assert_eq!(error.raw_os_error(), Some(libc::ELOOP));
    }

    /// Dropping the guard puts the real owner back: the same walk that was
    /// refused while the override was armed follows the link again.
    #[test]
    fn dropping_the_guard_restores_the_real_owner() {
        let temp = tempfile::tempdir().expect("tempdir");
        let through_link = plant(temp.path());

        let owner = judge_symlink_owner("alt", foreign_uid());
        assert!(operator_open_basis_read(&through_link).is_err());
        drop(owner);

        assert_eq!(judged_owner(OsStr::new("alt")), None);
        operator_open_basis_read(&through_link).expect("the operator's own link is followed again");
    }

    #[test]
    #[should_panic(expected = "already armed")]
    fn arming_a_second_override_while_one_is_live_panics() {
        let _first = judge_symlink_owner("alt", 7);
        let _second = judge_symlink_owner("other", 8);
    }
}
