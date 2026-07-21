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

    /// Wraps a Unix-style absolute path so it remains absolute on Windows.
    ///
    /// Windows treats paths like `/etc/foo` as relative because they lack a
    /// drive letter, which breaks tests that assert directives roundtrip as
    /// absolute. Prefixing with a drive on Windows preserves the intent
    /// without changing behaviour on Unix.
    fn abs(rest: &str) -> String {
        #[cfg(unix)]
        {
            rest.to_owned()
        }
        #[cfg(windows)]
        {
            format!("C:{rest}")
        }
    }


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
        assert_eq!(result.modules[0].numeric_ids, Some(true));
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
        assert_eq!(result.modules[0].gid, Some(GidSetting::List(vec![1000])));
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
    fn parse_amp_include_directive() {
        // upstream: params.c:parse_directives - `&include /path` (whitespace
        // separator, no '=') is the canonical syntax in rsync 3.4.3.
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let included = format!("[amp_included]\npath = {}\n", path.display());
        let include_file = write_config(&included);

        let main_config = format!("&include {}\n", include_file.path().display());
        let main_file = write_config(&main_config);

        let result = parse_config_modules(main_file.path()).expect("parse succeeds");
        assert_eq!(result.modules.len(), 1);
        assert_eq!(result.modules[0].name, "amp_included");
    }

    #[test]
    fn parse_amp_include_inherits_parent_globals_for_included_module() {
        // Regression: modules declared inside an `&include` target must
        // inherit the parent file's P_LOCAL defaults (use chroot, hosts
        // allow, read only, ...). Upstream's `loadparm.c::do_section`
        // shares the `Vars` block across the `]push`/`]pop` boundary so
        // the included file's sections finalize against the parent's
        // current globals. Without this inheritance the upstream
        // `daemon-config` testsuite case fails: `inc-mod` lands with
        // `use_chroot=true` (the rsync default) even though the parent
        // declared `use chroot = no`, which makes the daemon attempt
        // `chroot()` and corrupt the wire stream for non-root listens.
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let included = format!(
            "[inc-mod]\npath = {}\nread only = yes\ncomment = via-include\n",
            path.display(),
        );
        let include_file = write_config(&included);

        let main_config = format!(
            "use chroot = no\nhosts allow = localhost 127.0.0.0/8\n&include {}\n",
            include_file.path().display(),
        );
        let main_file = write_config(&main_config);

        let result = parse_config_modules(main_file.path()).expect("parse succeeds");
        let included_mod = result
            .modules
            .iter()
            .find(|m| m.name == "inc-mod")
            .expect("inc-mod present");
        assert!(
            !included_mod.use_chroot,
            "use chroot from parent must be inherited by included module",
        );
        assert_eq!(
            included_mod.hosts_allow.len(),
            2,
            "global hosts allow must propagate into included module",
        );
        assert!(included_mod.read_only, "module's own read only must be honoured");
    }

    #[test]
    fn parse_amp_include_module_overrides_inherited_default() {
        // Regression: per-module directives inside an `&include` target
        // must still win over the parent's inherited defaults so the
        // include is purely additive, never coercive. Mirrors upstream's
        // shared-Vars semantics where per-section settings shadow the
        // active defaults until the next `]pop` restores the snapshot.
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let included = format!(
            "[override-mod]\npath = {}\nuse chroot = yes\n",
            path.display(),
        );
        let include_file = write_config(&included);

        let main_config = format!(
            "use chroot = no\n&include {}\n",
            include_file.path().display(),
        );
        let main_file = write_config(&main_config);

        let result = parse_config_modules(main_file.path()).expect("parse succeeds");
        let module = result
            .modules
            .iter()
            .find(|m| m.name == "override-mod")
            .expect("override-mod present");
        assert!(
            module.use_chroot,
            "per-module use chroot must override inherited parent default",
        );
    }

    #[test]
    fn parse_amp_merge_directive_with_equals() {
        // upstream: params.c - `&merge` is the bare-include variant; the
        // optional '=' separator must also be accepted.
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let included = format!("[merge_included]\npath = {}\n", path.display());
        let include_file = write_config(&included);

        let main_config = format!("&merge = {}\n", include_file.path().display());
        let main_file = write_config(&main_config);

        let result = parse_config_modules(main_file.path()).expect("parse succeeds");
        assert_eq!(result.modules.len(), 1);
        assert_eq!(result.modules[0].name, "merge_included");
    }

    #[test]
    fn backslash_continuation_joins_directive_value() {
        // upstream: params.c:Continuation() - a line ending in a backslash is
        // joined with the next physical line into one logical directive. Split
        // across two lines, the `World` fragment on its own is not a valid
        // `key = value` line, so a config that failed to join would error;
        // proving the join is what makes this parse succeed as one directive.
        let file = write_config("motd = Hello \\\nWorld\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.motd_lines, vec!["Hello World"]);
    }

    #[test]
    fn backslash_continuation_not_applied_to_comment_line() {
        // upstream: params.c:Parse() - comment lines are consumed by
        // EatComment(), which never scans for the continuation character, so a
        // trailing backslash on a comment must not swallow the following
        // directive. Were it wrongly continued, the `motd` line would be eaten
        // and `motd_lines` would be empty.
        let file = write_config("# a trailing backslash comment \\\nmotd = Real\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.motd_lines, vec!["Real"]);
    }

    #[test]
    fn amp_include_directory_globs_conf_files_sorted() {
        // upstream: params.c:include_config() - an `&include` of a directory
        // pulls in every `*.conf` entry (never `*.inc`), processed in sorted
        // order.
        let dir = TempDir::new().expect("create temp dir");
        let data = dir.path().join("data");
        fs::create_dir(&data).expect("create data dir");

        let conf_dir = dir.path().join("conf.d");
        fs::create_dir(&conf_dir).expect("create conf dir");
        fs::write(
            conf_dir.join("b.conf"),
            format!("[mod_b]\npath = {}\n", data.display()),
        )
        .expect("write b.conf");
        fs::write(
            conf_dir.join("a.conf"),
            format!("[mod_a]\npath = {}\n", data.display()),
        )
        .expect("write a.conf");
        // A `*.inc` file and an unrelated file must both be ignored by
        // `&include`.
        fs::write(
            conf_dir.join("z.inc"),
            format!("[mod_z]\npath = {}\n", data.display()),
        )
        .expect("write z.inc");
        fs::write(conf_dir.join("notes.txt"), "ignored\n").expect("write notes.txt");

        let main_config = format!("&include {}\n", conf_dir.display());
        let main_file = write_config(&main_config);

        let result = parse_config_modules(main_file.path()).expect("parse succeeds");
        let names: Vec<&str> = result.modules.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["mod_a", "mod_b"]);
    }

    #[test]
    fn amp_merge_directory_globs_inc_files() {
        // upstream: params.c:include_config() - an `&merge` of a directory
        // pulls in every `*.inc` entry (never `*.conf`).
        let dir = TempDir::new().expect("create temp dir");
        let data = dir.path().join("data");
        fs::create_dir(&data).expect("create data dir");

        let inc_dir = dir.path().join("inc.d");
        fs::create_dir(&inc_dir).expect("create inc dir");
        fs::write(
            inc_dir.join("first.inc"),
            format!("[mod_inc]\npath = {}\n", data.display()),
        )
        .expect("write first.inc");
        fs::write(
            inc_dir.join("other.conf"),
            format!("[mod_conf]\npath = {}\n", data.display()),
        )
        .expect("write other.conf");

        let main_config = format!("&merge {}\n", inc_dir.display());
        let main_file = write_config(&main_config);

        let result = parse_config_modules(main_file.path()).expect("parse succeeds");
        let names: Vec<&str> = result.modules.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["mod_inc"]);
    }

    #[test]
    fn amp_include_global_directives_do_not_leak_to_parent() {
        // upstream: params.c:include_config() - `&include` wraps the parse in
        // `]push`/`]pop`, restoring the parent's global `Vars` afterwards, so a
        // global directive set only inside the included file must not surface in
        // the parent configuration.
        let included = write_config("bwlimit = 2000\n");
        let main_config = format!("&include {}\n", included.path().display());
        let main_file = write_config(&main_config);

        let result = parse_config_modules(main_file.path()).expect("parse succeeds");
        assert!(
            result.global_bandwidth_limit.is_none(),
            "&include must not leak the included file's globals into the parent",
        );
    }

    #[test]
    fn amp_merge_global_directives_leak_to_parent() {
        // upstream: params.c:include_config() - `&merge` shares the current
        // scope (no `]push`/`]pop`), so a global directive set inside the merged
        // file does take effect in the parent configuration.
        let included = write_config("bwlimit = 2000\n");
        let main_config = format!("&merge {}\n", included.path().display());
        let main_file = write_config(&main_config);

        let result = parse_config_modules(main_file.path()).expect("parse succeeds");
        assert!(
            result.global_bandwidth_limit.is_some(),
            "&merge must merge the included file's globals into the parent",
        );
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


    #[test]
    fn parse_invalid_boolean_keeps_default() {
        // upstream: loadparm.c:418-423 - a badly formed boolean warns and
        // retains the directive's default rather than aborting the load, so
        // `read only = maybe` leaves read_only at its default (true).
        let file = write_config("[mod]\npath = /tmp\nread only = maybe\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].read_only);
    }


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
    fn parse_module_munge_symlinks_invalid_boolean_keeps_default() {
        // upstream: loadparm.c:418-423 - a badly formed boolean warns and keeps
        // the default; munge symlinks is unset (None) unless explicitly set.
        let file = write_config("[mod]\npath = /tmp\nmunge symlinks = maybe\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].munge_symlinks.is_none());
    }

    #[test]
    fn parse_module_munge_symlinks_unset_tristate() {
        // upstream: loadparm.c:369 - munge symlinks is a P_BOOL3 param, so the
        // `unset` tri-state is accepted and leaves the setting at its default.
        let file = write_config("[mod]\npath = /tmp\nmunge symlinks = unset\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].munge_symlinks.is_none());
    }

    #[test]
    fn parse_module_munge_symlinks_duplicate() {
        let file = write_config(
            "[mod]\npath = /tmp\nmunge symlinks = yes\nmunge symlinks = no\n",
        );
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("duplicate"));
    }


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
    fn parse_module_max_verbosity_atoi_leniency() {
        // upstream: loadparm.c:431-433 - P_INTEGER uses atoi(), so "abc" yields
        // 0 and a trailing suffix like "5x" yields 5, never an error.
        let file = write_config("[mod]\npath = /tmp\nmax verbosity = abc\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules[0].max_verbosity, 0);

        let file = write_config("[mod]\npath = /tmp\nmax verbosity = 5x\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.modules[0].max_verbosity, 5);
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
    fn parse_module_strict_modes_invalid_boolean_keeps_default() {
        // upstream: loadparm.c:418-423 - a badly formed boolean warns and keeps
        // the default. `strict modes` defaults to True (daemon-parm.h:203), so
        // the malformed value must leave it enabled, not clear it.
        let file = write_config("[mod]\npath = /tmp\nstrict modes = maybe\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].strict_modes);
    }

    #[test]
    fn parse_module_strict_modes_duplicate() {
        let file = write_config(
            "[mod]\npath = /tmp\nstrict modes = yes\nstrict modes = no\n",
        );
        let err = parse_config_modules(file.path()).expect_err("should fail");
        assert!(err.to_string().contains("duplicate"));
    }


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
    fn parse_module_open_noatime_invalid_boolean_keeps_default() {
        // upstream: loadparm.c:418-423 - a badly formed boolean warns and keeps
        // the default (open noatime defaults to false).
        let file = write_config("[mod]\npath = /tmp\nopen noatime = maybe\n");
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(!result.modules[0].open_noatime);
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


    #[test]
    fn parse_nonexistent_config() {
        let err = parse_config_modules(Path::new("/nonexistent/config.conf"))
            .expect_err("should fail");
        assert!(err.to_string().contains("failed to"));
    }


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
        assert_eq!(mod_a.numeric_ids, Some(true));
        assert!(mod_a.fake_super);
        assert_eq!(mod_a.timeout.map(|t| t.get()), Some(300));
        assert_eq!(mod_a.max_verbosity, 5);
        assert!(mod_a.ignore_errors);
        assert!(mod_a.transfer_logging);

        // mod_b retains all defaults, unaffected by mod_a settings
        let mod_b = &result.modules[1];
        assert!(mod_b.read_only); // default true
        assert!(mod_b.use_chroot); // default true
        assert_eq!(mod_b.numeric_ids, None); // default unset (BOOL3)
        assert!(!mod_b.fake_super); // default false
        assert!(mod_b.timeout.is_none()); // default None
        assert_eq!(mod_b.max_verbosity, 1); // default 1
        assert!(!mod_b.ignore_errors); // default false
        assert!(!mod_b.transfer_logging); // default false
    }

    #[test]
    fn global_only_directive_inside_module_is_unknown() {
        // "pid file" is a P_GLOBAL directive. Inside a module section, upstream
        // reports it ("Global parameter ... found in module section!") and
        // ignores it, so it never becomes a daemon-wide or per-module setting.
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
        let config = format!(
            "bwlimit = 1000\n\
             [mod]\npath = {}\n",
            abs("/tmp"),
        );
        let file = write_config(&config);
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

    // upstream: reverse lookup is P_LOCAL (daemon-parm.h:78). A module value
    // overrides the global default for that module only; another module without
    // its own directive inherits the global default. Proving the override wins
    // guards against a regression back to treating it as a daemon-wide singleton.
    #[test]
    fn module_reverse_lookup_override_beats_global_and_sibling_inherits() {
        let dir = TempDir::new().expect("create temp dir");
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::create_dir(&a).expect("create a");
        fs::create_dir(&b).expect("create b");

        let config = format!(
            "reverse lookup = yes\n\
             [override]\n\
             path = {}\n\
             reverse lookup = no\n\
             [inherit]\n\
             path = {}\n",
            a.display(),
            b.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        let override_mod = result
            .modules
            .iter()
            .find(|m| m.name == "override")
            .expect("override module");
        let inherit_mod = result
            .modules
            .iter()
            .find(|m| m.name == "inherit")
            .expect("inherit module");
        assert!(!override_mod.reverse_lookup, "module override must win");
        assert!(inherit_mod.reverse_lookup, "sibling inherits global yes");
    }

    // upstream: loadparm.c init_section copies the global default into each
    // module, so a global `reverse lookup = no` becomes the module default.
    #[test]
    fn module_inherits_global_reverse_lookup_disabled() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "reverse lookup = no\n\
             [mod]\n\
             path = {}\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(!result.modules[0].reverse_lookup);
    }

    // upstream: daemon-parm.h:78 default True when neither global nor module set.
    #[test]
    fn module_reverse_lookup_defaults_true_when_unset() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(result.modules[0].reverse_lookup);
    }

    #[test]
    fn duplicate_module_reverse_lookup_errors() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nreverse lookup = yes\nreverse lookup = no\n",
            path.display()
        );
        let file = write_config(&config);
        let err = parse_config_modules(file.path()).expect_err("duplicate rejected");
        assert!(err.to_string().contains("reverse lookup"));
    }

    // upstream: lock file is P_LOCAL (daemon-parm.h:46). A module value overrides
    // the daemon-wide lock file for that module; an unset module inherits it.
    #[test]
    fn module_lock_file_override_parsed_and_sibling_inherits() {
        let dir = TempDir::new().expect("create temp dir");
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::create_dir(&a).expect("create a");
        fs::create_dir(&b).expect("create b");
        let lock = dir.path().join("mod-a.lock");

        let config = format!(
            "[locked]\n\
             path = {}\n\
             lock file = {}\n\
             [plain]\n\
             path = {}\n",
            a.display(),
            lock.display(),
            b.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        let locked = result
            .modules
            .iter()
            .find(|m| m.name == "locked")
            .expect("locked module");
        let plain = result
            .modules
            .iter()
            .find(|m| m.name == "plain")
            .expect("plain module");
        assert_eq!(locked.lock_file.as_deref(), Some(lock.as_path()));
        assert!(
            plain.lock_file.is_none(),
            "module without override inherits the daemon lock file at runtime"
        );
    }

    #[test]
    fn duplicate_module_lock_file_errors() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nlock file = /tmp/a.lock\nlock file = /tmp/b.lock\n",
            path.display()
        );
        let file = write_config(&config);
        let err = parse_config_modules(file.path()).expect_err("duplicate rejected");
        assert!(err.to_string().contains("lock file"));
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

        // No global 'use chroot' directive - default is true
        assert!(result.modules[0].use_chroot);
    }


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

        let excludes = abs("/etc/rsync/excludes.txt");
        let config = format!(
            "[mod]\npath = {}\nexclude from = {}\n",
            module_path.display(),
            excludes,
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 1);
        assert_eq!(
            result.modules[0].exclude_from,
            Some(PathBuf::from(excludes))
        );
    }

    #[test]
    fn include_from_set_to_absolute_path() {
        let dir = TempDir::new().expect("create temp dir");
        let module_path = dir.path().join("data");
        fs::create_dir(&module_path).expect("create dir");

        let includes = abs("/etc/rsync/includes.txt");
        let config = format!(
            "[mod]\npath = {}\ninclude from = {}\n",
            module_path.display(),
            includes,
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 1);
        assert_eq!(
            result.modules[0].include_from,
            Some(PathBuf::from(includes))
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

        let excludes = abs("/etc/excludes.txt");
        let includes = abs("/etc/includes.txt");
        let config = format!(
            "[mod]\npath = {}\nexclude from = {}\ninclude from = {}\n",
            module_path.display(),
            excludes,
            includes,
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 1);
        assert_eq!(
            result.modules[0].exclude_from,
            Some(PathBuf::from(excludes))
        );
        assert_eq!(
            result.modules[0].include_from,
            Some(PathBuf::from(includes))
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

        let excludes = abs("/etc/mod1_excludes.txt");
        let config = format!(
            "[mod1]\npath = {}\nexclude from = {}\n\
             [mod2]\npath = {}\n",
            path1.display(),
            excludes,
            path2.display(),
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 2);
        assert_eq!(
            result.modules[0].exclude_from,
            Some(PathBuf::from(excludes))
        );
        assert!(result.modules[1].exclude_from.is_none());
    }


    #[test]
    fn parse_syslog_facility_global_directive() {
        let file = write_config(&format!(
            "syslog facility = local5\n[mod]\npath = {}\n",
            abs("/tmp")
        ));
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (facility, _origin) = result.syslog_facility.expect("should have syslog facility");
        assert_eq!(facility, "local5");
    }

    #[test]
    fn parse_syslog_facility_default_when_absent() {
        let file = write_config(&format!("[mod]\npath = {}\n", abs("/tmp")));
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
        let file = write_config(&format!(
            "syslog facility = daemon\nsyslog facility = daemon\n[mod]\npath = {}\n",
            abs("/tmp")
        ));
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

    // upstream: `syslog facility` is P_LOCAL (loadparm.c). A value inside a
    // module section is a per-module override, not a global one: it must land on
    // the module (so its connection logs to that facility) and must not leak into
    // the global default. WHY: operators route per-module logs to distinct syslog
    // facilities; treating the directive as global-only silently dropped it.
    #[test]
    fn parse_syslog_facility_inside_module_overrides_that_module() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nsyslog facility = local0\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(
            result.syslog_facility.is_none(),
            "a module-scoped directive must not set the global facility"
        );
        let module = result
            .modules
            .iter()
            .find(|m| m.name == "mod")
            .expect("mod module");
        assert_eq!(module.syslog_facility.as_deref(), Some("local0"));
    }

    // upstream: loadparm.c:456 `case P_ENUM` leaves the inherited value unchanged
    // when the facility name matches no entry, so an unknown name is not a config
    // error - the module keeps inheriting the global/default facility. WHY: mirror
    // upstream's accept-and-continue instead of failing the daemon on a typo.
    #[test]
    fn parse_syslog_facility_unknown_name_inherits_silently() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nsyslog facility = bogus\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let module = result
            .modules
            .iter()
            .find(|m| m.name == "mod")
            .expect("mod module");
        assert!(
            module.syslog_facility.is_none(),
            "unknown facility name must inherit, not override"
        );
    }

    // upstream: `syslog tag`/`syslog facility` are P_LOCAL; a module override wins
    // for that module only while a sibling without its own directive inherits the
    // global-section value. WHY: guards against regressing to a daemon-wide
    // singleton that ignores per-module routing.
    #[test]
    fn module_syslog_override_beats_global_and_sibling_inherits() {
        let dir = TempDir::new().expect("create temp dir");
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::create_dir(&a).expect("create a");
        fs::create_dir(&b).expect("create b");

        let config = format!(
            "syslog tag = globaltag\n\
             syslog facility = local1\n\
             [override]\n\
             path = {}\n\
             syslog tag = overridetag\n\
             syslog facility = local5\n\
             [inherit]\n\
             path = {}\n",
            a.display(),
            b.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        let override_mod = result
            .modules
            .iter()
            .find(|m| m.name == "override")
            .expect("override module");
        let inherit_mod = result
            .modules
            .iter()
            .find(|m| m.name == "inherit")
            .expect("inherit module");

        assert_eq!(override_mod.syslog_tag.as_deref(), Some("overridetag"));
        assert_eq!(override_mod.syslog_facility.as_deref(), Some("local5"));
        assert_eq!(inherit_mod.syslog_tag.as_deref(), Some("globaltag"));
        assert_eq!(inherit_mod.syslog_facility.as_deref(), Some("local1"));
    }

    // A module without any syslog directive, under a config with no global
    // directive either, resolves to None (inherits the daemon-global logger).
    #[test]
    fn module_without_syslog_directive_inherits_default() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let file = write_config(&format!("[mod]\npath = {}\n", path.display()));
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let module = result
            .modules
            .iter()
            .find(|m| m.name == "mod")
            .expect("mod module");
        assert!(module.syslog_tag.is_none());
        assert!(module.syslog_facility.is_none());
    }

    #[test]
    fn parse_module_syslog_tag_empty_rejected() {
        let file = write_config("[mod]\npath = /tmp\nsyslog tag =\n");
        let err = parse_config_modules(file.path()).expect_err("should fail on empty");
        assert!(err.to_string().contains("syslog tag"));
    }

    #[test]
    fn parse_module_duplicate_syslog_facility_rejected() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nsyslog facility = local2\nsyslog facility = local3\n",
            path.display()
        );
        let file = write_config(&config);
        let err = parse_config_modules(file.path()).expect_err("should fail on duplicate");
        assert!(err.to_string().contains("syslog facility"));
    }


    #[test]
    fn parse_syslog_tag_global_directive() {
        let file = write_config(&format!(
            "syslog tag = mybackup\n[mod]\npath = {}\n",
            abs("/tmp")
        ));
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (tag, _origin) = result.syslog_tag.expect("should have syslog tag");
        assert_eq!(tag, "mybackup");
    }

    #[test]
    fn parse_syslog_tag_default_when_absent() {
        let file = write_config(&format!("[mod]\npath = {}\n", abs("/tmp")));
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
        let file = write_config(&format!(
            "syslog tag = rsyncd\nsyslog tag = rsyncd\n[mod]\npath = {}\n",
            abs("/tmp")
        ));
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
    // upstream: `syslog tag` is P_LOCAL (loadparm.c). A value inside a module
    // section overrides that module's tag and must not set the global default.
    // WHY: operators tag per-module logs distinctly; treating it as global-only
    // silently dropped the override.
    fn parse_syslog_tag_inside_module_overrides_that_module() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nsyslog tag = custom\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(
            result.syslog_tag.is_none(),
            "a module-scoped directive must not set the global tag"
        );
        let module = result
            .modules
            .iter()
            .find(|m| m.name == "mod")
            .expect("mod module");
        assert_eq!(module.syslog_tag.as_deref(), Some("custom"));
    }

    #[test]
    fn parse_both_syslog_directives_global() {
        let file = write_config(&format!(
            "syslog facility = local3\nsyslog tag = backup-daemon\n[mod]\npath = {}\n",
            abs("/tmp")
        ));
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        let (facility, _) = result.syslog_facility.expect("should have facility");
        let (tag, _) = result.syslog_tag.expect("should have tag");
        assert_eq!(facility, "local3");
        assert_eq!(tag, "backup-daemon");
    }


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
        let file = write_config(&format!("[mod]\npath = {}\n", abs("/tmp")));
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
    fn parse_global_rsync_port_atoi_leniency() {
        // upstream: loadparm.c:431-433 - `port` is P_INTEGER, parsed with
        // atoi(), so "abc" yields 0 (the default port) rather than an error.
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("port = abc\n[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(result.rsync_port.map(|(port, _)| port), Some(0));
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

    #[test]
    fn parse_global_acceptor_threads() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("acceptor threads = 4\n[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).unwrap();
        assert_eq!(result.acceptor_threads.unwrap().0.get(), 4);
    }

    #[test]
    fn parse_global_acceptor_threads_default_is_none() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).unwrap();
        assert!(result.acceptor_threads.is_none());
    }

    #[test]
    fn parse_global_acceptor_threads_invalid() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("acceptor threads = abc\n[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path());
        assert!(result.is_err());
    }

    #[test]
    fn parse_global_acceptor_threads_zero_rejected() {
        // Zero replicas would bind no listeners; the directive must reject it.
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("acceptor threads = 0\n[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path());
        assert!(result.is_err());
    }

    #[test]
    fn parse_global_acceptor_threads_duplicate_conflict() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "acceptor threads = 2\nacceptor threads = 4\n[mod]\npath = {}\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path());
        assert!(result.is_err());
    }

    #[test]
    fn parse_module_log_file_absolute() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let log_file_path = abs("/var/log/rsync-mod.log");
        let config = format!(
            "[mod]\npath = {}\nlog file = {log_file_path}\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).unwrap();
        assert_eq!(
            result.modules[0].log_file,
            Some(PathBuf::from(&log_file_path))
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

    /// Modules may legitimately serve the filesystem root.
    ///
    /// upstream: loadparm.c stores `path = /` verbatim (P_PATH only strips
    /// trailing slashes when `len > 1`, so the bare `/` is preserved), and
    /// clientserver.c serves it directly when `use chroot = no`. oc-rsync
    /// must accept the same form for wire-compatible daemon configs.
    #[test]
    #[cfg(unix)]
    fn parse_module_path_root_without_chroot() {
        let config = "[root]\npath = /\nuse chroot = no\n";
        let file = write_config(config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 1);
        assert_eq!(result.modules[0].name, "root");
        assert_eq!(result.modules[0].path, PathBuf::from("/"));
        assert!(!result.modules[0].use_chroot);
    }

    /// Upstream tolerates `path = /` with `use chroot = yes` as well: the
    /// resulting `chroot("/")` is a no-op rather than an error, and the parser
    /// must not reject the configuration on that basis.
    #[test]
    #[cfg(unix)]
    fn parse_module_path_root_with_chroot() {
        let config = "[root]\npath = /\nuse chroot = yes\n";
        let file = write_config(config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 1);
        assert_eq!(result.modules[0].path, PathBuf::from("/"));
        assert!(result.modules[0].use_chroot);
    }

    /// `&include` of a missing file must name the unresolved path. The error
    /// surfaces the include site (parent file + line) and the underlying I/O
    /// failure rather than the generic `expected 'key = value'` rejection.
    #[test]
    fn parse_amp_include_missing_file_reports_path() {
        let dir = TempDir::new().expect("create temp dir");
        let missing = dir.path().join("does_not_exist.conf");

        let main_config = format!("&include {}\n", missing.display());
        let main_file = write_config(&main_config);

        let err = parse_config_modules(main_file.path()).expect_err("should fail");
        let message = err.to_string();
        assert!(
            message.contains(&missing.display().to_string()),
            "error must name the missing include path: {message}"
        );
        assert!(
            message.contains("&include"),
            "error must name the &include directive: {message}"
        );
        assert!(
            !message.contains("expected 'key = value'"),
            "error must not fall back to the generic key=value rejection: {message}"
        );
    }

    /// Circular includes spelled with the upstream `&include` syntax must be
    /// detected and the offending file path must appear in the error.
    #[test]
    fn parse_amp_include_circular_detected() {
        let dir = TempDir::new().expect("create temp dir");
        let parent_path = dir.path().join("parent.conf");
        let child_path = dir.path().join("child.conf");

        let parent_content = format!("&include {}\n", child_path.display());
        let child_content = format!("&include {}\n", parent_path.display());
        fs::write(&parent_path, &parent_content).expect("write parent");
        fs::write(&child_path, &child_content).expect("write child");

        let err = parse_config_modules(&parent_path).expect_err("should fail");
        let message = err.to_string();
        assert!(
            message.contains("recursive include"),
            "error must flag the recursive include: {message}"
        );
        assert!(
            message.contains(&parent_path.display().to_string())
                || message.contains(
                    &parent_path
                        .canonicalize()
                        .unwrap_or_else(|_| parent_path.clone())
                        .display()
                        .to_string(),
                ),
            "error must name the offending file: {message}"
        );
    }

    /// An `&include` directive placed after a module section is opened must
    /// preserve declaration order: the parent module appears first, then any
    /// modules pulled in by the included file.
    #[test]
    fn parse_amp_include_preserves_module_order() {
        let dir = TempDir::new().expect("create temp dir");
        let parent_path = dir.path().join("parent");
        let child_path = dir.path().join("child");
        let inc_path = dir.path().join("inc.conf");
        fs::create_dir(&parent_path).expect("create parent dir");
        fs::create_dir(&child_path).expect("create child dir");

        let included = format!("[child_mod]\npath = {}\n", child_path.display());
        fs::write(&inc_path, &included).expect("write included");

        let main_config = format!(
            "[parent_mod]\npath = {}\n&include {}\n",
            parent_path.display(),
            inc_path.display(),
        );
        let main_file = write_config(&main_config);

        let result = parse_config_modules(main_file.path()).expect("parse succeeds");
        assert_eq!(result.modules.len(), 2);
        assert_eq!(result.modules[0].name, "parent_mod");
        assert_eq!(result.modules[1].name, "child_mod");
    }

    /// `&merge` shares the inclusion path with `&include`; missing-file errors
    /// must name both the `&merge` directive and the unresolved path.
    #[test]
    fn parse_amp_merge_missing_file_reports_path() {
        let dir = TempDir::new().expect("create temp dir");
        let missing = dir.path().join("absent.inc");

        let main_config = format!("&merge {}\n", missing.display());
        let main_file = write_config(&main_config);

        let err = parse_config_modules(main_file.path()).expect_err("should fail");
        let message = err.to_string();
        assert!(
            message.contains(&missing.display().to_string()),
            "error must name the missing merge path: {message}"
        );
        assert!(
            message.contains("&merge"),
            "error must name the &merge directive: {message}"
        );
    }

    /// An empty `&include` value reports the directive the user wrote so
    /// `&include\n` does not masquerade as a legacy `include =` failure.
    #[test]
    fn parse_amp_include_empty_value_names_directive() {
        let file = write_config("&include =\n");
        let err = parse_config_modules(file.path()).expect_err("should fail");
        let message = err.to_string();
        assert!(
            message.contains("'&include'") && message.contains("must not be empty"),
            "error must name the &include directive: {message}"
        );
    }

    /// Parameter-name matching must fold whitespace and case exactly like
    /// upstream `strwiEQ` (loadparm.c:282), so every spacing/case variant of a
    /// label collapses to the same canonical form. This is why `read only`,
    /// `readonly`, and `Read Only` all address the same parameter.
    #[test]
    fn normalize_param_name_folds_whitespace_and_case() {
        assert_eq!(normalize_param_name("read only"), "readonly");
        assert_eq!(normalize_param_name("readonly"), "readonly");
        assert_eq!(normalize_param_name("Read Only"), "readonly");
        assert_eq!(normalize_param_name("READONLY"), "readonly");
        assert_eq!(normalize_param_name("  read   only  "), "readonly");
        assert_eq!(normalize_param_name("read\tonly"), "readonly");
        // upstream isSpace()==C isspace() also folds the vertical tab.
        assert_eq!(normalize_param_name("read\u{0B}only"), "readonly");
    }

    /// `strwiEQ` skips only whitespace - never underscores or hyphens - and the
    /// generated labels already have underscores rewritten to spaces
    /// (daemon-parm.awk). An underscore is therefore significant: `read_only`
    /// stays distinct from the `read only` parameter and must NOT be folded away.
    #[test]
    fn normalize_param_name_preserves_underscore_and_hyphen() {
        assert_eq!(normalize_param_name("read_only"), "read_only");
        assert_eq!(normalize_param_name("Read_Only"), "read_only");
        assert_eq!(normalize_param_name("post-xfer exec"), "post-xferexec");
    }

    /// Whitespace- and case-variant spellings of a parameter name resolve to
    /// the same directive, matching upstream 3.4.4 which accepts `readonly`,
    /// `Read   Only`, and `read<TAB>only` as synonyms for `read only`.
    #[test]
    fn parse_module_read_only_whitespace_insensitive() {
        for spelling in ["readonly", "Read   Only", "read\tonly", "READ ONLY"] {
            let dir = TempDir::new().expect("create temp dir");
            let path = dir.path().join("data");
            fs::create_dir(&path).expect("create dir");

            let config = format!("[mod]\npath = {}\n{spelling} = no\n", path.display());
            let file = write_config(&config);
            let result = parse_config_modules(file.path()).expect("parse succeeds");
            assert!(
                !result.modules[0].read_only,
                "'{spelling}' must resolve to the read only parameter",
            );
        }
    }

    /// An underscore in a parameter name is significant. Upstream 3.4.4 logs
    /// `Unknown Parameter encountered: "read_only"` and ignores the line
    /// (loadparm.c:356), so `read_only` must NOT set the read only parameter,
    /// which retains its default of true.
    #[test]
    fn parse_module_read_only_underscore_is_not_a_synonym() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("[mod]\npath = {}\nread_only = no\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert!(
            result.modules[0].read_only,
            "underscore name must be ignored, leaving read only at its default",
        );
    }

    /// The whitespace-insensitive match is shared by every parameter, not just
    /// booleans: a multi-word string directive spelled without its space still
    /// resolves. `hostsallow` must populate the same list as `hosts allow`.
    #[test]
    fn parse_module_hosts_allow_whitespace_insensitive() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!(
            "[mod]\npath = {}\nHOSTS  ALLOW = 10.0.0.0/8 192.168.0.0/16\n",
            path.display()
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");
        assert_eq!(
            result.modules[0].hosts_allow.len(),
            2,
            "'HOSTS  ALLOW' must resolve to the hosts allow parameter",
        );
    }

    /// Regression guard for the config-parser consolidation: the daemon once
    /// carried a second, weaker `rsyncd.conf` parser whose name matching only
    /// lowercased the directive (never folding interior whitespace), so it
    /// rejected the collapsed spelling `maxconnections`. The surviving parser
    /// folds whitespace exactly like upstream `strwiEQ`, so every spacing
    /// variant of an integer directive - like the multi-word `max connections`
    /// - resolves to the same parameter and yields an identical value.
    #[test]
    fn parse_module_max_connections_whitespace_insensitive() {
        for spelling in ["max connections", "maxconnections", "Max  Connections"] {
            let dir = TempDir::new().expect("create temp dir");
            let path = dir.path().join("data");
            fs::create_dir(&path).expect("create dir");

            let config = format!("[mod]\npath = {}\n{spelling} = 10\n", path.display());
            let file = write_config(&config);
            let result = parse_config_modules(file.path()).expect("parse succeeds");
            assert_eq!(
                result.modules[0].max_connections.map(NonZeroU32::get),
                Some(10),
                "'{spelling}' must resolve to the max connections parameter",
            );
        }
    }

    // upstream: daemon-parm.txt classifies each parameter P_GLOBAL or P_LOCAL.
    // The `Globals:` block is valid only before the first module; everything in
    // the `Locals:` block may be set per-module. This test pins that boundary so
    // a parameter cannot silently drift across it: the daemon-wide params stay
    // global-only, while representative P_LOCAL params - including the ones oc
    // currently accepts only in the global section (reverse lookup, lock file,
    // syslog tag, syslog facility) - are never treated as global-only.
    #[test]
    fn global_only_classification_matches_upstream_parm_table() {
        for global in [
            "address",
            "daemon chroot",
            "daemon gid",
            "daemon uid",
            "motd file",
            "pid file",
            "socket options",
            "listen backlog",
            "port",
            "proxy protocol",
        ] {
            assert!(
                is_global_only_directive(global),
                "'{global}' is P_GLOBAL in upstream daemon-parm.txt and must be global-only",
            );
        }

        for local in [
            "path",
            "read only",
            "uid",
            "gid",
            "hosts allow",
            "reverse lookup",
            "lock file",
            "syslog tag",
            "syslog facility",
        ] {
            assert!(
                !is_global_only_directive(local),
                "'{local}' is P_LOCAL in upstream daemon-parm.txt and is valid per-module",
            );
        }
    }

    // upstream: loadparm.c:do_parameter - a P_LOCAL parameter set inside a module
    // overrides the global default for that module only. This is the semantic the
    // P_GLOBAL/P_LOCAL split protects: the value must reach the module, not be
    // rejected as a global-only directive.
    #[test]
    fn local_param_in_module_overrides_global_default() {
        let dir = TempDir::new().expect("create temp dir");
        let inherited = dir.path().join("inherited");
        let overridden = dir.path().join("overridden");
        fs::create_dir(&inherited).expect("create inherited dir");
        fs::create_dir(&overridden).expect("create overridden dir");

        let config = format!(
            "read only = yes\n\
             [inherited]\n\
             path = {}\n\
             [overridden]\n\
             path = {}\n\
             read only = no\n",
            inherited.display(),
            overridden.display(),
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules.len(), 2);
        assert!(
            result.modules[0].read_only,
            "module without an override inherits the global P_LOCAL default",
        );
        assert!(
            !result.modules[1].read_only,
            "module-level P_LOCAL directive overrides the global default",
        );
    }

    // WHY: upstream daemon-parm.txt marks `uid`/`gid` P_LOCAL, so a value in the
    // global section is the default `lp_uid(module_id)`/`lp_gid(module_id)` every
    // module inherits (clientserver.c:781,790). A root daemon only drops a module
    // to `nobody` when that per-module value is unset (`am_root ? NOBODY_USER`).
    // Regression guard: mapping the global `uid`/`gid` to the process-wide daemon
    // drop instead made a root daemon silently run every uid-less module as
    // nobody even under `uid = 0`, leaving nobody-owned files upstream keeps as
    // root. The default MUST reach the module so the drop resolves to root.
    #[test]
    fn global_uid_gid_seed_module_default() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("uid = 0\ngid = 0\n[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(
            result.modules[0].uid,
            Some(0),
            "module without an explicit uid inherits the global P_LOCAL uid default",
        );
        assert_eq!(
            result.modules[0].gid,
            Some(GidSetting::List(vec![0])),
            "module without an explicit gid inherits the global P_LOCAL gid default",
        );
        assert!(
            result.daemon_uid.is_none() && result.daemon_gid.is_none(),
            "bare `uid`/`gid` are P_LOCAL and must NOT arm the process-wide daemon drop",
        );
    }

    // WHY: upstream clientserver.c:781 - an explicit per-module `uid` overrides
    // the inherited global default, exactly like every other P_LOCAL parameter.
    #[test]
    fn module_uid_overrides_global_uid_default() {
        let dir = TempDir::new().expect("create temp dir");
        let inherited = dir.path().join("inherited");
        let overridden = dir.path().join("overridden");
        fs::create_dir(&inherited).expect("create inherited dir");
        fs::create_dir(&overridden).expect("create overridden dir");

        let config = format!(
            "uid = 0\n[inherited]\npath = {}\n[overridden]\npath = {}\nuid = 1000\n",
            inherited.display(),
            overridden.display(),
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(result.modules[0].uid, Some(0), "inherits global default");
        assert_eq!(result.modules[1].uid, Some(1000), "module override wins");
    }

    // WHY: upstream daemon-parm.txt marks `daemon_uid`/`daemon_gid` P_GLOBAL;
    // `daemon uid` sets the process-wide drop the listener applies once before
    // the accept loop (clientserver.c:1376), and must NOT leak into per-module
    // uid defaults. Keeps the two identity mechanisms cleanly separated.
    #[test]
    fn daemon_uid_directive_is_process_wide_not_module_default() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("data");
        fs::create_dir(&path).expect("create dir");

        let config = format!("daemon uid = 0\ndaemon gid = 0\n[mod]\npath = {}\n", path.display());
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        assert_eq!(
            result.modules[0].uid, None,
            "`daemon uid` is P_GLOBAL and must not seed the per-module uid default",
        );
        assert_eq!(result.modules[0].gid, None, "`daemon gid` must not seed module gid");
        assert!(
            result.daemon_uid.is_some(),
            "`daemon uid` must arm the process-wide daemon drop",
        );
    }

    /// A global `auth users` is a P_LOCAL default (daemon-parm.h:262): every
    /// module that omits its own `auth users` must inherit it and therefore
    /// require authentication (authenticate.c:228 auth_server reads
    /// lp_auth_users). Before this was wired, the global directive was silently
    /// dropped and such modules were left open - a fail-open security hole. A
    /// module that declares its own `auth users` still overrides the default.
    #[test]
    fn parse_global_auth_users_inherited_as_module_default() {
        let dir = TempDir::new().expect("create temp dir");
        let secrets = dir.path().join("rsyncd.secrets");
        fs::write(&secrets, "alice:pw\ncarol:pw\n").expect("write secrets");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&secrets, fs::Permissions::from_mode(0o600))
                .expect("chmod secrets");
        }

        let open = dir.path().join("open");
        let own = dir.path().join("own");
        fs::create_dir(&open).expect("create open dir");
        fs::create_dir(&own).expect("create own dir");

        let config = format!(
            "auth users = alice\nsecrets file = {secrets}\n\
             [inherits]\npath = {open}\n\
             [overrides]\npath = {own}\nauth users = carol\n",
            secrets = secrets.display(),
            open = open.display(),
            own = own.display(),
        );
        let file = write_config(&config);
        let result = parse_config_modules(file.path()).expect("parse succeeds");

        let inherits = result
            .modules
            .iter()
            .find(|m| m.name == "inherits")
            .expect("inherits module parsed");
        assert_eq!(
            inherits.auth_users.len(),
            1,
            "module without its own 'auth users' must inherit the global default and require auth",
        );
        assert_eq!(inherits.auth_users[0].username, "alice");
        assert!(
            inherits.secrets_file.is_some(),
            "inherited auth must also inherit the global secrets file",
        );

        let overrides = result
            .modules
            .iter()
            .find(|m| m.name == "overrides")
            .expect("overrides module parsed");
        assert_eq!(
            overrides.auth_users.len(),
            1,
            "module with its own 'auth users' keeps exactly its list",
        );
        assert_eq!(
            overrides.auth_users[0].username, "carol",
            "a module-level 'auth users' overrides the global default",
        );
    }
}
