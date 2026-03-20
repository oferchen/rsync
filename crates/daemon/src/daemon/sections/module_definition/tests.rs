// These tests use Unix-style paths like /data and /etc/secrets
#[cfg(all(test, unix))]
mod module_definition_builder_tests {
    use super::*;
    use std::path::PathBuf;

    fn test_config_path() -> PathBuf {
        PathBuf::from("/test/rsyncd.conf")
    }

    #[test]
    fn builder_new_sets_name_and_line() {
        let builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 42);
        assert_eq!(builder.name, "testmod");
        assert_eq!(builder.declaration_line, 42);
    }

    #[test]
    fn builder_new_starts_with_all_none() {
        let builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        assert!(builder.path.is_none());
        assert!(builder.comment.is_none());
        assert!(builder.hosts_allow.is_none());
        assert!(builder.hosts_deny.is_none());
        assert!(builder.auth_users.is_none());
        assert!(builder.secrets_file.is_none());
        assert!(builder.bandwidth_limit.is_none());
        assert!(builder.refuse_options.is_none());
        assert!(builder.read_only.is_none());
        assert!(builder.write_only.is_none());
        assert!(builder.numeric_ids.is_none());
        assert!(builder.uid.is_none());
        assert!(builder.gid.is_none());
        assert!(builder.timeout.is_none());
        assert!(builder.listable.is_none());
        assert!(builder.use_chroot.is_none());
        assert!(builder.max_connections.is_none());
        assert!(builder.incoming_chmod.is_none());
        assert!(builder.outgoing_chmod.is_none());
        assert!(builder.munge_symlinks.is_none());
        assert!(builder.max_verbosity.is_none());
        assert!(builder.ignore_errors.is_none());
        assert!(builder.ignore_nonreadable.is_none());
        assert!(builder.transfer_logging.is_none());
        assert!(builder.log_format.is_none());
        assert!(builder.dont_compress.is_none());
        assert!(builder.early_exec.is_none());
        assert!(builder.pre_xfer_exec.is_none());
        assert!(builder.post_xfer_exec.is_none());
        assert!(builder.name_converter.is_none());
        assert!(builder.temp_dir.is_none());
        assert!(builder.charset.is_none());
        assert!(builder.forward_lookup.is_none());
        assert!(builder.strict_modes.is_none());
        assert!(builder.exclude_from.is_none());
        assert!(builder.include_from.is_none());
        assert!(builder.open_noatime.is_none());
    }

    #[test]
    fn set_path_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let result = builder.set_path(PathBuf::from("/data"), &test_config_path(), 5);
        assert!(result.is_ok());
        assert_eq!(builder.path, Some(PathBuf::from("/data")));
    }

    #[test]
    fn set_path_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 5).unwrap();
        let result = builder.set_path(PathBuf::from("/other"), &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_comment_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let result = builder.set_comment(Some("A test module".to_owned()), &test_config_path(), 5);
        assert!(result.is_ok());
        assert_eq!(builder.comment, Some("A test module".to_owned()));
    }

    #[test]
    fn set_comment_allows_none() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let result = builder.set_comment(None, &test_config_path(), 5);
        assert!(result.is_ok());
        assert!(builder.comment.is_none());
    }

    #[test]
    fn set_comment_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_comment(Some("first".to_owned()), &test_config_path(), 5).unwrap();
        let result = builder.set_comment(Some("second".to_owned()), &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_hosts_allow_stores_patterns() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let patterns = vec![HostPattern::Any];
        let result = builder.set_hosts_allow(patterns.clone(), &test_config_path(), 5);
        assert!(result.is_ok());
        assert_eq!(builder.hosts_allow, Some(patterns));
    }

    #[test]
    fn set_hosts_allow_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_hosts_allow(vec![HostPattern::Any], &test_config_path(), 5).unwrap();
        let result = builder.set_hosts_allow(vec![], &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_hosts_deny_stores_patterns() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let patterns = vec![HostPattern::Any];
        let result = builder.set_hosts_deny(patterns.clone(), &test_config_path(), 5);
        assert!(result.is_ok());
        assert_eq!(builder.hosts_deny, Some(patterns));
    }

    #[test]
    fn set_hosts_deny_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_hosts_deny(vec![HostPattern::Any], &test_config_path(), 5).unwrap();
        let result = builder.set_hosts_deny(vec![], &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_auth_users_stores_users() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let users = vec![
            AuthUser::new("alice".to_owned()),
            AuthUser::new("bob".to_owned()),
        ];
        let result = builder.set_auth_users(users.clone(), &test_config_path(), 5);
        assert!(result.is_ok());
        assert_eq!(builder.auth_users, Some(users));
    }

    #[test]
    fn set_auth_users_rejects_empty() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let result = builder.set_auth_users(vec![], &test_config_path(), 5);
        assert!(result.is_err());
    }

    #[test]
    fn set_auth_users_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_auth_users(vec![AuthUser::new("alice".to_owned())], &test_config_path(), 5).unwrap();
        let result = builder.set_auth_users(vec![AuthUser::new("bob".to_owned())], &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_bandwidth_limit_stores_values() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let limit = NonZeroU64::new(1000);
        let burst = NonZeroU64::new(2000);
        let result = builder.set_bandwidth_limit(limit, burst, true, &test_config_path(), 5);
        assert!(result.is_ok());
        assert_eq!(builder.bandwidth_limit, limit);
        assert_eq!(builder.bandwidth_burst, burst);
        assert!(builder.bandwidth_burst_specified);
        assert!(builder.bandwidth_limit_specified);
        assert!(builder.bandwidth_limit_set);
    }

    #[test]
    fn set_bandwidth_limit_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_bandwidth_limit(None, None, false, &test_config_path(), 5).unwrap();
        let result = builder.set_bandwidth_limit(NonZeroU64::new(100), None, false, &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_refuse_options_stores_options() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let options = vec!["delete".to_owned(), "hardlinks".to_owned()];
        let result = builder.set_refuse_options(options.clone(), &test_config_path(), 5);
        assert!(result.is_ok());
        assert_eq!(builder.refuse_options, Some(options));
    }

    #[test]
    fn set_refuse_options_rejects_empty() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let result = builder.set_refuse_options(vec![], &test_config_path(), 5);
        assert!(result.is_err());
    }

    #[test]
    fn set_refuse_options_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_refuse_options(vec!["delete".to_owned()], &test_config_path(), 5).unwrap();
        let result = builder.set_refuse_options(vec!["hardlinks".to_owned()], &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_read_only_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_read_only(false, &test_config_path(), 5).unwrap();
        assert_eq!(builder.read_only, Some(false));
    }

    #[test]
    fn set_read_only_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_read_only(true, &test_config_path(), 5).unwrap();
        let result = builder.set_read_only(false, &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_write_only_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_write_only(true, &test_config_path(), 5).unwrap();
        assert_eq!(builder.write_only, Some(true));
    }

    #[test]
    fn set_write_only_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_write_only(true, &test_config_path(), 5).unwrap();
        let result = builder.set_write_only(false, &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_numeric_ids_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_numeric_ids(true, &test_config_path(), 5).unwrap();
        assert_eq!(builder.numeric_ids, Some(true));
    }

    #[test]
    fn set_numeric_ids_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_numeric_ids(true, &test_config_path(), 5).unwrap();
        let result = builder.set_numeric_ids(false, &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_listable_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_listable(false, &test_config_path(), 5).unwrap();
        assert_eq!(builder.listable, Some(false));
    }

    #[test]
    fn set_listable_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_listable(true, &test_config_path(), 5).unwrap();
        let result = builder.set_listable(false, &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_use_chroot_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_use_chroot(false, &test_config_path(), 5).unwrap();
        assert_eq!(builder.use_chroot, Some(false));
    }

    #[test]
    fn set_use_chroot_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_use_chroot(true, &test_config_path(), 5).unwrap();
        let result = builder.set_use_chroot(false, &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_uid_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_uid(1000, &test_config_path(), 5).unwrap();
        assert_eq!(builder.uid, Some(1000));
    }

    #[test]
    fn set_uid_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_uid(1000, &test_config_path(), 5).unwrap();
        let result = builder.set_uid(2000, &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_gid_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_gid(100, &test_config_path(), 5).unwrap();
        assert_eq!(builder.gid, Some(100));
    }

    #[test]
    fn set_gid_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_gid(100, &test_config_path(), 5).unwrap();
        let result = builder.set_gid(200, &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_timeout_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let timeout = NonZeroU64::new(60);
        builder.set_timeout(timeout, &test_config_path(), 5).unwrap();
        assert_eq!(builder.timeout, Some(timeout));
    }

    #[test]
    fn set_timeout_allows_none() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_timeout(None, &test_config_path(), 5).unwrap();
        assert_eq!(builder.timeout, Some(None));
    }

    #[test]
    fn set_timeout_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_timeout(NonZeroU64::new(60), &test_config_path(), 5).unwrap();
        let result = builder.set_timeout(NonZeroU64::new(120), &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_max_connections_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        let max = NonZeroU32::new(10);
        builder.set_max_connections(max, &test_config_path(), 5).unwrap();
        assert_eq!(builder.max_connections, Some(max));
    }

    #[test]
    fn set_max_connections_allows_none() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_max_connections(None, &test_config_path(), 5).unwrap();
        assert_eq!(builder.max_connections, Some(None));
    }

    #[test]
    fn set_max_connections_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_max_connections(NonZeroU32::new(10), &test_config_path(), 5).unwrap();
        let result = builder.set_max_connections(NonZeroU32::new(20), &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_incoming_chmod_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_incoming_chmod(Some("Dg+s,ug+w".to_owned()), &test_config_path(), 5).unwrap();
        assert_eq!(builder.incoming_chmod, Some(Some("Dg+s,ug+w".to_owned())));
    }

    #[test]
    fn set_incoming_chmod_allows_none() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_incoming_chmod(None, &test_config_path(), 5).unwrap();
        assert_eq!(builder.incoming_chmod, Some(None));
    }

    #[test]
    fn set_incoming_chmod_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_incoming_chmod(Some("a+r".to_owned()), &test_config_path(), 5).unwrap();
        let result = builder.set_incoming_chmod(Some("a+w".to_owned()), &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_outgoing_chmod_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_outgoing_chmod(Some("Fo-w,+X".to_owned()), &test_config_path(), 5).unwrap();
        assert_eq!(builder.outgoing_chmod, Some(Some("Fo-w,+X".to_owned())));
    }

    #[test]
    fn set_outgoing_chmod_allows_none() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_outgoing_chmod(None, &test_config_path(), 5).unwrap();
        assert_eq!(builder.outgoing_chmod, Some(None));
    }

    #[test]
    fn set_outgoing_chmod_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_outgoing_chmod(Some("a+r".to_owned()), &test_config_path(), 5).unwrap();
        let result = builder.set_outgoing_chmod(Some("a+w".to_owned()), &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_fake_super_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_fake_super(true, &test_config_path(), 5).unwrap();
        assert_eq!(builder.fake_super, Some(true));
    }

    #[test]
    fn set_fake_super_stores_false() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_fake_super(false, &test_config_path(), 5).unwrap();
        assert_eq!(builder.fake_super, Some(false));
    }

    #[test]
    fn set_fake_super_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_fake_super(true, &test_config_path(), 5).unwrap();
        let result = builder.set_fake_super(false, &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_munge_symlinks_stores_some_true() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_munge_symlinks(Some(true), &test_config_path(), 5).unwrap();
        assert_eq!(builder.munge_symlinks, Some(Some(true)));
    }

    #[test]
    fn set_munge_symlinks_stores_some_false() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_munge_symlinks(Some(false), &test_config_path(), 5).unwrap();
        assert_eq!(builder.munge_symlinks, Some(Some(false)));
    }

    #[test]
    fn set_munge_symlinks_stores_none() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_munge_symlinks(None, &test_config_path(), 5).unwrap();
        assert_eq!(builder.munge_symlinks, Some(None));
    }

    #[test]
    fn set_munge_symlinks_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_munge_symlinks(Some(true), &test_config_path(), 5).unwrap();
        let result = builder.set_munge_symlinks(Some(false), &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_strict_modes_stores_true() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_strict_modes(true, &test_config_path(), 5).unwrap();
        assert_eq!(builder.strict_modes, Some(true));
    }

    #[test]
    fn set_strict_modes_stores_false() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_strict_modes(false, &test_config_path(), 5).unwrap();
        assert_eq!(builder.strict_modes, Some(false));
    }

    #[test]
    fn set_strict_modes_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_strict_modes(true, &test_config_path(), 5).unwrap();
        let result = builder.set_strict_modes(false, &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_exclude_from_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_exclude_from(PathBuf::from("/etc/excludes.txt"), &test_config_path(), 5).unwrap();
        assert_eq!(builder.exclude_from, Some(PathBuf::from("/etc/excludes.txt")));
    }

    #[test]
    fn set_exclude_from_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_exclude_from(PathBuf::from("/etc/excludes.txt"), &test_config_path(), 5).unwrap();
        let result = builder.set_exclude_from(PathBuf::from("/etc/other.txt"), &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_include_from_stores_value() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_include_from(PathBuf::from("/etc/includes.txt"), &test_config_path(), 5).unwrap();
        assert_eq!(builder.include_from, Some(PathBuf::from("/etc/includes.txt")));
    }

    #[test]
    fn set_include_from_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_include_from(PathBuf::from("/etc/includes.txt"), &test_config_path(), 5).unwrap();
        let result = builder.set_include_from(PathBuf::from("/etc/other.txt"), &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn set_open_noatime_stores_true() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_open_noatime(true, &test_config_path(), 5).unwrap();
        assert_eq!(builder.open_noatime, Some(true));
    }

    #[test]
    fn set_open_noatime_stores_false() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_open_noatime(false, &test_config_path(), 5).unwrap();
        assert_eq!(builder.open_noatime, Some(false));
    }

    #[test]
    fn set_open_noatime_rejects_duplicate() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_open_noatime(true, &test_config_path(), 5).unwrap();
        let result = builder.set_open_noatime(false, &test_config_path(), 10);
        assert!(result.is_err());
    }

    #[test]
    fn finish_succeeds_with_minimal_config() {
        let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 2).unwrap();
        let result = builder.finish(&test_config_path(), None, None, None, None);
        assert!(result.is_ok());
        let def = result.unwrap();
        assert_eq!(def.name, "testmod");
        assert_eq!(def.path, PathBuf::from("/data"));
        assert!(def.read_only); // default
        assert!(!def.write_only); // default
        assert!(def.listable); // default
        assert!(def.use_chroot); // default
    }

    #[test]
    fn finish_fails_without_path() {
        let builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
        let result = builder.finish(&test_config_path(), None, None, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn finish_requires_absolute_path_with_chroot() {
        let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
        builder.set_path(PathBuf::from("relative/path"), &test_config_path(), 2).unwrap();
        // use_chroot defaults to true
        let result = builder.finish(&test_config_path(), None, None, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn finish_allows_relative_path_without_chroot() {
        let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
        builder.set_path(PathBuf::from("relative/path"), &test_config_path(), 2).unwrap();
        builder.set_use_chroot(false, &test_config_path(), 3).unwrap();
        let result = builder.finish(&test_config_path(), None, None, None, None);
        assert!(result.is_ok());
    }

    #[test]
    fn finish_applies_default_secrets_for_auth_users() {
        let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 2).unwrap();
        builder.set_auth_users(vec![AuthUser::new("alice".to_owned())], &test_config_path(), 3).unwrap();
        let default_secrets = PathBuf::from("/etc/secrets");
        let result = builder.finish(&test_config_path(), Some(&default_secrets), None, None, None);
        assert!(result.is_ok());
        let def = result.unwrap();
        assert_eq!(def.secrets_file, Some(PathBuf::from("/etc/secrets")));
    }

    #[test]
    fn finish_fails_auth_users_without_secrets() {
        let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 2).unwrap();
        builder.set_auth_users(vec![AuthUser::new("alice".to_owned())], &test_config_path(), 3).unwrap();
        let result = builder.finish(&test_config_path(), None, None, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn finish_applies_default_chmod_values() {
        let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 2).unwrap();
        let result = builder.finish(
            &test_config_path(),
            None,
            Some("Dg+s"),
            Some("Fo-w"),
            None,
        );
        assert!(result.is_ok());
        let def = result.unwrap();
        assert_eq!(def.incoming_chmod.as_deref(), Some("Dg+s"));
        assert_eq!(def.outgoing_chmod.as_deref(), Some("Fo-w"));
    }

    #[test]
    fn finish_preserves_explicit_chmod_over_defaults() {
        let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 2).unwrap();
        builder.set_incoming_chmod(Some("a+r".to_owned()), &test_config_path(), 3).unwrap();
        builder.set_outgoing_chmod(Some("a+x".to_owned()), &test_config_path(), 4).unwrap();
        let result = builder.finish(
            &test_config_path(),
            None,
            Some("default-in"),
            Some("default-out"),
            None,
        );
        assert!(result.is_ok());
        let def = result.unwrap();
        assert_eq!(def.incoming_chmod.as_deref(), Some("a+r"));
        assert_eq!(def.outgoing_chmod.as_deref(), Some("a+x"));
    }

    #[test]
    fn finish_transfers_all_set_values() {
        let mut builder = ModuleDefinitionBuilder::new("fullmod".to_owned(), 1);
        builder.set_path(PathBuf::from("/full/path"), &test_config_path(), 2).unwrap();
        builder.set_comment(Some("Full test".to_owned()), &test_config_path(), 3).unwrap();
        builder.set_read_only(false, &test_config_path(), 4).unwrap();
        builder.set_write_only(true, &test_config_path(), 5).unwrap();
        builder.set_numeric_ids(true, &test_config_path(), 6).unwrap();
        builder.set_listable(false, &test_config_path(), 7).unwrap();
        builder.set_uid(1000, &test_config_path(), 8).unwrap();
        builder.set_gid(100, &test_config_path(), 9).unwrap();
        builder.set_timeout(NonZeroU64::new(300), &test_config_path(), 10).unwrap();
        builder.set_max_connections(NonZeroU32::new(5), &test_config_path(), 11).unwrap();
        builder.set_bandwidth_limit(
            NonZeroU64::new(1000),
            NonZeroU64::new(2000),
            true,
            &test_config_path(),
            12,
        ).unwrap();

        let result = builder.finish(&test_config_path(), None, None, None, None);
        assert!(result.is_ok());
        let def = result.unwrap();

        assert_eq!(def.name, "fullmod");
        assert_eq!(def.path, PathBuf::from("/full/path"));
        assert_eq!(def.comment.as_deref(), Some("Full test"));
        assert!(!def.read_only);
        assert!(def.write_only);
        assert!(def.numeric_ids);
        assert!(!def.listable);
        assert_eq!(def.uid, Some(1000));
        assert_eq!(def.gid, Some(100));
        assert_eq!(def.timeout, NonZeroU64::new(300));
        assert_eq!(def.max_connections, NonZeroU32::new(5));
        assert_eq!(def.bandwidth_limit, NonZeroU64::new(1000));
        assert_eq!(def.bandwidth_burst, NonZeroU64::new(2000));
        assert!(def.bandwidth_burst_specified);
        assert!(def.bandwidth_limit_specified);
        assert!(def.bandwidth_limit_configured);
    }

    #[test]
    fn finish_uses_default_values_for_unset_fields() {
        let mut builder = ModuleDefinitionBuilder::new("defaults".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 2).unwrap();

        let result = builder.finish(&test_config_path(), None, None, None, None);
        assert!(result.is_ok());
        let def = result.unwrap();

        assert!(def.read_only); // default true
        assert!(!def.write_only); // default false
        assert!(!def.numeric_ids); // default false
        assert!(def.listable); // default true
        assert!(def.use_chroot); // default true
        assert!(def.hosts_allow.is_empty());
        assert!(def.hosts_deny.is_empty());
        assert!(def.auth_users.is_empty());
        assert!(def.refuse_options.is_empty());
        assert!(def.uid.is_none());
        assert!(def.gid.is_none());
        assert!(def.timeout.is_none());
        assert!(def.max_connections.is_none());
        assert!(def.bandwidth_limit.is_none());
        assert!(!def.bandwidth_limit_specified);
        assert!(!def.bandwidth_limit_configured);
        assert!(!def.fake_super); // default false
        assert!(def.munge_symlinks.is_none()); // default None (auto)
        assert_eq!(def.max_verbosity, 1); // default 1
        assert!(!def.ignore_errors); // default false
        assert!(!def.ignore_nonreadable); // default false
        assert!(!def.transfer_logging); // default false
        assert_eq!(
            def.log_format.as_deref(),
            Some("%o %h [%a] %m (%u) %f %l")
        ); // default format
        assert!(def.dont_compress.is_none());
        assert!(def.early_exec.is_none());
        assert!(def.pre_xfer_exec.is_none());
        assert!(def.post_xfer_exec.is_none());
        assert!(def.name_converter.is_none());
        assert!(def.temp_dir.is_none());
        assert!(def.charset.is_none());
        assert!(def.forward_lookup); // default true
        assert!(def.strict_modes); // default true
        assert!(def.exclude_from.is_none());
        assert!(def.include_from.is_none());
        assert!(!def.open_noatime); // default false
    }

    #[test]
    fn finish_preserves_fake_super_when_set() {
        let mut builder = ModuleDefinitionBuilder::new("fakesupermod".to_owned(), 1);
        builder.set_path(PathBuf::from("/backup"), &test_config_path(), 2).unwrap();
        builder.set_fake_super(true, &test_config_path(), 3).unwrap();

        let result = builder.finish(&test_config_path(), None, None, None, None);
        assert!(result.is_ok());
        let def = result.unwrap();
        assert!(def.fake_super);
    }

    #[test]
    fn finish_munge_symlinks_default_none() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 2).unwrap();

        let def = builder.finish(&test_config_path(), None, None, None, None).unwrap();
        assert!(def.munge_symlinks.is_none());
    }

    #[test]
    fn finish_munge_symlinks_explicit_true() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 2).unwrap();
        builder.set_munge_symlinks(Some(true), &test_config_path(), 3).unwrap();

        let def = builder.finish(&test_config_path(), None, None, None, None).unwrap();
        assert_eq!(def.munge_symlinks, Some(true));
    }

    #[test]
    fn finish_munge_symlinks_explicit_false() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 2).unwrap();
        builder.set_munge_symlinks(Some(false), &test_config_path(), 3).unwrap();

        let def = builder.finish(&test_config_path(), None, None, None, None).unwrap();
        assert_eq!(def.munge_symlinks, Some(false));
    }

    #[test]
    fn finish_transfers_exclude_from() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 2).unwrap();
        builder.set_exclude_from(PathBuf::from("/etc/excludes.txt"), &test_config_path(), 3).unwrap();

        let def = builder.finish(&test_config_path(), None, None, None, None).unwrap();
        assert_eq!(def.exclude_from, Some(PathBuf::from("/etc/excludes.txt")));
    }

    #[test]
    fn finish_transfers_include_from() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 2).unwrap();
        builder.set_include_from(PathBuf::from("/etc/includes.txt"), &test_config_path(), 3).unwrap();

        let def = builder.finish(&test_config_path(), None, None, None, None).unwrap();
        assert_eq!(def.include_from, Some(PathBuf::from("/etc/includes.txt")));
    }

    #[test]
    fn finish_preserves_open_noatime_when_set() {
        let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
        builder.set_path(PathBuf::from("/data"), &test_config_path(), 2).unwrap();
        builder.set_open_noatime(true, &test_config_path(), 3).unwrap();

        let def = builder.finish(&test_config_path(), None, None, None, None).unwrap();
        assert!(def.open_noatime);
    }
}
