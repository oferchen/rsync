#[test]
fn runtime_options_module_definition_parses_inline_options() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from(
            "mirror=./data;use-chroot=no;read-only=yes;write-only=yes;list=no;numeric-ids=yes;hosts-allow=192.0.2.0/24;auth-users=alice,bob;secrets-file=/etc/oc-rsyncd/oc-rsyncd.secrets;bwlimit=1m;refuse-options=compress;uid=1000;gid=2000;timeout=600;max-connections=5",
        ),
    ])
    .expect("parse module with inline options");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.name(), "mirror");
    assert_eq!(module.path, PathBuf::from("./data"));
    assert!(module.read_only());
    assert!(module.write_only());
    assert!(!module.listable());
    assert!(module.numeric_ids());
    assert!(!module.use_chroot());
    assert!(module.permits(
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 42)),
        Some("host.example")
    ));
    assert_eq!(module.auth_users().len(), 2);
    assert_eq!(module.auth_users()[0].username, "alice");
    assert_eq!(module.auth_users()[1].username, "bob");
    assert_eq!(
        module
            .secrets_file()
            .map(|path| path.to_string_lossy().into_owned()),
        Some(String::from(branding::OC_DAEMON_SECRETS_PATH))
    );
    assert_eq!(
        module.bandwidth_limit().map(NonZeroU64::get),
        Some(1_048_576)
    );
    assert!(module.bandwidth_burst().is_none());
    assert!(!module.bandwidth_burst_specified());
    assert_eq!(module.refused_options(), [String::from("compress")]);
    assert_eq!(module.uid(), Some(1000));
    assert_eq!(module.gid(), Some(2000));
    assert_eq!(module.timeout().map(NonZeroU64::get), Some(600));
    assert_eq!(module.max_connections().map(NonZeroU32::get), Some(5));
}

