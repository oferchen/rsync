/// An empty `hosts allow` / `hosts deny` value is legal config, not a parse
/// error, and it means "no list".
///
/// upstream: access.c:275-278 - `allow_access()` normalises an empty list
/// string to `NULL` before deciding anything, so the directive with no value
/// behaves exactly as if it were absent. Refusing it aborted the daemon at
/// startup where upstream serves: measured against real rsync 3.5.0, a config
/// carrying `hosts allow =` starts and listens, while oc exited 1 with
/// "hosts allow directive must specify at least one pattern".
#[test]
fn runtime_options_accept_an_empty_hosts_directive() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nhosts allow =\nhosts deny =\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("an empty hosts directive is legal config");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);

    let module = &modules[0];
    assert!(module.hosts_allow.is_empty());
    assert!(module.hosts_deny.is_empty());

    // The empty list is what carries upstream's "no restriction"; assert the
    // decision, not just the representation.
    assert!(module.permits(
        IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)),
        PeerHost::new(None, true)
    ));
}
