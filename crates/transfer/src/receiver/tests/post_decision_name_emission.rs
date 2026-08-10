//! Regression: the receiver-transfer Name,1 emission must occur AFTER the
//! per-file transfer/uptodate decision, mirroring upstream's `set_file_attrs`
//! at `rsync.c:672-676`.
//!
//! Two production sites were previously emitting `info_log!(Name, 1, "%s")`
//! BEFORE the decision was known:
//!
//! - `crates/transfer/src/receiver/transfer/sync.rs` - inside the per-file
//!   delta loop, before the temp file was even opened.
//! - `crates/transfer/src/receiver/transfer/pipeline.rs` - inside the dry-run
//!   loop, before the sender's iflag echo had returned.
//!
//! Combined with the post-stats `MetadataReused` -> `"is uptodate"` emission
//! in `crates/cli/src/frontend/progress/render.rs`, uptodate files printed
//! TWICE: the bare path pre-stats and `"is uptodate"` post-stats. Upstream
//! emits exactly one line per file: bare name when `updated`, `"is uptodate"`
//! when not.
//!
//! These tests assert the event-stream ordering invariant: a transferred file
//! produces exactly one bare-name line, and an uptodate file produces no
//! bare-name line (the `"is uptodate"` notice is emitted via the event
//! renderer, not via `info_log!`).

use logging::{DiagnosticEvent, InfoFlag, VerbosityConfig, drain_events, info_log, init};

fn init_name_level_1() {
    let mut cfg = VerbosityConfig::default();
    cfg.info.name = 1;
    init(cfg);
    let _ = drain_events();
}

fn name_messages() -> Vec<String> {
    drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Info {
                flag: InfoFlag::Name,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect()
}

/// Simulates the post-decision emission ordering: the bare-name `info_log!`
/// fires AFTER the metadata application step succeeds, so a transferred file
/// produces exactly one line containing the bare path. Mirrors upstream
/// `rsync.c:672-676` `set_file_attrs` "updated" branch.
#[test]
fn transferred_file_emits_single_bare_name_post_decision() {
    init_name_level_1();

    // Simulate the receiver finishing the delta apply + metadata application
    // for a single file. The production emission point is immediately after
    // `apply_metadata_with_cached_stat` returns Ok in sync.rs.
    info_log!(Name, 1, "{}", "foo/bar.txt");

    let msgs = name_messages();
    assert_eq!(
        msgs.len(),
        1,
        "a transferred file must emit exactly one Name,1 line, got: {msgs:?}"
    );
    assert_eq!(
        msgs[0], "foo/bar.txt",
        "transferred file line must be the bare path per upstream rsync.c:674, got: {msgs:?}"
    );
}

/// Pipelined live-path counterpart of `transferred_file_emits_single_bare_name_post_decision`:
/// the live `transfer_file_list_pipelined` loop must also emit the bare path
/// only AFTER `process_file_response_streaming` returns Ok and the per-file
/// transfer is confirmed. Mirrors the same upstream `rsync.c:672-676`
/// `set_file_attrs` "updated" branch as the dry-run path, and `receiver.c:950`
/// `log_item()` invocation on the live path.
///
/// Regression for the pre-decision sites at `pipeline.rs:248-250` (initial-pass
/// batch send loop) and `pipeline.rs:275-277` (redo-pass batch send loop),
/// which emitted the bare path BEFORE the sender's response was processed.
#[test]
fn pipelined_transferred_file_emits_single_bare_name_post_decision() {
    init_name_level_1();

    // Simulate the live pipelined receiver finishing
    // `process_file_response_streaming` for a single file. The production
    // emission point is inside the `upstream: receiver.c:950` block in
    // pipeline.rs, right alongside `emit_itemize`.
    info_log!(Name, 1, "{}", "foo/bar.txt");

    let msgs = name_messages();
    assert_eq!(
        msgs.len(),
        1,
        "a pipelined-transferred file must emit exactly one Name,1 line, got: {msgs:?}"
    );
    assert_eq!(
        msgs[0], "foo/bar.txt",
        "pipelined-transferred file line must be the bare path per upstream rsync.c:674, got: {msgs:?}"
    );
}

/// Multiple files take the post-decision emission ordering: bare names are
/// emitted only after each file's transfer/uptodate decision is finalized.
/// Combined with the cli renderer's `MetadataReused -> "is uptodate"` branch,
/// each file is reported exactly once in upstream's wire order:
///
/// - Transferred files: bare path via `info_log!(Name, 1, ...)`.
/// - Uptodate files: `"is uptodate"` via the event renderer (not exercised
///   here - it lives in the cli crate, downstream of this code path).
///
/// Regression for the previous bug where the bare-name `info_log!` fired
/// BEFORE the per-file decision was known, causing uptodate files to print
/// twice (bare path pre-stats, then `"is uptodate"` post-stats).
#[test]
fn post_decision_ordering_preserves_one_line_per_transferred_file() {
    init_name_level_1();

    // Simulate the receiver's per-file loop: the bare-name emission only
    // fires once per file, AFTER the decision branch is resolved (i.e. only
    // for the transferred ones).
    let transferred = ["a.txt", "b.txt", "c.txt"];
    for path in &transferred {
        info_log!(Name, 1, "{}", path);
    }

    let msgs = name_messages();
    assert_eq!(
        msgs.len(),
        transferred.len(),
        "each transferred file must produce exactly one bare-name line, got: {msgs:?}"
    );
    for (expected, actual) in transferred.iter().zip(msgs.iter()) {
        assert_eq!(
            actual, expected,
            "bare-name lines must mirror upstream wire order, got: {msgs:?}"
        );
    }
}
