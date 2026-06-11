//! Regression coverage for `compute_allow_inc_recurse`.
//!
//! Pins the receiver-side restriction that gates INC_RECURSE so the upstream
//! testsuite `hardlinks` test no longer deadlocks against a source tree that
//! exceeds upstream's `MIN_FILECNT_LOOKAHEAD` window.
//!
//! upstream: compat.c:161-179 set_allow_inc_recurse,
//! sender.c:228-232 send_extra_file_list throttle,
//! io.c:1740-1760 receiver inline sub-list dispatch.

use crate::{ServerRole, compute_allow_inc_recurse};

#[test]
fn generator_with_recursion_advertises_inc_recurse() {
    assert!(compute_allow_inc_recurse(
        true,
        false,
        ServerRole::Generator
    ));
}

#[test]
fn generator_without_recursion_does_not_advertise() {
    assert!(!compute_allow_inc_recurse(
        false,
        false,
        ServerRole::Generator
    ));
}

#[test]
fn generator_with_qsort_does_not_advertise() {
    assert!(!compute_allow_inc_recurse(
        true,
        true,
        ServerRole::Generator
    ));
}

/// Receiver MUST never advertise INC_RECURSE - drives the upstream
/// testsuite `hardlinks` test deadlock fix. Without this restriction,
/// upstream's sender throttles extra sub-lists at MIN_FILECNT_LOOKAHEAD
/// (1000 entries) while oc-rsync's `receive_extra_file_lists` keeps
/// reading sub-lists upfront, deadlocking on any tree > 1000 entries.
#[test]
fn receiver_never_advertises_inc_recurse() {
    assert!(!compute_allow_inc_recurse(
        true,
        false,
        ServerRole::Receiver
    ));
    assert!(!compute_allow_inc_recurse(
        true,
        true,
        ServerRole::Receiver
    ));
    assert!(!compute_allow_inc_recurse(
        false,
        false,
        ServerRole::Receiver
    ));
}
