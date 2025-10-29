#[test]
fn runtime_options_rejects_duplicate_use_chroot_directive() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nuse chroot = yes\nuse chroot = no\n",
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("duplicate directive should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate 'use chroot' directive")
    );
}

#[test]
fn runtime_options_rejects_relative_path_with_chroot_enabled() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = data/docs\nuse chroot = yes\n",).expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("relative path with chroot should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("requires an absolute path when 'use chroot' is enabled")
    );
}

#[test]
fn runtime_options_rejects_invalid_list_directive() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nlist = maybe\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("invalid list boolean should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("invalid boolean value 'maybe' for 'list'")
    );
}

#[test]
fn runtime_options_apply_global_refuse_options() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "refuse options = compress, delete\n[docs]\npath = /srv/docs\n[logs]\npath = /srv/logs\nrefuse options = stats\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config with global refuse options");

    assert_eq!(
        options.modules()[0].refused_options(),
        ["compress", "delete"]
    );
    assert_eq!(options.modules()[1].refused_options(), ["stats"]);
}

#[test]
fn runtime_options_cli_modules_inherit_global_refuse_options() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "refuse options = compress\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
        OsString::from("--module"),
        OsString::from("extra=/srv/extra"),
    ])
    .expect("parse config with cli module");

    assert_eq!(options.modules()[0].refused_options(), ["compress"]);
}

#[test]
fn runtime_options_loads_modules_from_included_config() {
    let dir = tempdir().expect("tempdir");
    let include_path = dir.path().join("modules.conf");
    writeln!(
        File::create(&include_path).expect("create include"),
        "[docs]\npath = /srv/docs\n"
    )
    .expect("write include");

    let main_path = dir.path().join("rsyncd.conf");
    writeln!(
        File::create(&main_path).expect("create config"),
        "include = modules.conf\n"
    )
    .expect("write main config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        main_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with include");

    assert_eq!(options.modules().len(), 1);
    assert_eq!(options.modules()[0].name(), "docs");
}

#[test]
fn parse_config_modules_detects_recursive_include() {
    let dir = tempdir().expect("tempdir");
    let first = dir.path().join("first.conf");
    let second = dir.path().join("second.conf");

    writeln!(
        File::create(&first).expect("create first"),
        "include = second.conf\n"
    )
    .expect("write first");
    writeln!(
        File::create(&second).expect("create second"),
        "include = first.conf\n"
    )
    .expect("write second");

    let error = parse_config_modules(&first).expect_err("recursive include should fail");
    assert!(error.message().to_string().contains("recursive include"));
}

#[test]
fn runtime_options_rejects_duplicate_global_refuse_options() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "refuse options = compress\nrefuse options = delete\n[docs]\npath = /srv/docs\n",
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("duplicate global refuse options should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate 'refuse options' directive")
    );
}

#[test]
fn runtime_options_rejects_duplicate_global_bwlimit() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "bwlimit = 1M\nbwlimit = 2M\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("duplicate global bwlimit should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate 'bwlimit' directive in global section")
    );
}

#[test]
fn runtime_options_rejects_invalid_uid() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nuid = alpha\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("invalid uid should fail");

    assert!(error.message().to_string().contains("invalid uid"));
}

#[test]
fn runtime_options_rejects_invalid_timeout() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\ntimeout = never\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("invalid timeout should fail");

    assert!(error.message().to_string().contains("invalid timeout"));
}

#[test]
fn runtime_options_rejects_invalid_bwlimit_in_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nbwlimit = nope\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("invalid bwlimit should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("invalid 'bwlimit' value 'nope'")
    );
}

#[test]
fn runtime_options_rejects_duplicate_bwlimit_in_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nbwlimit = 1M\nbwlimit = 2M\n"
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("duplicate bwlimit should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate 'bwlimit' directive")
    );
}

#[test]
fn runtime_options_loads_max_connections_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nmax connections = 7\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("config parses");

    assert_eq!(options.modules[0].max_connections(), NonZeroU32::new(7));
}

#[test]
fn runtime_options_loads_unlimited_max_connections_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nmax connections = 0\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("config parses");

    assert!(options.modules[0].max_connections().is_none());
}

#[test]
fn runtime_options_rejects_invalid_max_connections() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nmax connections = nope\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("invalid max connections should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("invalid max connections value")
    );
}

#[test]
fn runtime_options_rejects_duplicate_refuse_options_directives() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nrefuse options = delete\nrefuse options = compress\n"
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("duplicate refuse options should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate 'refuse options' directive")
    );
}

#[test]
fn runtime_options_rejects_empty_refuse_options_directive() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nrefuse options =   \n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("empty refuse options should fail");

    let rendered = error.message().to_string();
    assert!(rendered.contains("must specify at least one option"));
}

#[test]
fn runtime_options_parse_bwlimit_argument() {
    let options = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from("8M")])
        .expect("parse bwlimit");

    assert_eq!(
        options.bandwidth_limit(),
        Some(NonZeroU64::new(8 * 1024 * 1024).unwrap())
    );
    assert!(options.bandwidth_burst().is_none());
    assert!(options.bandwidth_limit_configured());
}

#[test]
fn runtime_options_parse_bwlimit_argument_with_burst() {
    let options = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from("8M:12M")])
        .expect("parse bwlimit with burst");

    assert_eq!(
        options.bandwidth_limit(),
        Some(NonZeroU64::new(8 * 1024 * 1024).unwrap())
    );
    assert_eq!(
        options.bandwidth_burst(),
        Some(NonZeroU64::new(12 * 1024 * 1024).unwrap())
    );
    assert!(options.bandwidth_limit_configured());
}

#[test]
fn runtime_options_reject_whitespace_wrapped_bwlimit_argument() {
    let error = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from(" 8M \n")])
        .expect_err("whitespace-wrapped bwlimit should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("--bwlimit= 8M \n is invalid")
    );
}

#[test]
fn runtime_options_parse_bwlimit_unlimited() {
    let options = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from("0")])
        .expect("parse unlimited");

    assert!(options.bandwidth_limit().is_none());
    assert!(options.bandwidth_burst().is_none());
    assert!(options.bandwidth_limit_configured());
}

#[test]
fn runtime_options_parse_bwlimit_unlimited_ignores_burst() {
    let options = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from("0:512K")])
        .expect("parse unlimited with burst");

    assert!(options.bandwidth_limit().is_none());
    assert!(options.bandwidth_burst().is_none());
    assert!(options.bandwidth_limit_configured());
}

#[test]
fn runtime_options_parse_no_bwlimit_argument() {
    let options =
        RuntimeOptions::parse(&[OsString::from("--no-bwlimit")]).expect("parse no-bwlimit");

    assert!(options.bandwidth_limit().is_none());
    assert!(options.bandwidth_burst().is_none());
    assert!(options.bandwidth_limit_configured());
}

#[test]
fn runtime_options_reject_invalid_bwlimit() {
    let error = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from("foo")])
        .expect_err("invalid bwlimit should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("--bwlimit=foo is invalid")
    );
}

#[test]
fn runtime_options_reject_duplicate_bwlimit() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--bwlimit"),
        OsString::from("8M"),
        OsString::from("--bwlimit"),
        OsString::from("16M"),
    ])
    .expect_err("duplicate bwlimit should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate daemon argument '--bwlimit'")
    );
}

#[test]
fn runtime_options_parse_log_file_argument() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--log-file"),
        OsString::from("/var/log/rsyncd.log"),
    ])
    .expect("parse log file argument");

    assert_eq!(
        options.log_file(),
        Some(&PathBuf::from("/var/log/rsyncd.log"))
    );
}

#[test]
fn runtime_options_reject_duplicate_log_file_argument() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--log-file"),
        OsString::from("/tmp/one.log"),
        OsString::from("--log-file"),
        OsString::from("/tmp/two.log"),
    ])
    .expect_err("duplicate log file should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate daemon argument '--log-file'")
    );
}

