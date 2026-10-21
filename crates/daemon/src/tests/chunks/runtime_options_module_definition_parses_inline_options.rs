#[test]
fn runtime_options_module_definition_parses_inline_options() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from(
            "mirror=./data;use chroot=no;read only=yes;write only=yes;list=no;numeric ids=yes;hosts allow=192.0.2.0/24;auth users=alice,bob;secrets file=/etc/oc-rsync/oc-rsyncd.secrets;refuse options=compress;timeout=600;max connections=5",
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
    // CFN_DROP_TRAILING_DOT_DIR)` (util1.c:1504-1515).
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
    assert_eq!(module.timeout().map(NonZeroU64::get), Some(600));
    assert_eq!(
        module.max_connections(),
        MaxConnections::Limited(NonZeroU32::new(5).expect("non-zero"))
    );
}

/// Inline `uid`/`gid` options resolve into the module's privilege drop. Unix
/// only: a Windows daemon cannot drop to a uid or gid, so it refuses them.
#[cfg(unix)]
#[test]
fn runtime_options_module_definition_parses_inline_uid_and_gid() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from("mirror=./data;uid=1000;gid=2000"),
    ])
    .expect("parse module with inline uid and gid");
    let module = &options.modules()[0];
    assert_eq!(module.uid(), Some(1000));
    assert_eq!(module.gid(), Some(2000));
}

/// The same inline `uid`/`gid` options stop a Windows daemon from starting,
/// naming the first directive it cannot honour.
#[cfg(windows)]
#[test]
fn runtime_options_module_definition_refuses_inline_uid_on_windows() {
    for (spec, directive) in [
        ("mirror=./data;uid=1000;gid=2000", "'uid' in module [mirror]"),
        ("mirror=./data;gid=2000", "'gid' in module [mirror]"),
    ] {
        let error = RuntimeOptions::parse(&[OsString::from("--module"), OsString::from(spec)])
            .expect_err("a Windows daemon must refuse an inline uid or gid");
        let message = error.to_string();
        assert!(
            message.contains(directive)
                && message.contains("a Windows daemon cannot drop privileges"),
            "{spec}: unexpected error: {message}"
        );
        assert_eq!(error.exit_code(), 1, "{spec}: config errors exit 1");
    }
}
