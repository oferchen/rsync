#[test]
fn runtime_options_parse_hosts_allow_and_deny() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nhosts allow = 127.0.0.1,192.168.0.0/24\nhosts deny = 192.168.0.5\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse hosts directives");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);

    let module = &modules[0];
    assert_eq!(module.hosts_allow.len(), 2);
    assert!(matches!(
        module.hosts_allow[0],
        HostPattern::Ipv4 { prefix: 32, .. }
    ));
    assert!(matches!(
        module.hosts_allow[1],
        HostPattern::Ipv4 { prefix: 24, .. }
    ));
    assert_eq!(module.hosts_deny.len(), 1);
    assert!(matches!(
        module.hosts_deny[0],
        HostPattern::Ipv4 { prefix: 32, .. }
    ));
}

