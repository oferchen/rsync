//! UTS-V3-D regression for the upstream `files-from.test` 4th invocation:
//! upstream sender → oc-rsync server-receiver with
//! `--files-from=host:path`. Upstream's `main.c:1173-1180` opens the
//! local filesfrom file on the receiver side and forwards its bytes back
//! to the sender via `start_filesfrom_forwarding(filesfrom_fd)`. Before
//! this fix oc-rsync's receiver dropped the path on the floor, so the
//! upstream sender blocked forever at `building file list ...` waiting
//! for filenames that never arrived (300s testsuite timeout, then
//! SIGTERM at `rsync.c:716`).
//!
//! The transport-layer half of the fix is
//! `protocol::forward_files_from`, which is the receiver-side equivalent
//! of upstream `io.c:370 forward_filesfrom_data()`. This test pins the
//! load-bearing byte contract that the upstream sender's `filesfrom_fd`
//! reader expects.
//!
//! The companion CLI-parser surface
//! (`cli::frontend::execution::file_list::parser::resolve_files_from_source`)
//! has its own unit tests inside that crate; both must hold for the
//! 4-invocation testsuite to complete.

use std::io::Cursor;

use protocol::{forward_files_from, read_files_from_stream};

/// Files-from list used by the upstream `files-from.test`. Mirrors the
/// `scratch/filelist` produced by `testsuite/files-from_test.py`.
const UPSTREAM_FILES_FROM_PAYLOAD: &[u8] = b"from/./\n\
from/./dir/subdir\n\
from/./dir/subdir/subsubdir\n\
from/./dir/subdir/subsubdir2/\n\
from/./dir/subdir/foobar.baz\n";

/// Wire bytes upstream rsync emits when it forwards the same payload via
/// `start_filesfrom_forwarding`. Each newline-delimited entry becomes a
/// NUL-terminated wire entry, terminated by a single trailing NUL since
/// the last entry already had a NUL appended.
const UPSTREAM_FILES_FROM_WIRE: &[u8] = b"from/./\0\
from/./dir/subdir\0\
from/./dir/subdir/subsubdir\0\
from/./dir/subdir/subsubdir2/\0\
from/./dir/subdir/foobar.baz\0\
\0";

#[test]
fn forwarded_wire_bytes_match_upstream_sender_expectation() {
    // Bind the receiver-side forwarding contract to a byte-exact wire
    // pattern. The upstream sender's `flist.c:2262 read_line` parser
    // consumes NUL-terminated entries until it hits a NUL with an empty
    // entry buffer (the double-NUL terminator). Any deviation here -
    // missing terminator, dropped CR, premature flush - reproduces the
    // 4th-invocation hang.
    let mut reader = Cursor::new(UPSTREAM_FILES_FROM_PAYLOAD);
    let mut wire = Vec::new();

    forward_files_from(&mut reader, &mut wire, false, None).expect("forward must succeed");

    assert_eq!(
        wire, UPSTREAM_FILES_FROM_WIRE,
        "forwarded wire bytes must match upstream's `forward_filesfrom_data`"
    );
}

#[test]
fn forwarded_round_trip_repeats_cleanly() {
    // The `files-from.test` 4th invocation is preceded by three earlier
    // transfers that share process state in the testsuite runner; the
    // forwarding helper must not accumulate residual state across runs.
    // Loop 4x (matching the upstream testsuite) and assert each emission
    // matches.
    for iteration in 1..=4 {
        let mut reader = Cursor::new(UPSTREAM_FILES_FROM_PAYLOAD);
        let mut wire = Vec::new();
        forward_files_from(&mut reader, &mut wire, false, None)
            .unwrap_or_else(|err| panic!("iteration {iteration} forward failed: {err}"));

        assert_eq!(
            wire, UPSTREAM_FILES_FROM_WIRE,
            "iteration {iteration} wire bytes diverged - residual state in forwarder?"
        );

        // Confirm the sender's reader recovers the exact filename list
        // each time. Without the trailing double-NUL the reader would
        // block here for input that the receiver never sends.
        let mut wire_reader = Cursor::new(&wire);
        let names = read_files_from_stream(&mut wire_reader, None)
            .unwrap_or_else(|err| panic!("iteration {iteration} read failed: {err}"));
        assert_eq!(
            names,
            vec![
                "from/./",
                "from/./dir/subdir",
                "from/./dir/subdir/subsubdir",
                "from/./dir/subdir/subsubdir2/",
                "from/./dir/subdir/foobar.baz",
            ],
            "iteration {iteration} parsed filenames diverged"
        );
    }
}

#[test]
fn forwarded_already_nul_delimited_round_trip_repeats() {
    // `--from0` flips to NUL-delimited input. The upstream testsuite's
    // 4th invocation may carry `--from0` when an earlier flag in the
    // chain set it, so the forwarder must preserve NUL-delimited semantics
    // across repeated invocations too.
    let nul_payload: &[u8] = b"from/./\0\
from/./dir/subdir\0\
from/./dir/subdir/subsubdir\0\
from/./dir/subdir/subsubdir2/\0\
from/./dir/subdir/foobar.baz\0";

    for iteration in 1..=4 {
        let mut reader = Cursor::new(nul_payload);
        let mut wire = Vec::new();
        forward_files_from(&mut reader, &mut wire, true, None)
            .unwrap_or_else(|err| panic!("iteration {iteration} --from0 forward failed: {err}"));

        assert_eq!(
            wire, UPSTREAM_FILES_FROM_WIRE,
            "iteration {iteration} --from0 wire bytes diverged"
        );
    }
}
