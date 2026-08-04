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
fn parse_args_recognises_quic_ca_flag() {
    // WHY (#50): `--quic-ca <PATH>` is a recognised value flag under the feature
    // and threads the private CA bundle path through to the QUIC trust ladder.
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--quic-ca"),
        OsString::from("/etc/oc-rsync/ca.pem"),
        OsString::from("quic://host/module"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(
        parsed.quic_ca.as_deref(),
        Some(std::path::Path::new("/etc/oc-rsync/ca.pem"))
    );
}

#[cfg(feature = "quic")]
#[test]
fn parse_args_quic_ca_defaults_none() {
    // WHY: without `--quic-ca` the QUIC trust source stays the system-roots
    // default (no private CA bundle).
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("host::module"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.quic_ca.is_none());
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
