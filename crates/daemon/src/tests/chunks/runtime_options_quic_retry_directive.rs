// QUIC Retry policy (oc extension). The `quic retry` global directive selects
// when the QUIC listener answers a new client with a stateless Retry: `auto`
// (the default) while it is under load, `always`, or `never`. A value outside
// that set is a config error that names the directive, so an operator never
// runs with a policy other than the one they wrote. These configs set no
// certificate, which only listener startup rejects, so they parse on Unix.

use rsync_io::quic::RetryPolicy;

fn parse_retry_config(body: &str) -> Result<RuntimeOptions, DaemonError> {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(&config_path, body).expect("write config");
    RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
}

#[test]
fn quic_retry_unset_defaults_to_retry_under_load() {
    let options = parse_retry_config("port = 873\n").expect("parse config");
    assert_eq!(options.quic_retry_policy(), RetryPolicy::default());
    assert!(matches!(
        options.quic_retry_policy(),
        RetryPolicy::UnderLoad { .. }
    ));
}

#[test]
fn quic_retry_directive_selects_each_policy() {
    for (value, expected) in [
        ("auto", RetryPolicy::default()),
        ("always", RetryPolicy::Always),
        ("never", RetryPolicy::Never),
        (" ALWAYS ", RetryPolicy::Always),
        ("Never", RetryPolicy::Never),
    ] {
        let options = parse_retry_config(&format!("quic retry = {value}\n")).expect("parse config");
        assert_eq!(
            options.quic_retry_policy(),
            expected,
            "quic retry = {value}"
        );
    }
}

#[test]
fn quic_retry_last_occurrence_wins() {
    let options =
        parse_retry_config("quic retry = never\nquic retry = always\n").expect("parse config");
    assert_eq!(options.quic_retry_policy(), RetryPolicy::Always);
}

#[test]
fn quic_retry_invalid_value_is_config_error() {
    let error = parse_retry_config("quic retry = sometimes\n")
        .expect_err("an unknown quic retry value is a config error");
    let text = error.to_string();
    assert!(
        text.contains("invalid 'quic retry' value 'sometimes'")
            && text.contains("expected auto, always or never"),
        "error must name the directive, the value and the choices, got: {text}"
    );
}

#[test]
fn quic_retry_in_module_scope_is_config_error() {
    let error = parse_retry_config("[data]\npath = /srv/data\nquic retry = always\n")
        .expect_err("module-scoped quic retry is a config error");
    assert!(
        error
            .to_string()
            .contains("'quic retry' is a global-only directive"),
        "error must name the offending directive, got: {error}"
    );
}

/// The QUIC listener's pre-accept limits follow `--max-connections`, so a
/// daemon that serves four sessions never lets quinn hold more attempts.
#[test]
fn quic_incoming_limits_follow_max_connections() {
    let options =
        RuntimeOptions::parse(&[OsString::from("--max-connections"), OsString::from("4")])
            .expect("parse options");
    assert_eq!(options.quic_incoming_limits().max_incoming(), 4);
    let unlimited = RuntimeOptions::parse(&[]).expect("parse options");
    assert_eq!(
        unlimited.quic_incoming_limits(),
        rsync_io::quic::IncomingLimits::default()
    );
}
