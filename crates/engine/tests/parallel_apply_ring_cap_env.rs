//! Integration coverage for the `OC_RSYNC_REORDER_RING_CAP` operator
//! override (ROB-11, #3678).
//!
//! The applier reads the env var once per process via a [`std::sync::OnceLock`],
//! so this test binary deliberately exercises a single override path: set the
//! variable before the first applier is constructed, then assert every
//! subsequently-built applier reports the overridden capacity through
//! [`ParallelDeltaApplier::per_file_reorder_capacity`]. A sibling unit-test
//! module inside `engine::concurrent_delta::parallel_apply::ring_cap_env`
//! covers the parser fallback paths (empty / zero / negative / non-numeric)
//! at the function level so each parser branch lives in its own isolated
//! [`std::env::var`] read.
//!
//! This binary is also the canonical regression for the contract that the
//! explicit builder [`ParallelDeltaApplier::with_per_file_reorder_capacity`]
//! wins over the env override - the closing assertion swaps in a non-default
//! capacity and confirms the per-instance value sticks.

use engine::concurrent_delta::ParallelDeltaApplier;

const RING_CAP_ENV: &str = "OC_RSYNC_REORDER_RING_CAP";
const OVERRIDE_VALUE: usize = 256;

#[test]
fn env_override_resolves_into_per_file_reorder_capacity() {
    // SAFETY: `set_var` is process-wide; no other test in this binary
    // touches the env or the applier before this test runs.
    unsafe {
        std::env::set_var(RING_CAP_ENV, OVERRIDE_VALUE.to_string());
    }

    // First construction in this process snapshots the env var into the
    // applier's `OnceLock`. The reported capacity must match the override.
    let applier = ParallelDeltaApplier::new(4);
    assert_eq!(
        applier.per_file_reorder_capacity(),
        OVERRIDE_VALUE,
        "OC_RSYNC_REORDER_RING_CAP did not override the hard 64 default"
    );

    // A second construction must see the same cached value - the OnceLock
    // contract guarantees the env var is only read once per process. Even
    // if a malicious test had cleared the var in the meantime, the cache
    // would return the original snapshot.
    let second = ParallelDeltaApplier::new(8);
    assert_eq!(
        second.per_file_reorder_capacity(),
        OVERRIDE_VALUE,
        "cached env override did not propagate to subsequently constructed appliers"
    );

    // Builder override beats the env var: with_per_file_reorder_capacity
    // takes precedence so per-instance tuning still works under an active
    // operator override. Documented in the rustdoc on with_strategy.
    let manual_override = ParallelDeltaApplier::new(4).with_per_file_reorder_capacity(17);
    assert_eq!(
        manual_override.per_file_reorder_capacity(),
        17,
        "explicit builder override did not win over the env-var resolver"
    );
}
