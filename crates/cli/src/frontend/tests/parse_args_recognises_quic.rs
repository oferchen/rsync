use super::common::*;
use super::*;

#[cfg(feature = "quic")]
#[test]
fn parse_args_recognises_quic_flag() {
    // WHY (QUIC-8c): `--quic` is a recognised modifier under the feature and
    // sets the flag that upgrades a daemon target to the QUIC transport.
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--quic"),
        OsString::from("host::module"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.quic);
}

#[cfg(feature = "quic")]
#[test]
fn parse_args_quic_defaults_off() {
    // WHY: without `--quic` the daemon transport stays TCP (default behaviour
    // preserved).
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("host::module"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.quic);
}

#[cfg(not(feature = "quic"))]
#[test]
fn parse_args_does_not_recognise_quic_flag_when_feature_off() {
    // WHY (QUIC-8d): with the feature compiled out the `--quic` modifier is
    // absent. The parser therefore never consumes it as a flag - it falls
    // through to the trailing operand list (where it later fails as a bogus
    // path), so no code path can select an unbuilt transport.
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--quic"),
        OsString::from("host::module"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(
        parsed.remainder.contains(&OsString::from("--quic")),
        "--quic must not be consumed as a flag when quic is off"
    );
}
