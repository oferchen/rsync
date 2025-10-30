#[test]
fn runtime_options_parse_hostname_patterns() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nhosts allow = trusted.example.com,.example.org\nhosts deny = bad?.example.net\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse hostname hosts directives");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);

    let module = &modules[0];
    assert_eq!(module.hosts_allow.len(), 2);
    assert!(matches!(module.hosts_allow[0], HostPattern::Hostname(_)));
    assert!(matches!(module.hosts_allow[1], HostPattern::Hostname(_)));
    assert_eq!(module.hosts_deny.len(), 1);
    assert!(matches!(module.hosts_deny[0], HostPattern::Hostname(_)));
}

