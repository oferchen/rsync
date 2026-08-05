//! Guards that the local-copy executor emits no non-upstream per-file debug
//! lines into the `-v`/`-vv`/`-vvv` verbose stream.
//!
//! Upstream rsync has no analog in its verbose stream for oc's former per-file
//! `"sending <f>: N bytes"`, `"transferred <f>: N literal bytes in Ss"`,
//! `"cloned <f>: N bytes (CoW)"`/`(FICLONE)`, `"wincopy <f>: N bytes"`, or
//! `"skipping <f>: already up-to-date"` diagnostics. They were emitted through
//! the `DEBUG_GTE(SEND)` / `DEBUG_GTE(DELTASUM)` categories and, being buffered,
//! flushed after the summary trailer - so they both leaked non-upstream text and
//! landed in the wrong position. They must never appear again.
//!
//! This drives a real fresh copy and an up-to-date rerun at maximal SEND /
//! DELTASUM / FLIST debug verbosity and asserts none of those shapes surface.

use std::fs;

use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use logging::{DiagnosticEvent, VerbosityConfig, drain_events, init};
use tempfile::tempdir;

/// Every diagnostic message emitted since the last drain.
fn drained_messages() -> Vec<String> {
    drain_events()
        .into_iter()
        .map(|event| match event {
            DiagnosticEvent::Debug { message, .. } | DiagnosticEvent::Info { message, .. } => {
                message
            }
        })
        .collect()
}

/// Recognises the exact shapes of the removed non-upstream debug lines. A match
/// means one leaked back into the verbose stream.
fn is_removed_line(message: &str) -> bool {
    message.contains("literal bytes in")                    // "transferred <f>: N literal bytes in Ss"
        || message.contains("bytes (CoW)")                  // clonefile.rs
        || message.contains("bytes (FICLONE)")              // ficlone.rs
        || message.starts_with("wincopy ")                  // wincopy.rs
        || (message.starts_with("skipping ") && message.contains(": ")) // record_metadata_only_skip
        || (message.starts_with("sending ")
            && message.contains(": ")
            && message.contains(" bytes")) // "sending <f>: N bytes"
}

fn assert_no_leak(context: &str) {
    let leaked: Vec<String> = drained_messages()
        .into_iter()
        .filter(|message| is_removed_line(message))
        .collect();
    assert!(
        leaked.is_empty(),
        "non-upstream per-file debug line leaked during {context}: {leaked:?}"
    );
}

/// Raise every debug category the removed lines used well above their emit
/// thresholds (SEND >= 1, DELTASUM >= 2), so a regression that reinstates any
/// line is guaranteed to be drained here.
fn max_debug_config() -> VerbosityConfig {
    let mut config = VerbosityConfig::default();
    config.debug.send = 3;
    config.debug.deltasum = 3;
    config.debug.flist = 3;
    config
}

fn copy(operands: &[std::ffi::OsString], what: &str) {
    let plan = LocalCopyPlan::from_operands(operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .unwrap_or_else(|error| panic!("{what} copy failed: {error}"));
}

#[test]
fn local_copy_emits_no_nonupstream_perfile_debug_lines() {
    init(max_debug_config());
    let _ = drain_events();

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&source).expect("create source");
    fs::write(source.join("a.txt"), b"hello").expect("write a.txt");
    fs::write(source.join("b.txt"), b"world!!").expect("write b.txt");

    let operands = vec![
        source.clone().into_os_string(),
        dest.clone().into_os_string(),
    ];

    // Fresh copy: exercises the whole-file transfer path (mod.rs) and, where the
    // filesystem supports it, the reflink/CoW fast paths (ficlone/clonefile).
    copy(&operands, "fresh");
    assert!(
        dest.join("source/a.txt").exists(),
        "fresh copy must write a.txt"
    );
    assert_no_leak("fresh copy");

    // Align each destination mtime to its source so the rerun's quick-check
    // (size + mtime) treats every file as up-to-date and skips it through
    // record_metadata_only_skip - the path that carried the former
    // "skipping <f>: already up-to-date" debug line.
    for name in ["a.txt", "b.txt"] {
        let src_meta = fs::metadata(source.join(name)).expect("source metadata");
        let mtime = filetime::FileTime::from_last_modification_time(&src_meta);
        filetime::set_file_mtime(dest.join("source").join(name), mtime).expect("align mtime");
    }

    // Up-to-date rerun: the quick-check now skips every file.
    copy(&operands, "rerun");
    assert_no_leak("up-to-date rerun");
}
