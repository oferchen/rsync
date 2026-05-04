// Tests use Unix-style paths and daemon functionality designed for Unix
#[cfg(all(test, unix))]
mod runtime_options_tests {
    use super::*;
    use std::ffi::OsString;
    use std::fs;

    use tempfile::{NamedTempFile, TempDir};

    #[cfg(unix)]
    fn set_owner_only_permissions(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(path, permissions).expect("set permissions");
    }

    #[cfg(not(unix))]
    fn set_owner_only_permissions(_path: &Path) {}

    #[test]
    fn default_has_expected_bind_address() {
        let options = RuntimeOptions::default();
        assert_eq!(options.bind_address(), DEFAULT_BIND_ADDRESS);
    }

    #[test]
    fn default_has_expected_port() {
        let options = RuntimeOptions::default();
        assert_eq!(options.port, DEFAULT_PORT);
    }

    #[test]
    fn default_has_no_modules() {
        let options = RuntimeOptions::default();
        assert!(options.modules().is_empty());
    }

    #[test]
    fn default_has_no_bandwidth_limit() {
        let options = RuntimeOptions::default();
        assert!(options.bandwidth_limit().is_none());
        assert!(options.bandwidth_burst().is_none());
        assert!(!options.bandwidth_limit_configured());
    }

    #[test]
    fn default_has_no_address_family() {
        let options = RuntimeOptions::default();
        assert!(options.address_family().is_none());
    }

    #[test]
    fn default_has_reverse_lookup_enabled() {
        let options = RuntimeOptions::default();
        assert!(options.reverse_lookup());
    }

    #[test]
    fn default_has_no_log_file() {
        let options = RuntimeOptions::default();
        assert!(options.log_file().is_none());
    }

    #[test]
    fn default_has_no_pid_file() {
        let options = RuntimeOptions::default();
        assert!(options.pid_file().is_none());
    }

    #[test]
    fn default_has_no_lock_file() {
        let options = RuntimeOptions::default();
        assert!(options.lock_file().is_none());
    }

    #[test]
    fn default_has_empty_motd() {
        let options = RuntimeOptions::default();
        assert!(options.motd_lines().is_empty());
    }

    #[test]
    fn parse_port_option() {
        let args = vec![OsString::from("--port"), OsString::from("8873")];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.port, 8873);
    }

    #[test]
    fn parse_port_default_when_not_specified() {
        let args: Vec<OsString> = vec![];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.port, DEFAULT_PORT);
    }

    #[test]
    fn parse_ipv4_option_sets_address_family() {
        let args = vec![OsString::from("--ipv4")];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.address_family(), Some(AddressFamily::Ipv4));
    }

    #[test]
    fn parse_ipv6_option_sets_address_family() {
        let args = vec![OsString::from("--ipv6")];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.address_family(), Some(AddressFamily::Ipv6));
    }

    #[test]
    fn ipv4_and_ipv6_conflict_is_rejected() {
        let args = vec![OsString::from("--ipv4"), OsString::from("--ipv6")];
        let result = RuntimeOptions::parse(&args);
        assert!(result.is_err());
    }

    #[test]
    fn ipv6_and_ipv4_conflict_is_rejected() {
        let args = vec![OsString::from("--ipv6"), OsString::from("--ipv4")];
        let result = RuntimeOptions::parse(&args);
        assert!(result.is_err());
    }

    #[test]
    fn parse_bind_address_option() {
        let args = vec![OsString::from("--address"), OsString::from("127.0.0.1")];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(
            options.bind_address(),
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))
        );
    }

    #[test]
    fn parse_bind_option_alias() {
        let args = vec![OsString::from("--bind"), OsString::from("192.168.1.1")];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(
            options.bind_address(),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))
        );
    }

    #[test]
    fn ipv6_bind_with_ipv4_flag_is_rejected() {
        let args = vec![
            OsString::from("--ipv4"),
            OsString::from("--address"),
            OsString::from("::1"),
        ];
        let result = RuntimeOptions::parse(&args);
        assert!(result.is_err());
    }

    #[test]
    fn ipv4_bind_with_ipv6_flag_is_rejected() {
        let args = vec![
            OsString::from("--ipv6"),
            OsString::from("--address"),
            OsString::from("127.0.0.1"),
        ];
        let result = RuntimeOptions::parse(&args);
        assert!(result.is_err());
    }

    #[test]
    fn parse_once_sets_max_sessions_to_one() {
        let args = vec![OsString::from("--once")];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.max_sessions, Some(NonZeroUsize::new(1).unwrap()));
    }

    #[test]
    fn parse_max_sessions_option() {
        let args = vec![OsString::from("--max-sessions"), OsString::from("10")];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.max_sessions, Some(NonZeroUsize::new(10).unwrap()));
    }

    #[test]
    fn parse_bwlimit_option() {
        let args = vec![OsString::from("--bwlimit"), OsString::from("1000")];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert!(options.bandwidth_limit().is_some());
        assert!(options.bandwidth_limit_configured());
    }

    #[test]
    fn parse_no_bwlimit_clears_limit() {
        let args = vec![OsString::from("--no-bwlimit")];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert!(options.bandwidth_limit().is_none());
        assert!(options.bandwidth_limit_configured());
    }

    #[test]
    fn duplicate_bwlimit_is_rejected() {
        let args = vec![
            OsString::from("--bwlimit"),
            OsString::from("1000"),
            OsString::from("--bwlimit"),
            OsString::from("2000"),
        ];
        let result = RuntimeOptions::parse(&args);
        assert!(result.is_err());
    }

    #[test]
    fn parse_log_file_option() {
        let args = vec![
            OsString::from("--log-file"),
            OsString::from("/var/log/rsyncd.log"),
        ];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(
            options.log_file(),
            Some(&PathBuf::from("/var/log/rsyncd.log"))
        );
    }

    #[test]
    fn duplicate_log_file_is_rejected() {
        let args = vec![
            OsString::from("--log-file"),
            OsString::from("/var/log/first.log"),
            OsString::from("--log-file"),
            OsString::from("/var/log/second.log"),
        ];
        let result = RuntimeOptions::parse(&args);
        assert!(result.is_err());
    }

    #[test]
    fn parse_lock_file_option() {
        let args = vec![
            OsString::from("--lock-file"),
            OsString::from("/var/run/rsyncd.lock"),
        ];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(
            options.lock_file(),
            Some(Path::new("/var/run/rsyncd.lock"))
        );
    }

    #[test]
    fn duplicate_lock_file_is_rejected() {
        let args = vec![
            OsString::from("--lock-file"),
            OsString::from("/first.lock"),
            OsString::from("--lock-file"),
            OsString::from("/second.lock"),
        ];
        let result = RuntimeOptions::parse(&args);
        assert!(result.is_err());
    }

    #[test]
    fn parse_pid_file_option() {
        let args = vec![
            OsString::from("--pid-file"),
            OsString::from("/var/run/rsyncd.pid"),
        ];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(
            options.pid_file(),
            Some(Path::new("/var/run/rsyncd.pid"))
        );
    }

    #[test]
    fn duplicate_pid_file_is_rejected() {
        let args = vec![
            OsString::from("--pid-file"),
            OsString::from("/first.pid"),
            OsString::from("--pid-file"),
            OsString::from("/second.pid"),
        ];
        let result = RuntimeOptions::parse(&args);
        assert!(result.is_err());
    }

    #[test]
    fn parse_detach_sets_flag() {
        let args = vec![OsString::from("--detach")];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert!(options.detach());
    }

    #[test]
    fn parse_no_detach_clears_flag() {
        let args = vec![OsString::from("--no-detach")];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert!(!options.detach());
    }

    #[test]
    fn detach_default_matches_platform() {
        let args: Vec<OsString> = vec![];
        let options = RuntimeOptions::parse(&args).expect("parse");
        #[cfg(unix)]
        assert!(options.detach());
        #[cfg(windows)]
        assert!(!options.detach());
    }

    #[test]
    fn last_detach_flag_wins() {
        let args = vec![
            OsString::from("--no-detach"),
            OsString::from("--detach"),
        ];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert!(options.detach());
    }

    #[test]
    fn parse_config_loads_modules_from_file() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[share]\npath = /srv/share\n").expect("write config");

        let args = vec![
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.modules().len(), 1);
        assert_eq!(options.modules()[0].name, "share");
    }

    #[test]
    fn parse_config_stores_config_path_for_sighup_reload() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[reload_test]\npath = /srv/reload\n").expect("write config");

        let args = vec![
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert!(
            options.config_path().is_some(),
            "config_path should be set when --config is used"
        );
        assert_eq!(options.config_path().unwrap(), file.path());
    }

    #[test]
    fn no_config_flag_yields_no_config_path() {
        let args: Vec<OsString> = vec![OsString::from("--port"), OsString::from("9999")];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert!(
            options.config_path().is_none(),
            "config_path should be None when no --config is used"
        );
    }

    #[test]
    fn parse_config_inline_form_loads_modules() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[data]\npath = /srv/data\n").expect("write config");

        let inline_arg = format!("--config={}", file.path().display());
        let args = vec![OsString::from(inline_arg)];
        let options = RuntimeOptions::parse(&args).expect("parse inline config");
        assert_eq!(options.modules().len(), 1);
        assert_eq!(options.modules()[0].name, "data");
    }

    #[test]
    fn parse_config_with_other_options() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[files]\npath = /srv/files\n").expect("write config");

        let args = vec![
            OsString::from("--port"),
            OsString::from("9999"),
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
            OsString::from("--once"),
        ];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.port, 9999);
        assert_eq!(options.modules().len(), 1);
        assert_eq!(options.modules()[0].name, "files");
    }

    #[test]
    fn unsupported_option_is_rejected() {
        let args = vec![OsString::from("--unknown-option")];
        let result = RuntimeOptions::parse(&args);
        assert!(result.is_err());
    }

    #[test]
    fn parse_motd_line_adds_to_motd() {
        let args = vec![
            OsString::from("--motd-line"),
            OsString::from("Welcome to the server"),
        ];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.motd_lines().len(), 1);
        assert_eq!(options.motd_lines()[0], "Welcome to the server");
    }

    #[test]
    fn parse_multiple_motd_lines() {
        let args = vec![
            OsString::from("--motd-line"),
            OsString::from("Line 1"),
            OsString::from("--motd-line"),
            OsString::from("Line 2"),
        ];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.motd_lines().len(), 2);
    }

    #[test]
    fn parse_motd_file() {
        let motd = NamedTempFile::new().expect("motd file");
        fs::write(motd.path(), "Hello\nWorld\n").expect("write motd");

        let args = vec![
            OsString::from("--motd-file"),
            motd.path().as_os_str().to_os_string(),
        ];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.motd_lines().len(), 2);
        assert_eq!(options.motd_lines()[0], "Hello");
        assert_eq!(options.motd_lines()[1], "World");
    }

    #[test]
    fn cli_secrets_file_sets_global_default_for_inline_modules() {
        let secrets = NamedTempFile::new().expect("secrets file");
        set_owner_only_permissions(secrets.path());

        let module_root = TempDir::new().expect("module path");
        let module_path = module_root.path().join("data");
        fs::create_dir_all(&module_path).expect("module dir");

        let args = vec![
            OsString::from("--secrets-file"),
            secrets.path().as_os_str().to_os_string(),
            OsString::from("--module"),
            OsString::from(format!(
                "docs={};auth users=alice",
                module_path.display()
            )),
        ];

        let options = RuntimeOptions::parse(&args).expect("parse");
        let module = options.modules().first().expect("module added");

        assert_eq!(module.secrets_file(), Some(secrets.path()));
    }

    #[test]
    fn duplicate_cli_secrets_file_is_rejected() {
        let first = NamedTempFile::new().expect("first secrets");
        let second = NamedTempFile::new().expect("second secrets");
        set_owner_only_permissions(first.path());
        set_owner_only_permissions(second.path());

        let args = vec![
            OsString::from("--secrets-file"),
            first.path().as_os_str().to_os_string(),
            OsString::from("--secrets-file"),
            second.path().as_os_str().to_os_string(),
        ];

        let result = RuntimeOptions::parse(&args);
        assert!(result.is_err());
    }

    #[test]
    fn parse_inline_module() {
        let module_root = TempDir::new().expect("module path");
        let module_path = module_root.path().join("data");
        fs::create_dir_all(&module_path).expect("module dir");

        let args = vec![
            OsString::from("--module"),
            OsString::from(format!("testmod={}", module_path.display())),
        ];

        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.modules().len(), 1);
        assert_eq!(options.modules()[0].name, "testmod");
    }

    #[test]
    fn duplicate_module_name_is_rejected() {
        let module_root = TempDir::new().expect("module path");
        let module_path = module_root.path().join("data");
        fs::create_dir_all(&module_path).expect("module dir");

        let args = vec![
            OsString::from("--module"),
            OsString::from(format!("testmod={}", module_path.display())),
            OsString::from("--module"),
            OsString::from(format!("testmod={}", module_path.display())),
        ];

        let result = RuntimeOptions::parse(&args);
        assert!(result.is_err());
    }

    #[test]
    fn runtime_options_clone_preserves_values() {
        let args = vec![OsString::from("--port"), OsString::from("9999")];
        let options = RuntimeOptions::parse(&args).expect("parse");
        let cloned = options.clone();
        assert_eq!(cloned.port, options.port);
        assert_eq!(cloned.brand(), options.brand());
    }

    #[test]
    fn runtime_options_debug_format() {
        let options = RuntimeOptions::default();
        let debug = format!("{options:?}");
        assert!(debug.contains("RuntimeOptions"));
    }

    #[test]
    fn runtime_options_equality() {
        let a = RuntimeOptions::default();
        let b = RuntimeOptions::default();
        assert_eq!(a, b);
    }

    #[test]
    fn default_has_no_syslog_config() {
        let options = RuntimeOptions::default();
        assert!(options.syslog_facility.is_none());
        assert!(options.syslog_tag.is_none());
    }

    #[test]
    fn syslog_config_loaded_from_config_file() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(
            file,
            "syslog facility = local5\nsyslog tag = my-daemon\n[m]\npath = /srv/m\n"
        )
        .expect("write config");

        let args = vec![
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.syslog_facility.as_deref(), Some("local5"));
        assert_eq!(options.syslog_tag.as_deref(), Some("my-daemon"));
    }

    #[test]
    fn syslog_config_defaults_to_none_when_absent() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[m]\npath = /srv/m\n").expect("write config");

        let args = vec![
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert!(options.syslog_facility.is_none());
        assert!(options.syslog_tag.is_none());
    }

    #[test]
    fn address_from_config_sets_bind_address() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "address = 10.0.0.5\n[m]\npath = /srv/m\n").expect("write config");

        let args = vec![
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(
            options.bind_address(),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))
        );
        assert!(options.bind_address_overridden);
    }

    #[test]
    fn cli_address_overrides_config_address() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "address = 10.0.0.5\n[m]\npath = /srv/m\n").expect("write config");

        let args = vec![
            OsString::from("--address"),
            OsString::from("192.168.1.1"),
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(
            options.bind_address(),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))
        );
    }

    #[test]
    fn config_without_address_keeps_default() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[m]\npath = /srv/m\n").expect("write config");

        let args = vec![
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.bind_address(), DEFAULT_BIND_ADDRESS);
        assert!(!options.bind_address_overridden);
    }
}
