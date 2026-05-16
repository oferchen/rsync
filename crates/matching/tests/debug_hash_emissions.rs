//! Integration test for `--debug=HASH` producer emissions wired into the
//! [`DeltaSignatureIndex`] lifecycle.
//!
//! Drives the `from_signature` builder under `debug.hash = 1` and asserts that
//! the upstream-format `created hashtable ...` line fires through the real
//! [`logging`] channel - the same code path users hit when running
//! `oc-rsync --debug=HASH`.

use std::num::NonZeroU8;

use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};
use matching::DeltaSignatureIndex;
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

/// Initializes logging with HASH debug at level 1 and clears any pending
/// events so assertions can focus on emissions produced by the test body.
fn init_hash_level_1() {
    let mut cfg = VerbosityConfig::default();
    cfg.debug.hash = 1;
    init(cfg);
    let _ = drain_events();
}

/// Collects HASH debug messages emitted since the last drain.
fn hash_messages() -> Vec<String> {
    drain_events()
        .into_iter()
        .filter_map(|event| match event {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Hash,
                message,
                ..
            } => Some(message),
            _ => None,
        })
        .collect()
}

/// Builds a signature with at least one full-length block so the index
/// construction path runs to completion.
fn build_signature() -> signature::FileSignature {
    let data = vec![b'a'; 4096];
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    generate_file_signature(data.as_slice(), layout, SignatureAlgorithm::Md4).expect("signature")
}

/// Exercises `DeltaSignatureIndex::from_signature` under `--debug=HASH` and
/// asserts the upstream-format created emission fires.
///
/// upstream: hashtable.c:45-53.
#[test]
fn from_signature_emits_created_under_debug_hash() {
    init_hash_level_1();

    let signature = build_signature();
    let _index =
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index");

    let msgs = hash_messages();
    assert!(
        msgs.iter().any(|m| m.starts_with("[sender] created hashtable ")
            && m.ends_with(", keys: 32-bit)")),
        "expected upstream-format HASH,1 created emission from from_signature, got {msgs:?}"
    );
}

/// Drops a constructed index and asserts the matching destroyed emission
/// fires through the [`Drop`] impl.
///
/// upstream: hashtable.c:60-63.
#[test]
fn drop_emits_destroyed_under_debug_hash() {
    init_hash_level_1();

    let signature = build_signature();
    let index =
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index");
    drop(index);

    let msgs = hash_messages();
    assert!(
        msgs.iter()
            .any(|m| m.starts_with("[sender] destroyed hashtable ")
                && m.ends_with(", keys: 32-bit)")),
        "expected upstream-format HASH,1 destroyed emission from Drop, got {msgs:?}"
    );
}
