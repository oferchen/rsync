//! Regression coverage for `compute_allow_inc_recurse`.
//!
//! Pins the receiver-side restriction that gates INC_RECURSE so the upstream
//! testsuite `hardlinks` test no longer deadlocks against a source tree that
//! exceeds upstream's `MIN_FILECNT_LOOKAHEAD` window.
//!
//! upstream: compat.c:161-179 set_allow_inc_recurse,
//! sender.c:228-232 send_extra_file_list throttle.

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

/// Receiver MUST never advertise INC_RECURSE. Measured: dropping the role term
/// deadlocks the upstream testsuite `hardlinks` cell on a source tree of 1024
/// entries while 961 still passes, so the boundary is upstream's
/// MIN_FILECNT_LOOKAHEAD of 1000. See `compute_allow_inc_recurse` for the full
/// A/B table and for why the receiver-side blocking site is deliberately left
/// unnamed.
#[test]
fn receiver_never_advertises_inc_recurse() {
    assert!(!compute_allow_inc_recurse(
        true,
        false,
        ServerRole::Receiver
    ));
    assert!(!compute_allow_inc_recurse(true, true, ServerRole::Receiver));
    assert!(!compute_allow_inc_recurse(
        false,
        false,
        ServerRole::Receiver
    ));
}
