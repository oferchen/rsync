//! `try_new_with_status` tests: the three reporting outcomes
//! (`Disabled`, `Enabled`, `RegistrationFailed`) used by the adaptive
//! sizer to distinguish opt-out from kernel rejection.

use super::super::MAX_REGISTERED_BUFFERS;
use super::super::registry::RegisteredBufferGroup;
use super::super::stats::RegisteredBufferStatus;
use super::try_ring;

/// `try_new_with_status` with `enabled=false` returns `Disabled` without
/// calling the kernel - distinct from a `RegistrationFailed` outcome.
#[test]
fn try_new_with_status_disabled_when_flag_off() {
    let Some(ring) = try_ring(4) else { return };
    let (group, status) = RegisteredBufferGroup::try_new_with_status(&ring, 4096, 4, false);
    assert!(group.is_none());
    assert_eq!(status, RegisteredBufferStatus::Disabled);
    assert!(status.is_disabled() && !status.is_enabled() && !status.is_registration_failed());
}

/// Successful registration yields `Enabled` and a live group; constrained
/// environments that reject registration still produce a non-`Disabled`
/// status, exercising the failure branch.
#[test]
fn try_new_with_status_enabled_on_success() {
    let Some(ring) = try_ring(4) else { return };
    let (group, status) = RegisteredBufferGroup::try_new_with_status(&ring, 4096, 4, true);
    assert_ne!(status, RegisteredBufferStatus::Disabled);
    if let RegisteredBufferStatus::Enabled = status {
        assert_eq!(group.expect("Enabled implies a group").count(), 4);
    }
}

/// When registration fails the status carries the formatted `errno` for
/// telemetry and the group is `None`. Forcing failure via the wrapper's
/// own `MAX_REGISTERED_BUFFERS` ceiling keeps the test portable.
#[test]
fn try_new_with_status_registration_failed_carries_reason() {
    let Some(ring) = try_ring(4) else { return };
    let (group, status) =
        RegisteredBufferGroup::try_new_with_status(&ring, 4096, MAX_REGISTERED_BUFFERS + 1, true);
    assert!(group.is_none());
    match status {
        RegisteredBufferStatus::RegistrationFailed { reason } => {
            assert!(!reason.is_empty(), "failure reason must be populated");
        }
        other => panic!("expected RegistrationFailed, got {other:?}"),
    }
}
