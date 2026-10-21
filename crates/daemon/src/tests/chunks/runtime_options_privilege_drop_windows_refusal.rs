// Windows has no setuid/setgid, so a Windows daemon cannot honour a numeric
// uid/gid drop. It must refuse every privilege-drop directive as a fatal config
// error that names the directive, never start and serve with the privileges
// the operator asked to drop. Unix parses the same configurations unchanged.

/// Every privilege-drop directive alone, with the name the refusal reports.
const PRIVILEGE_DROP_CONFIGS: [(&str, &str); 5] = [
    ("'daemon uid'", "daemon uid = 1000\n"),
    ("'daemon gid'", "daemon gid = 1000\n"),
    ("'uid' in module [data]", "[data]\npath = .\nuid = 1000\n"),
    ("'gid' in module [data]", "[data]\npath = .\ngid = 1000\n"),
    (
        "'uid' in module [data]",
        "[data]\npath = .\nuid = oc-rsync-no-such-user\n",
    ),
];

fn parse_privilege_config(text: &str) -> Result<RuntimeOptions, DaemonError> {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(&config_path, text).expect("write config");
    RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
}

#[cfg(not(windows))]
#[test]
fn runtime_options_privilege_drop_directive_names_each_directive() {
    for (directive, text) in PRIVILEGE_DROP_CONFIGS {
        let options = parse_privilege_config(text).expect("Unix accepts a privilege drop");
        assert_eq!(
            options.configured_privilege_drop_directive().as_deref(),
            Some(directive)
        );
    }
    let none = parse_privilege_config("[data]\npath = .\nuid =\n").expect("no privilege drop");
    assert_eq!(none.configured_privilege_drop_directive(), None);
}

#[cfg(windows)]
#[test]
fn runtime_options_privilege_drop_is_refused_on_windows() {
    for (directive, text) in PRIVILEGE_DROP_CONFIGS {
        let error = parse_privilege_config(text)
            .expect_err("a Windows daemon must refuse every uid/gid directive");
        let message = error.to_string();
        assert!(
            message.contains(directive)
                && message.contains("a Windows daemon cannot drop privileges"),
            "{directive}: unexpected error: {message}"
        );
        assert_eq!(error.exit_code(), 1, "{directive}: config errors exit 1");
    }
    parse_privilege_config("[data]\npath = .\nuid =\n")
        .expect("an empty uid requests no drop and must still start");
}
