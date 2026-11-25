//! A missing tool fails an oracle cell under `OC_RSYNC_UPSTREAM_COMPAT=1` and
//! skips it otherwise.
//!
//! The interop job's oracle step sets the variable and rejects the resolver's
//! skip line in its log, so a cell that lacks `strace`, `setfacl` or
//! `setfattr` must not pass there. Each case re-runs this test binary with
//! only the child test selected, so the real environment lookup in
//! `test_support::require_tool` decides the outcome.

use std::process::{Command, Output};

const CHILD: &str = "OC_RSYNC_REQUIRE_TOOL_CHILD";
const MISSING: &str = "oc-rsync-no-such-tool-xyzzy";

/// The cell the parent re-runs: inert unless the parent set `CHILD`.
#[test]
fn child_cell_needing_a_missing_tool() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    if test_support::require_tool(MISSING).is_none() {
        return;
    }
    panic!("{MISSING} must not resolve");
}

fn run_child(compat: Option<&str>) -> Output {
    let mut cmd = Command::new(std::env::current_exe().expect("test binary path"));
    cmd.args([
        "child_cell_needing_a_missing_tool",
        "--exact",
        "--nocapture",
    ])
    .env(CHILD, "1")
    .env_remove("OC_RSYNC_UPSTREAM_COMPAT");
    if let Some(value) = compat {
        cmd.env("OC_RSYNC_UPSTREAM_COMPAT", value);
    }
    cmd.output().expect("re-run the test binary")
}

#[test]
fn a_missing_tool_fails_the_cell_under_compat() {
    let out = run_child(Some("1"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "the cell must fail: {out:?}");
    assert!(
        stderr.contains(&format!("required tool `{MISSING}` is not on PATH")),
        "the failure must name the tool: {stderr}"
    );
}

#[test]
fn a_missing_tool_skips_the_cell_without_compat() {
    let out = run_child(None);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the cell must pass as a skip: {out:?}"
    );
    assert!(
        stderr.contains("Skipping upstream-compat test: required tool"),
        "the skip must use the line the oracle step's check-log rejects: {stderr}"
    );
}
