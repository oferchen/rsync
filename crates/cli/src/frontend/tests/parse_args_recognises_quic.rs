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
fn parse_args_recognises_quic_cc_flag() {
    // WHY: `--quic-cc <bbr|cubic|newreno>` selects the client endpoint's
    // congestion controller and threads a resolved `CongestionAlgorithm` down
    // to `build_transport_config`.
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--quic-cc"),
        OsString::from("cubic"),
        OsString::from("quic://host/module"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(
        parsed.quic_cc,
        Some(rsync_io::quic::CongestionAlgorithm::Cubic)
    );
}

#[cfg(feature = "quic")]
#[test]
fn parse_args_rejects_unknown_quic_cc_value() {
    // WHY: an unrecognised controller name must fail loudly at parse time (the
    // clap value restriction), never silently fall back to a default.
    let err = parse_args([
        OsString::from(RSYNC),
        OsString::from("--quic-cc"),
        OsString::from("reno"),
        OsString::from("quic://host/module"),
        OsString::from("dest"),
    ])
    .expect_err("unknown controller must be rejected");
    assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
}

#[cfg(feature = "quic")]
#[test]
fn parse_args_recognises_quic_window_flag() {
    // WHY: `--quic-window <SIZE>` sizes the client endpoint's flow-control
    // window; the K/M/G suffix is parsed by the shared workspace size parser.
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--quic-window"),
        OsString::from("32M"),
        OsString::from("quic://host/module"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.quic_window, Some(32 * 1024 * 1024));
}

#[cfg(feature = "quic")]
#[test]
fn parse_args_rejects_invalid_quic_window_value() {
    // WHY: a garbage window size must fail loudly, not be silently ignored.
    let err = parse_args([
        OsString::from(RSYNC),
        OsString::from("--quic-window"),
        OsString::from("lots"),
        OsString::from("quic://host/module"),
        OsString::from("dest"),
    ])
    .expect_err("invalid window must be rejected");
    assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
}

#[cfg(feature = "quic")]
#[test]
fn parse_args_quic_cc_and_window_default_none() {
    // WHY: without the flags the client endpoint defers to the env vars and
    // then the built-in defaults (BBR / BDP-generous window).
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("host::module"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.quic_cc.is_none());
    assert!(parsed.quic_window.is_none());
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
        vec!["--quic-cc", "bbr", "host::module", "dest"],
        vec!["--quic-window", "32M", "host::module", "dest"],
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
