//! Observing the process umask for tests that assert permission bits.
//!
//! Two shapes of test assert mode bits, and they need different tools:
//!
//! - Tests that spawn `oc-rsync` as a child pin the child's umask with
//!   [`crate::OcRsyncCliRunner::umask`], so the expectation can be a constant.
//! - Tests that drive the library in-process cannot pin anything: the umask is
//!   process-global and already latched by the time the test runs. Those must
//!   *derive* the expectation from the live umask, which is what this module
//!   provides.
//!
//! Deriving beats pinning here for a second reason: it mirrors what upstream's
//! own testsuite does (`umask 0` plus computed expectations), and it keeps the
//! assertion exact rather than weakening it to "not the unmasked value".

use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// Returns `requested_mode` as the filesystem would actually create it, i.e.
/// masked by the live process umask.
///
/// Use this for any expectation about a mode the *receiver* creates rather than
/// copies. `open(O_CREAT)` and `mkdir` both apply the same process-wide mask,
/// so one helper covers files and directories alike.
///
/// The mask is observed, not computed: creating a probe file exercises exactly
/// the mechanism under test, so the helper cannot disagree with the code it is
/// checking the way a cached `umask(2)` reading could.
///
/// # Panics
///
/// Panics if the probe file cannot be created or stat'd, which would mean the
/// temp directory itself is unusable.
#[must_use]
pub fn umask_masked(requested_mode: u32) -> u32 {
    requested_mode & permitted_permission_bits()
}

/// Returns the permission bits the umask currently leaves through, `~umask & 0o777`.
fn permitted_permission_bits() -> u32 {
    let dir = tempfile::tempdir().expect("create umask probe directory");
    let probe = dir.path().join("umask-probe");
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o777)
        .open(&probe)
        .expect("create umask probe");
    fs::metadata(&probe)
        .expect("stat umask probe")
        .permissions()
        .mode()
        & 0o777
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masking_is_idempotent_and_bounded_by_the_request() {
        let masked = umask_masked(0o777);
        assert_eq!(umask_masked(masked), masked);
        assert_eq!(masked & !0o777, 0);
    }

    #[test]
    fn a_requested_bit_is_never_added_only_removed() {
        // The helper answers "what survives", so it may only ever clear bits.
        for requested in [0o000, 0o111, 0o644, 0o750, 0o777] {
            let masked = umask_masked(requested);
            assert_eq!(masked & !requested, 0, "{requested:04o} gained a bit");
        }
    }

    #[test]
    fn matches_what_the_filesystem_actually_does() {
        // The whole point is agreement with a real create(2); prove it for a
        // mode other than the 0o777 the probe itself uses.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("observed");
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o746)
            .open(&path)
            .expect("create");
        let observed = fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(umask_masked(0o746), observed);
    }
}
