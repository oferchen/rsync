/// A module's empty `hosts allow` overrides a restrictive global default,
/// because upstream reads the module's own (empty) string and normalises it to
/// "no list" rather than falling back to the global one.
///
/// upstream: loadparm.c - `hosts allow` is a `P_LOCAL` string whose global
/// value seeds every module's default; access.c:275-278 then turns whatever
/// that module ends up holding into `NULL` when it is empty. So the empty
/// value has to reach the module as a *set* value, not as "unset" - mapping it
/// to absent would re-inherit the global list and deny the peer.
///
/// The `closed` module is the control: it names no directive, so it must still
/// inherit the global list and refuse. Without it this test would also pass if
/// the global default were simply never applied.
#[test]
fn runtime_options_empty_module_hosts_allow_overrides_the_global() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "hosts allow = 10.0.0.0/8\n\n[open]\npath = /srv/open\nhosts allow =\n\n[closed]\npath = /srv/closed\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse a global hosts allow with an empty module override");

    let modules = options.modules();
    assert_eq!(modules.len(), 2);

    let unlisted = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

    let open = &modules[0];
    assert_eq!(open.name, "open");
    assert!(open.hosts_allow.is_empty());
    assert!(open.permits(unlisted, PeerHost::new(None, true)));

    let closed = &modules[1];
    assert_eq!(closed.name, "closed");
    assert_eq!(closed.hosts_allow.len(), 1);
    assert!(!closed.permits(unlisted, PeerHost::new(None, true)));
    assert!(closed.permits(
        IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3)),
        PeerHost::new(None, true)
    ));
}
