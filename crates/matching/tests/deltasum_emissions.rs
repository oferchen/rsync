//! Guards the delta scanner's `--debug=DELTASUM` emissions against upstream
//! rsync 3.4.4.
//!
//! Upstream's sender prints exactly two things while scanning, and they live at
//! different debug levels:
//!
//! - `DELTASUM >= 2`, once per file, at the end of `match_sums()`:
//!   `false_alarms=%d hash_hits=%d matches=%d` (match.c:428-431).
//! - `DELTASUM >= 1`, once per RUN, from `match_report()` (match.c:439-448),
//!   called from a single place - `sender.c:491`, after `send_files()` has
//!   finished every file:
//!   `total: matches=%d  hash_hits=%d  false_alarms=%d data=%s`.
//!
//! The scanner therefore emits NOTHING at DELTASUM level 1: the run-total line
//! is not a per-file diagnostic and is not the scanner's to print. oc renders
//! it once from the client summary (`cli::frontend::progress::render`), which is
//! also what puts it at upstream's position - after the name list and before the
//! `sent ... received ...` trailer - rather than dead-last through the deferred
//! diagnostic flush.
//!
//! oc previously emitted a second, invented run-report from here:
//!
//! ```text
//! delta: 2 tokens, 1400 total, 700 literal, 700 matched
//! ```
//!
//! commented `// upstream: match.c match_report() equivalent`. MEASURED on a
//! `-rvv --no-whole-file` daemon push, upstream printed
//! `total: matches=1  hash_hits=1  false_alarms=0 data=700` between the name
//! list and the trailer, while oc printed the `delta:` line after the trailer.
//! Wrong text, wrong position, wrong cardinality (per file, not per run), and a
//! second emitter competing with the renderer's.
//!
//! These tests pin both halves so neither can drift back: level 1 stays silent,
//! and the level-2 per-file line keeps upstream's exact wording and field order.

use std::num::NonZeroU8;

use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};
use matching::{DeltaGenerator, DeltaSignatureIndex};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

/// Basis and source that produce one real block match plus one literal run, so
/// the scan has non-zero counters to report if it were inclined to.
const BLOCK: usize = 700;

fn basis_bytes() -> Vec<u8> {
    let mut data = vec![b'A'; BLOCK];
    data.extend(std::iter::repeat_n(b'B', BLOCK));
    data
}

fn source_bytes() -> Vec<u8> {
    let mut data = vec![b'A'; BLOCK];
    data.extend(std::iter::repeat_n(b'C', BLOCK));
    data
}

fn build_index(basis: &[u8]) -> DeltaSignatureIndex {
    let params = SignatureLayoutParams::new(
        basis.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature =
        generate_file_signature(basis, layout, SignatureAlgorithm::Md4).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
}

/// Initializes the given DELTASUM debug level and clears pending events so the
/// assertions only see emissions from the scan itself.
fn init_deltasum(level: u8) {
    let mut cfg = VerbosityConfig::default();
    cfg.debug.deltasum = level;
    init(cfg);
    let _ = drain_events();
}

fn deltasum_messages() -> Vec<String> {
    drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Deltasum,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect()
}

fn scan() {
    let basis = basis_bytes();
    let index = build_index(&basis);
    let source = source_bytes();
    let script = DeltaGenerator::new()
        .generate(std::io::Cursor::new(source.as_slice()), &index)
        .expect("delta scan");
    assert!(
        script.literal_bytes() > 0 && script.total_bytes() > script.literal_bytes(),
        "fixture must produce both a match and a literal run so the scan has \
         something to report"
    );
}

/// At `-vv` (DELTASUM level 1) the scanner must stay silent.
///
/// upstream: the only level-1 delta output is `match_report()`'s `total:` line,
/// which is a once-per-run summary printed from `sender.c:491` - not from the
/// per-file scan. A per-file emission here is both an invented line and a
/// second `total:` emitter competing with the one in the client renderer.
#[test]
fn level_1_scan_emits_nothing() {
    init_deltasum(1);
    scan();

    let msgs = deltasum_messages();
    assert!(
        msgs.is_empty(),
        "the delta scan must emit no DELTASUM level-1 line; upstream prints only \
         match_report()'s once-per-run `total:` line at this level, and oc \
         renders that from the client summary. Got: {msgs:?}"
    );
}

/// At `-vvv` (DELTASUM level 2) the per-file counter line must survive, with
/// upstream's exact wording and field order.
///
/// upstream: match.c:428-431 - `false_alarms=%d hash_hits=%d matches=%d`, note
/// the single spaces and the order, which is deliberately the REVERSE of the
/// `total:` line's `matches / hash_hits / false_alarms`.
#[test]
fn level_2_scan_emits_upstream_per_file_counter_line() {
    init_deltasum(2);
    scan();

    let msgs = deltasum_messages();
    let counters = msgs
        .iter()
        .find(|m| m.starts_with("false_alarms="))
        .unwrap_or_else(|| panic!("missing per-file counter line, got: {msgs:?}"));
    let fields: Vec<&str> = counters.split(' ').collect();
    assert_eq!(fields.len(), 3, "unexpected field count in {counters:?}");
    assert!(fields[0].starts_with("false_alarms="), "{counters:?}");
    assert!(fields[1].starts_with("hash_hits="), "{counters:?}");
    assert!(fields[2].starts_with("matches="), "{counters:?}");
    assert!(
        !msgs.iter().any(|m| m.starts_with("total:")),
        "the run-total line is never emitted per file, got: {msgs:?}"
    );
}
