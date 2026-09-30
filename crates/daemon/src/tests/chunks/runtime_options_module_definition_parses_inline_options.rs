#[test]
fn runtime_options_module_definition_parses_inline_options() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from(
            "mirror=./data;use-chroot=no;read-only=yes;write-only=yes;list=no;numeric-ids=yes;hosts-allow=192.0.2.0/24;auth-users=alice,bob;secrets-file=/etc/oc-rsync/oc-rsyncd.secrets;refuse-options=compress;uid=1000;gid=2000;timeout=600;max-connections=5",
        ),
    ])
    .expect("parse module with inline options");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.name(), "mirror");
    // `./data` is resolved against the current directory and cleaned, so the
    // leading `.` does not survive: upstream `normalize_path` joins onto
    // `curr_dir` and then runs `clean_fname(..., CFN_COLLAPSE_DOT_DOT_DIRS |
    // CFN_DROP_TRAILING_DOT_DIR)` (util1.c:1409-1420).
    assert_eq!(
        module.path,
        std::env::current_dir().expect("current dir").join("data")
    );
    assert!(module.read_only());
    assert!(module.write_only());
    assert!(!module.listable());
    assert!(module.numeric_ids());
    assert!(!module.use_chroot());
    assert!(module.permits(
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 42)),
        PeerHost::new(Some("host.example"), true)
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
    assert_eq!(module.refused_options(), [String::from("compress")]);
    assert_eq!(module.uid(), Some(1000));
    assert_eq!(module.gid(), Some(2000));
    assert_eq!(module.timeout().map(NonZeroU64::get), Some(600));
    assert_eq!(
        module.max_connections(),
        MaxConnections::Limited(NonZeroU32::new(5).expect("non-zero"))
    );
}
