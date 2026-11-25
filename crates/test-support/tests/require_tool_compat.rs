//! A missing tool fails an oracle cell under `OC_RSYNC_UPSTREAM_COMPAT=1` and
//! skips it otherwise.
//!
//! The interop job's oracle step sets the variable and rejects the resolver's
//! skip line in its log, so a cell that lacks `strace`, `setfacl` or
//! `setfattr` must not pass there. The real environment lookup in
//! `test_support::require_tool` decides the outcome.

const MISSING: &str = "oc-rsync-no-such-tool-xyzzy";

/// The cell the prover re-runs: inert in a normal run.
#[test]
fn child_cell_needing_a_missing_tool() {
    if !test_support::in_prerequisite_gate_child() {
        return;
    }
    assert!(
        test_support::require_tool(MISSING).is_none(),
        "{MISSING} must not resolve"
    );
}

#[test]
fn a_missing_tool_fails_under_compat_and_skips_without() {
    test_support::assert_prerequisite_gate(
        "child_cell_needing_a_missing_tool",
        &format!("required tool `{MISSING}` is not on PATH"),
    );
}
