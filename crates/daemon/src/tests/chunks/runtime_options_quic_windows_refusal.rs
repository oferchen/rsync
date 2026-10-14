// The QUIC daemon listener is Unix-only (it runs in a forked front process).
// A Windows daemon must refuse every QUIC directive as a fatal config error
// that names the directive, never start TCP-only while the operator believes
// QUIC is up. On Unix the same configurations parse, and listener startup
// applies its own checks.

/// Every QUIC directive alone, as an operator would write it.
const QUIC_DIRECTIVE_LINES: [(&str, &str); 4] = [
    ("quic cert file", "quic cert file = quic/server.pem\n"),
    ("quic key file", "quic key file = quic/server.key\n"),
    (
        "quic client ca file",
        "quic client ca file = quic/clients.pem\n",
    ),
    ("quic port", "quic port = 8730\n"),
];

fn parse_quic_config(line: &str) -> Result<RuntimeOptions, DaemonError> {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(&config_path, line).expect("write config");
    RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
}

#[test]
fn runtime_options_configured_quic_directive_names_each_directive() {
    for (directive, line) in QUIC_DIRECTIVE_LINES {
        // A lone cert or key is rejected for the missing half on Unix and for
        // the platform on Windows; either way the directive is not silent.
        match parse_quic_config(line) {
            Ok(options) => assert_eq!(options.configured_quic_directive(), Some(directive)),
            Err(error) => assert!(
                error.to_string().contains("quic"),
                "{directive}: unexpected error: {error}"
            ),
        }
    }
    let none = parse_quic_config("port = 873\n").expect("no QUIC directive");
    assert_eq!(none.configured_quic_directive(), None);
}

#[cfg(windows)]
#[test]
fn runtime_options_quic_directive_is_refused_on_windows() {
    for (directive, line) in QUIC_DIRECTIVE_LINES {
        let error =
            parse_quic_config(line).expect_err("a Windows daemon must refuse every QUIC directive");
        let text = error.to_string();
        assert!(
            text.contains(&format!("'{directive}'"))
                && text.contains("the QUIC daemon is not supported on Windows"),
            "{directive}: unexpected error: {text}"
        );
        assert_eq!(error.exit_code(), 1, "{directive}: config errors exit 1");
    }
}
