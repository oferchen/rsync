/// Wire-byte parity check: the cached greeting bytes used on the hot accept
/// path must be byte-identical to the per-call greeting that tests and
/// non-hot paths still allocate. Guards against regressions in the DIS-4.a
/// R2 hoist that powers the cold-start latency fix.
#[test]
fn cached_legacy_daemon_greeting_matches_per_call_bytes() {
    let cached = cached_legacy_daemon_greeting();
    let allocated = legacy_daemon_greeting();
    assert_eq!(cached, allocated.as_bytes());
}
