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
    fn parse_port_zero_coerces_to_well_known_rsync_port() {
        // upstream: clientserver.c:1573-1574 -
        //   `if (rsync_port == 0 && (rsync_port = lp_rsync_port()) == 0)
        //        rsync_port = RSYNC_PORT;`
        // A resolved port of 0 (from `--port 0`) falls back to the well-known
        // rsync port 873 rather than binding a kernel-assigned ephemeral port.
        let args = vec![OsString::from("--port"), OsString::from("0")];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(
            options.port, DEFAULT_PORT,
            "--port 0 must coerce to the well-known rsync port 873, matching upstream"
        );
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
    fn ipv4_then_ipv6_enables_dual_stack() {
        // UTS-DD-daemon-exit10: `--ipv4 --ipv6` together is the explicit
        // opt-in for the upstream `default_af_hint = 0` semantic: bind one
        // listener per family and tolerate per-family failure. The flag
        // pair must succeed and surface as `dual_stack = true` rather than
        // being rejected as a conflict.
        let args = vec![OsString::from("--ipv4"), OsString::from("--ipv6")];
        let options = RuntimeOptions::parse(&args).expect("--ipv4 --ipv6 must parse");
        assert!(
            options.dual_stack(),
            "dual_stack must be set when both family flags are given"
        );
        assert_eq!(options.address_family(), Some(AddressFamily::Ipv4));
    }

    #[test]
    fn ipv6_then_ipv4_enables_dual_stack() {
        // Companion to `ipv4_then_ipv6_enables_dual_stack`: the order of
        // the family flags must not change the resulting dual-stack
        // request.
        let args = vec![OsString::from("--ipv6"), OsString::from("--ipv4")];
        let options = RuntimeOptions::parse(&args).expect("--ipv6 --ipv4 must parse");
        assert!(
            options.dual_stack(),
            "dual_stack must be set regardless of family-flag order"
        );
        assert_eq!(options.address_family(), Some(AddressFamily::Ipv6));
    }

    #[test]
    fn dual_stack_default_is_false() {
        // Default daemon startup (no flags, no explicit bind address) must
        // not enable dual-stack so the accept loop binds IPv4 only and
        // avoids the GitHub Actions exit-10 IPv6-then-fail pattern.
        let options = RuntimeOptions::default();
        assert!(!options.dual_stack());
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
    fn parse_max_connections_option() {
        let args = vec![OsString::from("--max-connections"), OsString::from("4")];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.max_connections, Some(NonZeroUsize::new(4).unwrap()));
    }

    #[test]
    fn parse_max_connections_zero_is_rejected() {
        let args = vec![OsString::from("--max-connections"), OsString::from("0")];
        assert!(RuntimeOptions::parse(&args).is_err());
    }

    #[test]
    fn parse_max_connections_non_numeric_is_rejected() {
        let args = vec![OsString::from("--max-connections"), OsString::from("nope")];
        assert!(RuntimeOptions::parse(&args).is_err());
    }

    #[test]
    fn duplicate_max_connections_is_rejected() {
        let args = vec![
            OsString::from("--max-connections"),
            OsString::from("2"),
            OsString::from("--max-connections"),
            OsString::from("3"),
        ];
        assert!(RuntimeOptions::parse(&args).is_err());
    }

    #[test]
    fn tcp_fastopen_defaults_to_auto() {
        let options = RuntimeOptions::parse(&[]).expect("parse");
        assert_eq!(options.tcp_fastopen(), TcpFastOpenMode::Auto);
    }

    #[test]
    fn parse_tcp_fastopen_on() {
        let args = vec![OsString::from("--tcp-fastopen"), OsString::from("on")];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.tcp_fastopen(), TcpFastOpenMode::On);
    }

    #[test]
    fn parse_tcp_fastopen_off() {
        let args = vec![OsString::from("--tcp-fastopen=off")];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.tcp_fastopen(), TcpFastOpenMode::Off);
    }

    #[test]
    fn parse_tcp_fastopen_rejects_unknown_value() {
        let args = vec![OsString::from("--tcp-fastopen=maybe")];
        assert!(RuntimeOptions::parse(&args).is_err());
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
    fn parses_short_verbose_stack() {
        let options =
            RuntimeOptions::parse(&[OsString::from("-vvv")]).expect("parse -vvv");
        assert_eq!(options.verbosity(), 3);
    }

    #[test]
    fn parses_long_verbose_and_reset() {
        let options = RuntimeOptions::parse(&[
            OsString::from("--verbose"),
            OsString::from("-v"),
            OsString::from("--verbose"),
        ])
        .expect("parse stacked verbose");
        assert_eq!(options.verbosity(), 3);

        let reset = RuntimeOptions::parse(&[
            OsString::from("-vv"),
            OsString::from("--no-verbose"),
        ])
        .expect("parse with reset");
        assert_eq!(reset.verbosity(), 0);
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

    #[test]
    fn port_from_config_sets_binding_port() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "port = 8873\n[m]\npath = /srv/m\n").expect("write config");

        let args = vec![
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.port, 8873);
        assert_eq!(options.rsync_port, Some(8873));
    }

    #[test]
    fn cli_port_overrides_config_port() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "port = 8873\n[m]\npath = /srv/m\n").expect("write config");

        let args = vec![
            OsString::from("--port"),
            OsString::from("9999"),
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.port, 9999);
    }

    #[test]
    fn config_without_port_keeps_default() {
        let mut file = NamedTempFile::new().expect("config file");
        writeln!(file, "[m]\npath = /srv/m\n").expect("write config");

        let args = vec![
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
        ];
        let options = RuntimeOptions::parse(&args).expect("parse");
        assert_eq!(options.port, DEFAULT_PORT);
        assert!(options.rsync_port.is_none());
    }
}
