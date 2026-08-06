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
fn parse_args_rejects_quic_flags_with_actionable_error_when_feature_off() {
    // WHY (176a): with the feature compiled out the `--quic`/`--quic-ca`
    // modifiers are still RECOGNISED (hidden from help) so the parser rejects
    // them with an actionable "requires the 'quic' feature" diagnostic and exit
    // 1, rather than silently passing `--quic` through as a bogus operand. No
    // code path can select an unbuilt transport, and the user learns the remedy.
    for args in [
        vec!["--quic", "host::module", "dest"],
        vec!["--quic-ca", "/etc/oc-rsync/ca.pem", "host::module", "dest"],
    ] {
        let mut argv = vec![OsString::from(RSYNC)];
        argv.extend(args.iter().map(OsString::from));
        let err =
            parse_args(argv).expect_err("quic flags must be rejected when the feature is off");

        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
        let msg = err.to_string();
        assert!(
            msg.contains("--quic requires the QUIC transport"),
            "unexpected message: {msg}"
        );
        assert!(msg.contains("'quic' feature"), "unexpected message: {msg}");
    }
}
