#[test]
fn runtime_options_parse_pid_file_argument() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--pid-file"),
        OsString::from("/var/run/rsyncd.pid"),
    ])
    .expect("parse pid file argument");

    assert_eq!(options.pid_file(), Some(Path::new("/var/run/rsyncd.pid")));
}

#[test]
fn runtime_options_reject_duplicate_pid_file_argument() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--pid-file"),
        OsString::from("/var/run/one.pid"),
        OsString::from("--pid-file"),
        OsString::from("/var/run/two.pid"),
    ])
    .expect_err("duplicate pid file should fail");

    assert!(error.message().to_string().contains("--pid-file"));
}

#[test]
fn runtime_options_ipv6_sets_default_bind_address() {
    let options =
        RuntimeOptions::parse(&[OsString::from("--ipv6")]).expect("parse --ipv6 succeeds");

    assert_eq!(options.bind_address(), IpAddr::V6(Ipv6Addr::UNSPECIFIED));
    assert_eq!(options.address_family(), Some(AddressFamily::Ipv6));
}

#[test]
fn runtime_options_ipv6_accepts_ipv6_bind_address() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--bind"),
        OsString::from("::1"),
        OsString::from("--ipv6"),
    ])
    .expect("ipv6 bind succeeds");

    assert_eq!(options.bind_address(), IpAddr::V6(Ipv6Addr::LOCALHOST));
    assert_eq!(options.address_family(), Some(AddressFamily::Ipv6));
}

#[test]
fn runtime_options_bind_accepts_bracketed_ipv6() {
    let options = RuntimeOptions::parse(&[OsString::from("--bind"), OsString::from("[::1]")])
        .expect("parse bracketed ipv6");

    assert_eq!(options.bind_address(), IpAddr::V6(Ipv6Addr::LOCALHOST));
    assert_eq!(options.address_family(), Some(AddressFamily::Ipv6));
}

#[test]
fn runtime_options_bind_resolves_hostnames() {
    let options = RuntimeOptions::parse(&[OsString::from("--bind"), OsString::from("localhost")])
        .expect("parse hostname bind");

    let address = options.bind_address();
    assert!(
        address == IpAddr::V4(Ipv4Addr::LOCALHOST) || address == IpAddr::V6(Ipv6Addr::LOCALHOST),
        "unexpected resolved address {address}",
    );
}

#[test]
fn runtime_options_ipv6_rejects_ipv4_bind_address() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--bind"),
        OsString::from("127.0.0.1"),
        OsString::from("--ipv6"),
    ])
    .expect_err("ipv4 bind with --ipv6 should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("cannot use --ipv6 with an IPv4 bind address")
    );
}

#[test]
fn runtime_options_ipv4_rejects_ipv6_bind_address() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--bind"),
        OsString::from("::1"),
        OsString::from("--ipv4"),
    ])
    .expect_err("ipv6 bind with --ipv4 should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("cannot use --ipv4 with an IPv6 bind address")
    );
}

#[test]
fn runtime_options_rejects_ipv4_ipv6_combo() {
    let error = RuntimeOptions::parse(&[OsString::from("--ipv4"), OsString::from("--ipv6")])
        .expect_err("conflicting address families should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("cannot combine --ipv4 with --ipv6")
    );
}

#[test]
fn runtime_options_load_modules_from_config_file() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\ncomment = Documentation\n\n[logs]\npath=/var/log\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 2);
    assert_eq!(modules[0].name, "docs");
    assert_eq!(modules[0].path, PathBuf::from("/srv/docs"));
    assert_eq!(modules[0].comment.as_deref(), Some("Documentation"));
    assert!(modules[0].bandwidth_limit().is_none());
    assert!(modules[0].bandwidth_burst().is_none());
    assert!(modules[0].listable());
    assert_eq!(modules[1].name, "logs");
    assert_eq!(modules[1].path, PathBuf::from("/var/log"));
    assert!(modules[1].comment.is_none());
    assert!(modules[1].bandwidth_limit().is_none());
    assert!(modules[1].bandwidth_burst().is_none());
    assert!(modules[1].listable());
    assert!(modules.iter().all(ModuleDefinition::use_chroot));
}

#[test]
fn runtime_options_loads_pid_file_from_config() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    writeln!(
        File::create(&config_path).expect("create config"),
        "pid file = daemon.pid\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with pid file");

    let expected = dir.path().join("daemon.pid");
    assert_eq!(options.pid_file(), Some(expected.as_path()));
}

#[test]
fn runtime_options_config_pid_file_respects_cli_override() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    writeln!(
        File::create(&config_path).expect("create config"),
        "pid file = config.pid\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let cli_pid = PathBuf::from("/var/run/override.pid");
    let options = RuntimeOptions::parse(&[
        OsString::from("--pid-file"),
        cli_pid.as_os_str().to_os_string(),
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with cli override");

    assert_eq!(options.pid_file(), Some(cli_pid.as_path()));
}

#[test]
fn runtime_options_loads_lock_file_from_config() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    writeln!(
        File::create(&config_path).expect("create config"),
        "lock file = daemon.lock\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with lock file");

    let expected = dir.path().join("daemon.lock");
    assert_eq!(options.lock_file(), Some(expected.as_path()));
}

#[test]
fn runtime_options_config_lock_file_respects_cli_override() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    writeln!(
        File::create(&config_path).expect("create config"),
        "lock file = config.lock\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let cli_lock = PathBuf::from("/var/run/override.lock");
    let options = RuntimeOptions::parse(&[
        OsString::from("--lock-file"),
        cli_lock.as_os_str().to_os_string(),
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with cli lock override");

    assert_eq!(options.lock_file(), Some(cli_lock.as_path()));
}

#[test]
fn runtime_options_loads_bwlimit_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nbwlimit = 4M\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.name, "docs");
    assert_eq!(
        module.bandwidth_limit(),
        Some(NonZeroU64::new(4 * 1024 * 1024).unwrap())
    );
    assert!(module.bandwidth_burst().is_none());
    assert!(!module.bandwidth_burst_specified());
}

#[test]
fn runtime_options_loads_bwlimit_burst_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nbwlimit = 4M:16M\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.name, "docs");
    assert_eq!(
        module.bandwidth_limit(),
        Some(NonZeroU64::new(4 * 1024 * 1024).unwrap())
    );
    assert_eq!(
        module.bandwidth_burst(),
        Some(NonZeroU64::new(16 * 1024 * 1024).unwrap())
    );
    assert!(module.bandwidth_burst_specified());
}

#[test]
fn runtime_options_loads_global_bwlimit_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "bwlimit = 3M:12M\n[docs]\npath = /srv/docs\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    assert_eq!(
        options.bandwidth_limit(),
        Some(NonZeroU64::new(3 * 1024 * 1024).unwrap())
    );
    assert_eq!(
        options.bandwidth_burst(),
        Some(NonZeroU64::new(12 * 1024 * 1024).unwrap())
    );
    assert!(options.bandwidth_limit_configured());

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    assert!(modules[0].bandwidth_limit().is_none());
}

#[test]
fn runtime_options_global_bwlimit_respects_cli_override() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "bwlimit = 3M\n[docs]\npath = /srv/docs\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--bwlimit"),
        OsString::from("8M:32M"),
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config with cli override");

    assert_eq!(
        options.bandwidth_limit(),
        Some(NonZeroU64::new(8 * 1024 * 1024).unwrap())
    );
    assert_eq!(
        options.bandwidth_burst(),
        Some(NonZeroU64::new(32 * 1024 * 1024).unwrap())
    );
}

#[test]
fn runtime_options_loads_unlimited_global_bwlimit_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "bwlimit = 0\n[docs]\npath = /srv/docs\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    assert!(options.bandwidth_limit().is_none());
    assert!(options.bandwidth_burst().is_none());
    assert!(options.bandwidth_limit_configured());
}

#[test]
fn runtime_options_loads_refuse_options_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nrefuse options = delete, compress progress\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.name, "docs");
    assert_eq!(
        module.refused_options(),
        &["delete", "compress", "progress"]
    );
}

#[test]
fn runtime_options_loads_boolean_and_id_directives_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nread only = yes\nnumeric ids = on\nuid = 1234\ngid = 4321\nlist = no\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert!(module.read_only());
    assert!(module.numeric_ids());
    assert_eq!(module.uid(), Some(1234));
    assert_eq!(module.gid(), Some(4321));
    assert!(!module.listable());
    assert!(module.use_chroot());
}

#[test]
fn runtime_options_loads_use_chroot_directive_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nuse chroot = no\n",).expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    assert!(!modules[0].use_chroot());
}

#[test]
fn runtime_options_allows_relative_path_when_use_chroot_disabled() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = data/docs\nuse chroot = no\n",).expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].path, PathBuf::from("data/docs"));
    assert!(!modules[0].use_chroot());
}

#[test]
fn runtime_options_loads_timeout_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\ntimeout = 120\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.timeout().map(NonZeroU64::get), Some(120));
}

#[test]
fn runtime_options_allows_timeout_zero_in_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\ntimeout = 0\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert!(module.timeout().is_none());
}

#[test]
fn runtime_options_rejects_invalid_boolean_directive() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nread only = maybe\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("invalid boolean should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("invalid boolean value 'maybe'")
    );
}

