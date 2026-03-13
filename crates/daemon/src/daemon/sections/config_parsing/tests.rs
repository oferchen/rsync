#[cfg(test)]
mod config_parsing_tests {
    use super::*;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

    fn write_config(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().expect("create temp file");
        file.write_all(content.as_bytes()).expect("write config");
        file.flush().expect("flush");
        file
    }

    // --- resolve_config_relative_path tests ---

    #[test]
    fn resolve_config_relative_path_absolute() {
        let result = resolve_config_relative_path(Path::new("/etc/rsyncd.conf"), "/var/run/rsync.pid");
        assert_eq!(result, PathBuf::from("/var/run/rsync.pid"));
    }

    #[test]
    fn resolve_config_relative_path_relative() {
        let result = resolve_config_relative_path(Path::new("/etc/rsyncd.conf"), "rsync.pid");
        assert_eq!(result, PathBuf::from("/etc/rsync.pid"));
    }

    #[test]
    fn resolve_config_relative_path_nested() {
        let result = resolve_config_relative_path(Path::new("/etc/rsync/main.conf"), "sub/file.txt");
        assert_eq!(result, PathBuf::from("/etc/rsync/sub/file.txt"));
    }

    #[test]
    fn resolve_config_relative_path_no_parent() {
        let result = resolve_config_relative_path(Path::new("config.conf"), "relative.txt");
        assert_eq!(result, PathBuf::from("relative.txt"));
    }

    // --- parse_config_modules basic tests ---

    #[test]
    fn parse_empty_config() {
        let file = write_config("");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules.is_empty());
        assert!(result.motd_lines.is_empty());
        assert!(result.pid_file.is_none());
    }

    #[test]
    fn parse_comments_and_blanks() {
        let file = write_config("# Comment line\n\n; Another comment\n   \n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules.is_empty());
    }

    #[test]
    fn parse_single_module() {
        let dir = TempDir::new().expect("create temp dir");
        let module_path = dir.path().join("data");
        fs::create_dir(&module_path).expect("create module dir");

        let config = format!(
            "[mymodule]\npath = {}\ncomment = Test module\n",
            module_path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 1);
        assert_eq!(result.modules[0].name, "mymodule");
        assert_eq!(result.modules[0].path, module_path);
        assert_eq!(result.modules[0].comment, Some("Test module".to_owned()));
    }

    #[test]
    fn parse_multiple_modules() {
        let dir = TempDir::new().expect("create temp dir");
        let path1 = dir.path().join("data1");
        let path2 = dir.path().join("data2");
        fs::create_dir(&path1).expect("create dir 1");
        fs::create_dir(&path2).expect("create dir 2");

        let config = format!(
            "[mod1]\npath = {}\n\n[mod2]\npath = {}\n",
            path1.display(),
            path2.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 2);
        assert_eq!(result.modules[0].name, "mod1");
        assert_eq!(result.modules[1].name, "mod2");
    }

    // --- Module header error tests ---

    #[test]
    fn parse_unterminated_module_header() {
        let file = write_config("[unclosed\npath = /tmp\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("unterminated"));
    }

    #[test]
    fn parse_empty_module_name() {
        let file = write_config("[]\npath = /tmp\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("non-empty"));
    }

    #[test]
    fn parse_module_name_with_slash() {
        let file = write_config("[bad/name]\npath = /tmp\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("path separator"));
    }

    #[test]
    fn parse_trailing_chars_after_header() {
        let file = write_config("[module] extra\npath = /tmp\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("unexpected characters"));
    }

    #[test]
    fn parse_trailing_comment_after_header() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[module] # comment\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules.len(), 1);
    }

    // --- Directive parsing tests ---

    #[test]
    fn parse_missing_equals() {
        let file = write_config("[module]\npath /tmp\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("key = value"));
    }

    #[test]
    fn parse_unknown_global_directive_warns_and_continues() {
        let file = write_config("unknown = value\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds with warning");
        assert!(result.modules.is_empty());
    }

    // --- Global directive tests ---

    #[test]
    fn parse_global_pid_file() {
        let file = write_config("pid file = /var/run/rsync.pid\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.pid_file.is_some());
        let (path, _) = result.pid_file.unwrap();
        assert!(path.ends_with("rsync.pid"));
    }

    #[test]
    fn parse_global_reverse_lookup_true() {
        let file = write_config("reverse lookup = yes\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.reverse_lookup, Some((true, ConfigDirectiveOrigin { path: file.path().canonicalize().unwrap(), line: 1 })));
    }

    #[test]
    fn parse_global_reverse_lookup_false() {
        let file = write_config("reverse lookup = no\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (value, _) = result.reverse_lookup.unwrap();
        assert!(!value);
    }

    #[test]
    fn parse_global_lock_file() {
        let file = write_config("lock file = /var/lock/rsync.lock\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.lock_file.is_some());
    }

    #[test]
    fn parse_global_bwlimit() {
        let file = write_config("bwlimit = 1000\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.global_bandwidth_limit.is_some());
    }

    #[test]
    fn parse_global_incoming_chmod() {
        let file = write_config("incoming chmod = u+rwx,g+rx\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (value, _) = result.global_incoming_chmod.unwrap();
        assert_eq!(value, "u+rwx,g+rx");
    }

    #[test]
    fn parse_global_outgoing_chmod() {
        let file = write_config("outgoing chmod = a+r\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (value, _) = result.global_outgoing_chmod.unwrap();
        assert_eq!(value, "a+r");
    }

    // --- MOTD tests ---

    #[test]
    fn parse_inline_motd() {
        let file = write_config("motd = Welcome to rsync\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.motd_lines, vec!["Welcome to rsync"]);
    }

    #[test]
    fn parse_motd_file() {
        let motd_file = write_config("Line 1\nLine 2\nLine 3\n");
        let config = format!("motd file = {}\n", motd_file.path().display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.motd_lines.len(), 3);
        assert_eq!(result.motd_lines[0], "Line 1");
    }

    // --- Module directive tests ---

    #[test]
    fn parse_module_read_only() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nread only = no\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(!result.modules[0].read_only);
    }

    #[test]
    fn parse_module_write_only() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nwrite only = yes\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].write_only);
    }

    #[test]
    fn parse_module_use_chroot() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nuse chroot = false\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(!result.modules[0].use_chroot);
    }

    #[test]
    fn parse_module_numeric_ids() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nnumeric ids = true\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].numeric_ids);
    }

    #[test]
    fn parse_module_list() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nlist = no\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(!result.modules[0].listable);
    }

    #[test]
    fn parse_module_fake_super() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nfake super = yes\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].fake_super);
    }

    #[test]
    fn parse_module_fake_super_default_false() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(!result.modules[0].fake_super);
    }

    #[test]
    fn parse_module_uid_gid() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nuid = 1000\ngid = 1000\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules[0].uid, Some(1000));
        assert_eq!(result.modules[0].gid, Some(1000));
    }

    #[test]
    fn parse_module_timeout() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\ntimeout = 300\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules[0].timeout.unwrap().get(), 300);
    }

    #[test]
    fn parse_module_max_connections() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nmax connections = 10\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules[0].max_connections.unwrap().get(), 10);
    }

    // --- Include directive tests ---

    #[test]
    fn parse_include_directive() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let included = format!("[included_mod]\npath = {}\n", path.display());
        let include_file = write_config(&included);

        let main_config = format!("include = {}\n", include_file.path().display());
        let main_file = write_config(&main_config);

        let result = parse_config_modules(main_file.path()).expect("parse succeeds");
        assert_eq!(result.modules.len(), 1);
        assert_eq!(result.modules[0].name, "included_mod");
    }

    #[test]
    fn parse_recursive_include_detected() {
        let dir = TempDir::new().expect("create temp dir");
        let config_path = dir.path().join("config.conf");

        // Write config that includes itself
        let content = format!("include = {}\n", config_path.display());
        fs::write(&config_path, &content).expect("write config");

        let err = parse_config_modules(&config_path).expect_err("should fail");
        assert!(err.to_string().contains("recursive include"));
    }

    // --- Empty value error tests ---

    #[test]
    fn parse_empty_path_errors() {
        let file = write_config("[mod]\npath = \n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn parse_empty_pid_file_errors() {
        let file = write_config("pid file = \n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn parse_empty_include_errors() {
        let file = write_config("include = \n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn parse_empty_bwlimit_errors() {
        let file = write_config("bwlimit = \n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("must not be empty"));
    }

    // --- Duplicate directive tests ---

    #[test]
    fn parse_duplicate_pid_file_errors() {
        let file = write_config("pid file = /var/run/a.pid\npid file = /var/run/b.pid\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn parse_duplicate_reverse_lookup_errors() {
        let file = write_config("reverse lookup = yes\nreverse lookup = no\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("duplicate"));
    }

    // --- Invalid boolean tests ---

    #[test]
    fn parse_invalid_boolean_errors() {
        let file = write_config("[mod]\npath = /tmp\nread only = maybe\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("invalid boolean"));
    }

    // --- Case insensitivity tests ---

    #[test]
    fn parse_keys_case_insensitive() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\nPATH = {}\nREAD ONLY = NO\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules.len(), 1);
        assert!(!result.modules[0].read_only);
    }

    // --- Munge symlinks directive tests ---

    #[test]
    fn parse_module_munge_symlinks_yes() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nmunge symlinks = yes\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules[0].munge_symlinks, Some(true));
    }

    #[test]
    fn parse_module_munge_symlinks_no() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nmunge symlinks = no\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules[0].munge_symlinks, Some(false));
    }

    #[test]
    fn parse_module_munge_symlinks_default_none() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].munge_symlinks.is_none());
    }

    #[test]
    fn parse_module_munge_symlinks_invalid_boolean() {
        let file = write_config("[mod]\npath = /tmp\nmunge symlinks = maybe\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("invalid boolean"));
    }

    #[test]
    fn parse_module_munge_symlinks_duplicate() {
        let file = write_config(
            "[mod]\npath = /tmp\nmunge symlinks = yes\nmunge symlinks = no\n",
        );
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("duplicate"));
    }

    // --- New directive parsing tests ---

    #[test]
    fn parse_module_max_verbosity() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nmax verbosity = 3\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules[0].max_verbosity, 3);
    }

    #[test]
    fn parse_module_max_verbosity_default() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules[0].max_verbosity, 1);
    }

    #[test]
    fn parse_module_max_verbosity_invalid() {
        let file = write_config("[mod]\npath = /tmp\nmax verbosity = abc\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("invalid integer"));
    }

    #[test]
    fn parse_module_ignore_errors() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nignore errors = yes\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].ignore_errors);
    }

    #[test]
    fn parse_module_ignore_nonreadable() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nignore nonreadable = true\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].ignore_nonreadable);
    }

    #[test]
    fn parse_module_transfer_logging() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\ntransfer logging = yes\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].transfer_logging);
    }

    #[test]
    fn parse_module_log_format() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nlog format = %o %h %f %l\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(
            result.modules[0].log_format.as_deref(),
            Some("%o %h %f %l")
        );
    }

    #[test]
    fn parse_module_dont_compress() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\ndont compress = *.gz *.zip\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(
            result.modules[0].dont_compress.as_deref(),
            Some("*.gz *.zip")
        );
    }

    #[test]
    fn parse_module_early_exec() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nearly exec = /usr/bin/early-check\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(
            result.modules[0].early_exec.as_deref(),
            Some("/usr/bin/early-check")
        );
    }

    #[test]
    fn parse_module_pre_xfer_exec() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\npre-xfer exec = /usr/bin/check\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(
            result.modules[0].pre_xfer_exec.as_deref(),
            Some("/usr/bin/check")
        );
    }

    #[test]
    fn parse_module_post_xfer_exec() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\npost-xfer exec = /usr/bin/cleanup\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(
            result.modules[0].post_xfer_exec.as_deref(),
            Some("/usr/bin/cleanup")
        );
    }

    #[test]
    fn parse_module_name_converter() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nname converter = /usr/local/bin/nameconv\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(
            result.modules[0].name_converter.as_deref(),
            Some("/usr/local/bin/nameconv")
        );
    }

    #[test]
    fn parse_module_temp_dir() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\ntemp dir = /tmp/rsync\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(
            result.modules[0].temp_dir.as_deref(),
            Some("/tmp/rsync")
        );
    }

    #[test]
    fn parse_module_charset() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\ncharset = utf-8\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules[0].charset.as_deref(), Some("utf-8"));
    }

    #[test]
    fn parse_module_forward_lookup() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nforward lookup = no\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(!result.modules[0].forward_lookup);
    }

    #[test]
    fn parse_module_forward_lookup_default_true() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].forward_lookup);
    }

    // --- Strict modes directive tests ---

    #[test]
    fn parse_module_strict_modes_yes() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nstrict modes = yes\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].strict_modes);
    }

    #[test]
    fn parse_module_strict_modes_no() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nstrict modes = no\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(!result.modules[0].strict_modes);
    }

    #[test]
    fn parse_module_strict_modes_default_true() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].strict_modes);
    }

    #[test]
    fn parse_module_strict_modes_false_value() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nstrict modes = false\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(!result.modules[0].strict_modes);
    }

    #[test]
    fn parse_module_strict_modes_invalid_boolean() {
        let file = write_config("[mod]\npath = /tmp\nstrict modes = maybe\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("invalid boolean"));
    }

    #[test]
    fn parse_module_strict_modes_duplicate() {
        let file = write_config(
            "[mod]\npath = /tmp\nstrict modes = yes\nstrict modes = no\n",
        );
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("duplicate"));
    }

    // --- Open noatime directive tests ---

    #[test]
    fn parse_module_open_noatime_yes() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nopen noatime = yes\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].open_noatime);
    }

    #[test]
    fn parse_module_open_noatime_no() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nopen noatime = no\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(!result.modules[0].open_noatime);
    }

    #[test]
    fn parse_module_open_noatime_default_false() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(!result.modules[0].open_noatime);
    }

    #[test]
    fn parse_module_open_noatime_invalid_boolean() {
        let file = write_config("[mod]\npath = /tmp\nopen noatime = maybe\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("invalid boolean"));
    }

    #[test]
    fn parse_module_open_noatime_duplicate() {
        let file = write_config(
            "[mod]\npath = /tmp\nopen noatime = yes\nopen noatime = no\n",
        );
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn parse_unknown_per_module_directive_continues() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nunknown directive = value\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds with warning");
        assert_eq!(result.modules.len(), 1);
        assert_eq!(result.modules[0].name, "mod");
    }

    #[test]
    fn parse_module_all_new_directives() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\n\
             path = {}\n\
             max verbosity = 5\n\
             ignore errors = yes\n\
             ignore nonreadable = true\n\
             transfer logging = yes\n\
             log format = %o %f\n\
             dont compress = *.gz *.bz2\n\
             early exec = /bin/early\n\
             pre-xfer exec = /bin/pre\n\
             post-xfer exec = /bin/post\n\
             name converter = /bin/nameconv\n\
             temp dir = /tmp/staging\n\
             charset = utf-8\n\
             forward lookup = no\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let module = &result.modules[0];
        assert_eq!(module.max_verbosity, 5);
        assert!(module.ignore_errors);
        assert!(module.ignore_nonreadable);
        assert!(module.transfer_logging);
        assert_eq!(module.log_format.as_deref(), Some("%o %f"));
        assert_eq!(module.dont_compress.as_deref(), Some("*.gz *.bz2"));
        assert_eq!(module.early_exec.as_deref(), Some("/bin/early"));
        assert_eq!(module.pre_xfer_exec.as_deref(), Some("/bin/pre"));
        assert_eq!(module.post_xfer_exec.as_deref(), Some("/bin/post"));
        assert_eq!(module.name_converter.as_deref(), Some("/bin/nameconv"));
        assert_eq!(module.temp_dir.as_deref(), Some("/tmp/staging"));
        assert_eq!(module.charset.as_deref(), Some("utf-8"));
        assert!(!module.forward_lookup);
    }

    // --- Config file not found ---

    #[test]
    fn parse_nonexistent_config() {
        let err = parse_config_modules(Path::new("/nonexistent/config.conf"))
            .expect_err("should fail");
        assert!(err.to_string().contains("failed to"));
    }

    // --- Global vs module directive ordering tests ---

    #[test]
    fn global_directives_before_modules_apply_as_defaults() {
        let dir = TempDir::new().expect("create temp dir");
        let path1 = dir.path().join("alpha");
        let path2 = dir.path().join("beta");
        fs::create_dir(&path1).expect("create alpha dir");
        fs::create_dir(&path2).expect("create beta dir");

        let config = format!(
            "incoming chmod = Dg+s,ug+w\n\
             outgoing chmod = Fo-w,+X\n\
             \n\
             [alpha]\n\
             path = {}\n\
             \n\
             [beta]\n\
             path = {}\n",
            path1.display(),
            path2.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 2);

        // Both modules inherit global chmod defaults
        assert_eq!(
            result.modules[0].incoming_chmod.as_deref(),
            Some("Dg+s,ug+w")
        );
        assert_eq!(
            result.modules[0].outgoing_chmod.as_deref(),
            Some("Fo-w,+X")
        );
        assert_eq!(
            result.modules[1].incoming_chmod.as_deref(),
            Some("Dg+s,ug+w")
        );
        assert_eq!(
            result.modules[1].outgoing_chmod.as_deref(),
            Some("Fo-w,+X")
        );
    }

    #[test]
    fn module_directives_override_global_defaults() {
        let dir = TempDir::new().expect("create temp dir");
        let path1 = dir.path().join("inherited");
        let path2 = dir.path().join("overridden");
        fs::create_dir(&path1).expect("create inherited dir");
        fs::create_dir(&path2).expect("create overridden dir");

        let config = format!(
            "incoming chmod = global-in\n\
             outgoing chmod = global-out\n\
             \n\
             [inherited]\n\
             path = {}\n\
             \n\
             [overridden]\n\
             path = {}\n\
             incoming chmod = module-in\n\
             outgoing chmod = module-out\n",
            path1.display(),
            path2.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 2);

        // `inherited` gets global defaults
        let inherited = &result.modules[0];
        assert_eq!(inherited.name, "inherited");
        assert_eq!(inherited.incoming_chmod.as_deref(), Some("global-in"));
        assert_eq!(inherited.outgoing_chmod.as_deref(), Some("global-out"));

        // `overridden` uses its own module-level values
        let overridden = &result.modules[1];
        assert_eq!(overridden.name, "overridden");
        assert_eq!(overridden.incoming_chmod.as_deref(), Some("module-in"));
        assert_eq!(overridden.outgoing_chmod.as_deref(), Some("module-out"));
    }

    #[test]
    fn directive_in_one_module_does_not_affect_another() {
        let dir = TempDir::new().expect("create temp dir");
        let path_a = dir.path().join("mod_a");
        let path_b = dir.path().join("mod_b");
        fs::create_dir(&path_a).expect("create mod_a dir");
        fs::create_dir(&path_b).expect("create mod_b dir");

        let config = format!(
            "[mod_a]\n\
             path = {}\n\
             read only = no\n\
             use chroot = no\n\
             numeric ids = yes\n\
             fake super = yes\n\
             timeout = 300\n\
             max verbosity = 5\n\
             ignore errors = yes\n\
             transfer logging = yes\n\
             \n\
             [mod_b]\n\
             path = {}\n",
            path_a.display(),
            path_b.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 2);

        let mod_a = &result.modules[0];
        assert!(!mod_a.read_only);
        assert!(!mod_a.use_chroot);
        assert!(mod_a.numeric_ids);
        assert!(mod_a.fake_super);
        assert_eq!(mod_a.timeout.map(|t| t.get()), Some(300));
        assert_eq!(mod_a.max_verbosity, 5);
        assert!(mod_a.ignore_errors);
        assert!(mod_a.transfer_logging);

        // mod_b retains all defaults, unaffected by mod_a settings
        let mod_b = &result.modules[1];
        assert!(mod_b.read_only); // default true
        assert!(mod_b.use_chroot); // default true
        assert!(!mod_b.numeric_ids); // default false
        assert!(!mod_b.fake_super); // default false
        assert!(mod_b.timeout.is_none()); // default None
        assert_eq!(mod_b.max_verbosity, 1); // default 1
        assert!(!mod_b.ignore_errors); // default false
        assert!(!mod_b.transfer_logging); // default false
    }

    #[test]
    fn global_only_directive_inside_module_is_unknown() {
        // "pid file" is a global-only directive. Inside a module section,
        // it becomes an unknown per-module directive (warned and ignored).
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\n\
             path = {}\n\
             pid file = /var/run/test.pid\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        // The "pid file" directive should NOT be parsed as a global setting
        assert!(result.pid_file.is_none());
        assert_eq!(result.modules.len(), 1);
    }

    #[test]
    fn global_refuse_options_inherited_by_modules_without_own_refuse() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "refuse options = delete, compress\n\
             \n\
             [mod]\n\
             path = {}\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        // Global refuse options should be stored separately
        assert_eq!(result.global_refuse_options.len(), 1);
        assert_eq!(
            result.global_refuse_options[0].0,
            vec!["delete", "compress"]
        );

        // Module should have empty refuse options (inheritance happens at RuntimeOptions level)
        assert!(result.modules[0].refuse_options.is_empty());
    }

    #[test]
    fn module_with_own_refuse_options_separate_from_global() {
        let dir = TempDir::new().expect("create temp dir");
        let path1 = dir.path().join("inheritor");
        let path2 = dir.path().join("overrider");
        fs::create_dir(&path1).expect("create dir 1");
        fs::create_dir(&path2).expect("create dir 2");

        let config = format!(
            "refuse options = delete\n\
             \n\
             [inheritor]\n\
             path = {}\n\
             \n\
             [overrider]\n\
             path = {}\n\
             refuse options = hardlinks\n",
            path1.display(),
            path2.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.global_refuse_options.len(), 1);
        assert_eq!(result.global_refuse_options[0].0, vec!["delete"]);

        // Inheritor has no own refuse options
        assert!(result.modules[0].refuse_options.is_empty());

        // Overrider has its own refuse options
        assert_eq!(result.modules[1].refuse_options, vec!["hardlinks"]);
    }

    #[test]
    fn global_bwlimit_stored_in_result() {
        let file = write_config(
            "bwlimit = 1000\n\
             [mod]\npath = /tmp\n",
        );
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert!(result.global_bandwidth_limit.is_some());
        assert_eq!(result.modules.len(), 1);
    }

    #[test]
    fn reverse_lookup_before_module_is_global() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "reverse lookup = no\n\
             \n\
             [mod]\n\
             path = {}\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        let (value, _) = result.reverse_lookup.expect("reverse lookup set");
        assert!(!value);
    }

    #[test]
    fn multiple_modules_with_independent_boolean_settings() {
        let dir = TempDir::new().expect("create temp dir");
        let path_a = dir.path().join("a");
        let path_b = dir.path().join("b");
        let path_c = dir.path().join("c");
        fs::create_dir(&path_a).expect("create dir a");
        fs::create_dir(&path_b).expect("create dir b");
        fs::create_dir(&path_c).expect("create dir c");

        let config = format!(
            "[a]\n\
             path = {}\n\
             read only = no\n\
             \n\
             [b]\n\
             path = {}\n\
             write only = yes\n\
             \n\
             [c]\n\
             path = {}\n\
             list = no\n",
            path_a.display(),
            path_b.display(),
            path_c.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 3);

        // Module [a] overrides read_only; others keep defaults
        assert!(!result.modules[0].read_only);
        assert!(!result.modules[0].write_only); // default
        assert!(result.modules[0].listable); // default

        // Module [b] overrides write_only; others keep defaults
        assert!(result.modules[1].read_only); // default
        assert!(result.modules[1].write_only);
        assert!(result.modules[1].listable); // default

        // Module [c] overrides list; others keep defaults
        assert!(result.modules[2].read_only); // default
        assert!(!result.modules[2].write_only); // default
        assert!(!result.modules[2].listable);
    }

    #[test]
    fn global_use_chroot_false_applies_to_all_modules() {
        let dir = TempDir::new().expect("create temp dir");
        let path1 = dir.path().join("mod1");
        let path2 = dir.path().join("mod2");
        fs::create_dir(&path1).expect("create dir 1");
        fs::create_dir(&path2).expect("create dir 2");

        let config = format!(
            "use chroot = false\n\
             \n\
             [mod1]\n\
             path = {}\n\
             \n\
             [mod2]\n\
             path = {}\n",
            path1.display(),
            path2.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 2);
        assert!(!result.modules[0].use_chroot);
        assert!(!result.modules[1].use_chroot);
    }

    #[test]
    fn global_use_chroot_false_can_be_overridden_per_module() {
        let dir = TempDir::new().expect("create temp dir");
        let path1 = dir.path().join("inherits");
        let path2 = dir.path().join("overrides");
        fs::create_dir(&path1).expect("create dir 1");
        fs::create_dir(&path2).expect("create dir 2");

        let config = format!(
            "use chroot = false\n\
             \n\
             [inherits]\n\
             path = {}\n\
             \n\
             [overrides]\n\
             path = {}\n\
             use chroot = yes\n",
            path1.display(),
            path2.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 2);
        assert!(!result.modules[0].use_chroot); // inherits global false
        assert!(result.modules[1].use_chroot);  // overridden to true per-module
    }

    #[test]
    fn global_use_chroot_default_true_when_absent() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        // No global 'use chroot' directive — default is true
        assert!(result.modules[0].use_chroot);
    }

    // --- exclude from / include from tests ---

    #[test]
    fn exclude_from_and_include_from_default_to_none() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 1);
        assert!(result.modules[0].exclude_from.is_none());
        assert!(result.modules[0].include_from.is_none());
    }

    #[test]
    fn exclude_from_set_to_absolute_path() {
        let dir = TempDir::new().expect("create temp dir");
        let module_path = dir.path().join("data");
        fs::create_dir(&module_path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nexclude from = /etc/rsync/excludes.txt\n",
            module_path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 1);
        assert_eq!(
            result.modules[0].exclude_from,
            Some(PathBuf::from("/etc/rsync/excludes.txt"))
        );
    }

    #[test]
    fn include_from_set_to_absolute_path() {
        let dir = TempDir::new().expect("create temp dir");
        let module_path = dir.path().join("data");
        fs::create_dir(&module_path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\ninclude from = /etc/rsync/includes.txt\n",
            module_path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 1);
        assert_eq!(
            result.modules[0].include_from,
            Some(PathBuf::from("/etc/rsync/includes.txt"))
        );
    }

    #[test]
    fn exclude_from_relative_path_resolved_against_config_dir() {
        let dir = TempDir::new().expect("create temp dir");
        let module_path = dir.path().join("data");
        fs::create_dir(&module_path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nexclude from = excludes.txt\n",
            module_path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 1);
        let exclude_from = result.modules[0].exclude_from.as_ref().expect("exclude_from set");
        let config_dir = file.path().canonicalize().unwrap();
        let expected = config_dir.parent().unwrap().join("excludes.txt");
        assert_eq!(*exclude_from, expected);
    }

    #[test]
    fn include_from_relative_path_resolved_against_config_dir() {
        let dir = TempDir::new().expect("create temp dir");
        let module_path = dir.path().join("data");
        fs::create_dir(&module_path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\ninclude from = includes.txt\n",
            module_path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 1);
        let include_from = result.modules[0].include_from.as_ref().expect("include_from set");
        let config_dir = file.path().canonicalize().unwrap();
        let expected = config_dir.parent().unwrap().join("includes.txt");
        assert_eq!(*include_from, expected);
    }

    #[test]
    fn exclude_from_and_include_from_both_set() {
        let dir = TempDir::new().expect("create temp dir");
        let module_path = dir.path().join("data");
        fs::create_dir(&module_path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nexclude from = /etc/excludes.txt\ninclude from = /etc/includes.txt\n",
            module_path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 1);
        assert_eq!(
            result.modules[0].exclude_from,
            Some(PathBuf::from("/etc/excludes.txt"))
        );
        assert_eq!(
            result.modules[0].include_from,
            Some(PathBuf::from("/etc/includes.txt"))
        );
    }

    #[test]
    fn exclude_from_empty_value_rejected() {
        let dir = TempDir::new().expect("create temp dir");
        let module_path = dir.path().join("data");
        fs::create_dir(&module_path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nexclude from =\n",
            module_path.display()
        );
        let file = write_config(&config);
        let err = parse_config_modules(file.path()).expect_err("should fail on empty");
        assert!(err.to_string().contains("exclude from"));
    }

    #[test]
    fn include_from_empty_value_rejected() {
        let dir = TempDir::new().expect("create temp dir");
        let module_path = dir.path().join("data");
        fs::create_dir(&module_path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\ninclude from =\n",
            module_path.display()
        );
        let file = write_config(&config);
        let err = parse_config_modules(file.path()).expect_err("should fail on empty");
        assert!(err.to_string().contains("include from"));
    }

    #[test]
    fn exclude_from_duplicate_rejected() {
        let dir = TempDir::new().expect("create temp dir");
        let module_path = dir.path().join("data");
        fs::create_dir(&module_path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nexclude from = /a.txt\nexclude from = /b.txt\n",
            module_path.display()
        );
        let file = write_config(&config);
        let err = parse_config_modules(file.path()).expect_err("should fail on duplicate");
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn include_from_duplicate_rejected() {
        let dir = TempDir::new().expect("create temp dir");
        let module_path = dir.path().join("data");
        fs::create_dir(&module_path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\ninclude from = /a.txt\ninclude from = /b.txt\n",
            module_path.display()
        );
        let file = write_config(&config);
        let err = parse_config_modules(file.path()).expect_err("should fail on duplicate");
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn exclude_from_per_module_isolation() {
        let dir = TempDir::new().expect("create temp dir");
        let path1 = dir.path().join("data1");
        let path2 = dir.path().join("data2");
        fs::create_dir(&path1).expect("create dir 1");
        fs::create_dir(&path2).expect("create dir 2");

        let config = format!(
            "[mod1]\npath = {}\nexclude from = /etc/mod1_excludes.txt\n\
             [mod2]\npath = {}\n",
            path1.display(),
            path2.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 2);
        assert_eq!(
            result.modules[0].exclude_from,
            Some(PathBuf::from("/etc/mod1_excludes.txt"))
        );
        assert!(result.modules[1].exclude_from.is_none());
    }

    // --- Syslog facility directive tests ---

    #[test]
    fn parse_syslog_facility_global_directive() {
        let file = write_config("syslog facility = local5\n[mod]\npath = /tmp\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (facility, _origin) = result.syslog_facility.expect("should have syslog facility");
        assert_eq!(facility, "local5");
    }

    #[test]
    fn parse_syslog_facility_default_when_absent() {
        let file = write_config("[mod]\npath = /tmp\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.syslog_facility.is_none());
    }

    #[test]
    fn parse_syslog_facility_empty_rejected() {
        let file = write_config("syslog facility =\n[mod]\npath = /tmp\n");
        let err = parse_config_modules(file.path()).expect_err("should fail on empty");
        assert!(err.to_string().contains("syslog facility"));
    }

    #[test]
    fn parse_syslog_facility_duplicate_same_value_accepted() {
        let file = write_config(
            "syslog facility = daemon\nsyslog facility = daemon\n[mod]\npath = /tmp\n",
        );
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (facility, _) = result.syslog_facility.expect("should have facility");
        assert_eq!(facility, "daemon");
    }

    #[test]
    fn parse_syslog_facility_duplicate_different_value_rejected() {
        let file = write_config(
            "syslog facility = daemon\nsyslog facility = local0\n[mod]\npath = /tmp\n",
        );
        let err = parse_config_modules(file.path()).expect_err("should fail on duplicate");
        assert!(err.to_string().contains("duplicate"));
        assert!(err.to_string().contains("syslog facility"));
    }

    #[test]
    fn parse_syslog_facility_inside_module_is_unknown() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nsyslog facility = local0\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.syslog_facility.is_none());
    }

    // --- Syslog tag directive tests ---

    #[test]
    fn parse_syslog_tag_global_directive() {
        let file = write_config("syslog tag = mybackup\n[mod]\npath = /tmp\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (tag, _origin) = result.syslog_tag.expect("should have syslog tag");
        assert_eq!(tag, "mybackup");
    }

    #[test]
    fn parse_syslog_tag_default_when_absent() {
        let file = write_config("[mod]\npath = /tmp\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.syslog_tag.is_none());
    }

    #[test]
    fn parse_syslog_tag_empty_rejected() {
        let file = write_config("syslog tag =\n[mod]\npath = /tmp\n");
        let err = parse_config_modules(file.path()).expect_err("should fail on empty");
        assert!(err.to_string().contains("syslog tag"));
    }

    #[test]
    fn parse_syslog_tag_duplicate_same_value_accepted() {
        let file =
            write_config("syslog tag = rsyncd\nsyslog tag = rsyncd\n[mod]\npath = /tmp\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (tag, _) = result.syslog_tag.expect("should have tag");
        assert_eq!(tag, "rsyncd");
    }

    #[test]
    fn parse_syslog_tag_duplicate_different_value_rejected() {
        let file =
            write_config("syslog tag = rsyncd\nsyslog tag = custom\n[mod]\npath = /tmp\n");
        let err = parse_config_modules(file.path()).expect_err("should fail on duplicate");
        assert!(err.to_string().contains("duplicate"));
        assert!(err.to_string().contains("syslog tag"));
    }

    #[test]
    fn parse_syslog_tag_inside_module_is_unknown() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nsyslog tag = custom\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.syslog_tag.is_none());
    }

    #[test]
    fn parse_both_syslog_directives_global() {
        let file = write_config(
            "syslog facility = local3\nsyslog tag = backup-daemon\n[mod]\npath = /tmp\n",
        );
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (facility, _) = result.syslog_facility.expect("should have facility");
        let (tag, _) = result.syslog_tag.expect("should have tag");
        assert_eq!(facility, "local3");
        assert_eq!(tag, "backup-daemon");
    }

    // --- address directive tests ---

    #[test]
    fn parse_address_ipv4() {
        let file = write_config("address = 192.168.1.100\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (addr, _) = result.bind_address.expect("should have bind_address");
        assert_eq!(addr, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)));
    }

    #[test]
    fn parse_address_ipv6() {
        let file = write_config("address = ::1\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (addr, _) = result.bind_address.expect("should have bind_address");
        assert_eq!(addr, IpAddr::V6(Ipv6Addr::LOCALHOST));
    }

    #[test]
    fn parse_address_absent() {
        let file = write_config("# no address\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.bind_address.is_none());
    }

    #[test]
    fn parse_address_empty_value() {
        let file = write_config("address =\n");
        let err = parse_config_modules(file.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("'address' directive must not be empty"), "{msg}");
    }

    #[test]
    fn parse_address_invalid() {
        let file = write_config("address = not-an-ip\n");
        let err = parse_config_modules(file.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid bind address"), "{msg}");
    }

    #[test]
    fn parse_address_duplicate_same_value() {
        let file = write_config("address = 10.0.0.1\naddress = 10.0.0.1\n");
        let result = parse_config_modules(file.path()).expect("identical duplicates accepted");
        let (addr, _) = result.bind_address.expect("should have bind_address");
        assert_eq!(addr, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
    }

    #[test]
    fn parse_address_duplicate_different_value() {
        let file = write_config("address = 10.0.0.1\naddress = 10.0.0.2\n");
        let err = parse_config_modules(file.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("duplicate 'address' directive"), "{msg}");
    }

    // --- socket options directive tests ---

    #[test]
    fn parse_socket_options_single_option() {
        let file = write_config("socket options = TCP_NODELAY\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (opts, _) = result.socket_options.expect("should have socket_options");
        assert_eq!(opts, "TCP_NODELAY");
    }

    #[test]
    fn parse_socket_options_multiple_options() {
        let file = write_config("socket options = TCP_NODELAY, SO_KEEPALIVE, SO_SNDBUF=65536\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (opts, _) = result.socket_options.expect("should have socket_options");
        assert_eq!(opts, "TCP_NODELAY, SO_KEEPALIVE, SO_SNDBUF=65536");
    }

    #[test]
    fn parse_socket_options_empty_value_rejected() {
        let file = write_config("socket options =\n");
        let err = parse_config_modules(file.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("'socket options' directive must not be empty"),
            "{msg}"
        );
    }

    #[test]
    fn parse_socket_options_duplicate_same_value_accepted() {
        let file =
            write_config("socket options = SO_KEEPALIVE\nsocket options = SO_KEEPALIVE\n");
        let result = parse_config_modules(file.path()).expect("identical duplicates accepted");
        let (opts, _) = result.socket_options.expect("should have socket_options");
        assert_eq!(opts, "SO_KEEPALIVE");
    }

    #[test]
    fn parse_socket_options_duplicate_different_value_rejected() {
        let file =
            write_config("socket options = SO_KEEPALIVE\nsocket options = TCP_NODELAY\n");
        let err = parse_config_modules(file.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("duplicate 'socket options' directive"), "{msg}");
    }

    #[test]
    fn parse_socket_options_not_set_when_absent() {
        let file = write_config("[mod]\npath = /tmp\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.socket_options.is_none());
    }

    #[test]
    fn parse_socket_options_inside_module_is_unknown() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nsocket options = TCP_NODELAY\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.socket_options.is_none());
    }

    #[test]
    fn parse_global_proxy_protocol_true() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "proxy protocol = yes\n[mod]\npath = {}\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (enabled, _) = result.proxy_protocol.expect("should have proxy_protocol");
        assert!(enabled);
    }

    #[test]
    fn parse_global_proxy_protocol_false() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "proxy protocol = no\n[mod]\npath = {}\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (enabled, _) = result.proxy_protocol.expect("should have proxy_protocol");
        assert!(!enabled);
    }

    #[test]
    fn parse_global_proxy_protocol_default_is_none() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.proxy_protocol.is_none());
    }

    #[test]
    fn parse_global_daemon_chroot() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("daemon chroot = /var/chroot\n[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (chroot_path, _) = result.daemon_chroot.expect("should have daemon_chroot");
        assert_eq!(chroot_path, PathBuf::from("/var/chroot"));
    }

    #[test]
    fn parse_global_daemon_chroot_default_is_none() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.daemon_chroot.is_none());
    }

    #[test]
    fn parse_global_daemon_chroot_empty_rejected() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("daemon chroot = \n[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path());
        assert!(result.is_err());
    }

    // --- filter / exclude / include tests ---

    #[test]
    fn parse_module_filter_single_rule() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nfilter = - *.tmp\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules[0].filter, vec!["- *.tmp"]);
    }

    #[test]
    fn parse_module_filter_multiple_rules_accumulate() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nfilter = - *.tmp\nfilter = + *.rs\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules[0].filter, vec!["- *.tmp", "+ *.rs"]);
    }

    #[test]
    fn parse_module_exclude_single_rule() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nexclude = *.log\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules[0].exclude, vec!["*.log"]);
    }

    #[test]
    fn parse_module_include_single_rule() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\ninclude = *.rs\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules[0].include, vec!["*.rs"]);
    }

    #[test]
    fn parse_module_exclude_multiple_rules_accumulate() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nexclude = *.log\nexclude = *.tmp\nexclude = /cache/\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(
            result.modules[0].exclude,
            vec!["*.log", "*.tmp", "/cache/"]
        );
    }

    #[test]
    fn parse_module_filter_empty_value_ignored() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nfilter =\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].filter.is_empty());
    }

    #[test]
    fn parse_module_exclude_empty_value_ignored() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nexclude =\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].exclude.is_empty());
    }

    #[test]
    fn parse_module_filter_default_empty() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].filter.is_empty());
        assert!(result.modules[0].exclude.is_empty());
        assert!(result.modules[0].include.is_empty());
    }

    #[test]
    fn parse_module_filter_per_module_isolation() {
        let dir = TempDir::new().expect("create temp dir");
        let p1 = dir.path().join("d1");
        let p2 = dir.path().join("d2");
        fs::create_dir(&p1).expect("create dir");
        fs::create_dir(&p2).expect("create dir");

        let config = format!(
            "[mod1]\npath = {}\nexclude = *.tmp\n\n[mod2]\npath = {}\n",
            p1.display(),
            p2.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules[0].exclude, vec!["*.tmp"]);
        assert!(result.modules[1].exclude.is_empty());
    }

    // --- rsync port tests ---

    #[test]
    fn parse_global_rsync_port() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("port = 8730\n[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).unwrap();
        assert_eq!(result.rsync_port.unwrap().0, 8730);
    }

    #[test]
    fn parse_global_rsync_port_alias() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("rsync port = 8730\n[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).unwrap();
        assert_eq!(result.rsync_port.unwrap().0, 8730);
    }

    #[test]
    fn parse_global_rsync_port_default() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).unwrap();
        assert!(result.rsync_port.is_none());
    }

    #[test]
    fn parse_global_rsync_port_invalid() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("port = abc\n[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path());
        assert!(result.is_err());
    }

    #[test]
    fn parse_global_rsync_port_duplicate_conflict() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "port = 8730\nport = 9999\n[mod]\npath = {}\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path());
        assert!(result.is_err());
    }

    // --- module log file tests ---

    #[test]
    fn parse_module_log_file_absolute() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nlog file = /var/log/rsync-mod.log\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).unwrap();
        assert_eq!(
            result.modules[0].log_file,
            Some(PathBuf::from("/var/log/rsync-mod.log"))
        );
    }

    #[test]
    fn parse_module_log_file_relative() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nlog file = rsync-mod.log\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).unwrap();
        let log_file = result.modules[0].log_file.as_ref().expect("log_file set");
        let config_dir = file.path().canonicalize().unwrap();
        let expected = config_dir.parent().unwrap().join("rsync-mod.log");
        assert_eq!(*log_file, expected);
    }

    #[test]
    fn parse_module_log_file_empty_rejected() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nlog file =\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path());
        assert!(result.is_err());
    }

    #[test]
    fn parse_module_log_file_duplicate_rejected() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nlog file = /a.log\nlog file = /b.log\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path());
        assert!(result.is_err());
    }

    #[test]
    fn parse_module_log_file_default_none() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).unwrap();
        assert!(result.modules[0].log_file.is_none());
    }
}
