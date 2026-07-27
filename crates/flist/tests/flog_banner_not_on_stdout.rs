//! Traversal must not put upstream's log-file-only file-list banner on stdout.
//!
//! upstream: flist.c:2248 announces the walk with
//! `rprintf(FLOG, "building file list\n")`. log.c:rwrite() handles FLOG by
//! writing to the log and returning (`if (code == FLOG || ...) return;`), and
//! by returning outright (`else if (code == FLOG) return;`) when the client has
//! neither `--log-file` nor daemon mode - so the line never reaches stdout.
//! Verified against rsync 3.4.4: `-av` and `-avv` produce byte-identical stdout
//! with and without `--log-file`, and only the log file gains
//! `building file list`. Upstream has no per-entry "built file list with N
//! entries" message at any verbosity or logcode.
//!
//! Every `DiagnosticEvent` emitted here is rendered onto the client's stdout by
//! `cli::frontend::progress::diagnostic::render_diagnostic_events`, which has no
//! FLOG dimension to discard. A file-list banner emitted from this crate would
//! therefore appear on stdout at plain `-v`/`-vv`, where upstream shows nothing.

use std::fs;

use flist::FileListBuilder;
use logging::{DiagnosticEvent, VerbosityConfig, drain_events, init};

/// Substrings of the upstream banners that must never reach a diagnostic event.
const LOG_FILE_ONLY_BANNERS: [&str; 2] = ["building file list", "built file list with"];

fn message(event: &DiagnosticEvent) -> &str {
    match event {
        DiagnosticEvent::Info { message, .. } | DiagnosticEvent::Debug { message, .. } => message,
    }
}

fn leaked_banners(events: Vec<DiagnosticEvent>) -> Vec<String> {
    events
        .into_iter()
        .filter(|event| {
            let text = message(event);
            LOG_FILE_ONLY_BANNERS
                .iter()
                .any(|banner| text.contains(banner))
        })
        .map(|event| message(&event).to_owned())
        .collect()
}

fn tree() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("temp dir");
    let nested = temp.path().join("nested");
    fs::create_dir_all(&nested).expect("create nested dir");
    fs::write(temp.path().join("file.txt"), b"data").expect("write file");
    fs::write(nested.join("more.txt"), b"data").expect("write nested file");
    temp
}

/// `-vv` raises `DebugFlag::Flist` to 1 (upstream's `debug_verbosity[2]`
/// includes `FLIST`), which is the level at which a walker-side announcement
/// would fire. Upstream prints nothing to stdout there.
#[test]
fn walker_emits_no_file_list_banner_at_verbose_two() {
    init(VerbosityConfig::from_verbose_level(2));
    let _ = drain_events();

    let temp = tree();
    let walker = FileListBuilder::new(temp.path()).build().expect("walker");
    let count = walker.count();
    assert!(
        count > 0,
        "walker must yield entries for the assertion to mean anything"
    );

    let leaked = leaked_banners(drain_events());
    assert!(
        leaked.is_empty(),
        "walker leaked upstream's log-file-only file-list banner to stdout: {leaked:?}"
    );
}

/// A completed enumeration is reported upstream by the CLI-layer
/// `sending incremental file list` banner, never by an entry count from the
/// traversal itself.
#[cfg(feature = "parallel")]
#[test]
fn collect_entries_emits_no_entry_count_at_verbose_two() {
    use flist::parallel::collect_entries;

    init(VerbosityConfig::from_verbose_level(2));
    let _ = drain_events();

    let temp = tree();
    let walker = FileListBuilder::new(temp.path()).build().expect("walker");
    let entries = collect_entries(walker).expect("collect entries");
    assert!(
        !entries.is_empty(),
        "collection must yield entries for the assertion to mean anything"
    );

    let leaked = leaked_banners(drain_events());
    assert!(
        leaked.is_empty(),
        "collect_entries leaked a file-list banner to stdout: {leaked:?}"
    );
}
