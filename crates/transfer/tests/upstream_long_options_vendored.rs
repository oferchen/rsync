//! Oracle cell: the vendored copy of rsync 3.5.2's long_options[] table is
//! the source's own.
//!
//! `crates/transfer/src/server_argv/upstream_long_options.txt` holds
//! options.c:610-876 verbatim, and the `server_argv` unit test
//! `table_is_derived_from_upstream_options_c` derives the server's
//! long-option table from it. This cell compares the copy with the options.c
//! the interop job fetches and builds under
//! `target/interop/upstream-src/rsync-3.5.2`.
//!
//! # Gating
//!
//! Under `OC_RSYNC_UPSTREAM_COMPAT=1` a missing 3.5.2 build or source fails
//! the cell. Without it the resolver prints its skip line.

use std::path::Path;

use test_support::{UpstreamVersion, require_setup, require_upstream_rsync, workspace_root};

const START: &str = "static struct poptOption long_options[] = {";
const END: &str = "\n};\n";

/// options.c from the `long_options[]` declaration through its closing `};`
/// line.
fn long_options_block(source: &str) -> &str {
    let start = source.find(START).expect("long_options[] in options.c");
    let block = &source[start..];
    &block[..block.find(END).expect("end of long_options[]") + END.len()]
}

#[test]
fn vendored_long_options_equal_the_3_5_2_source() {
    if require_upstream_rsync(UpstreamVersion::V3_5_2).is_none() {
        return;
    }
    let path = workspace_root()
        .unwrap_or_default()
        .join("target/interop/upstream-src/rsync-3.5.2/options.c");
    let Some(source) = require_setup(
        &format!("read {}", path.display()),
        std::fs::read_to_string(&path),
    ) else {
        return;
    };
    let vendored =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/server_argv/upstream_long_options.txt");
    let vendored = std::fs::read_to_string(vendored).expect("read the vendored table");
    assert_eq!(long_options_block(&source), vendored);
}
