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
    builder.set_path(PathBuf::from("/data"));
    assert_eq!(builder.path, Some(PathBuf::from("/data")));
}

/// Repeating a directive inside one module section is not an error: upstream
/// loadparm.c:379-470 do_parameter() resolves the parameter pointer and assigns
/// unconditionally, so the last assignment is the one the module serves. The
/// real rsync 3.4.4 daemon starts and serves `path = b` for `path = a` followed
/// by `path = b`; rejecting the config would refuse a daemon upstream runs.
#[test]
fn set_path_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_path(PathBuf::from("/data"));
    builder.set_path(PathBuf::from("/other"));
    assert_eq!(builder.path, Some(PathBuf::from("/other")));
}

#[test]
fn set_comment_stores_value() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_comment(Some("A test module".to_owned()));
    assert_eq!(builder.comment, Some("A test module".to_owned()));
}

#[test]
fn set_comment_allows_none() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_comment(None);
    assert!(builder.comment.is_none());
}

#[test]
fn set_comment_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_comment(Some("first".to_owned()));
    builder.set_comment(Some("second".to_owned()));
    assert_eq!(builder.comment, Some("second".to_owned()));
}

#[test]
fn set_hosts_allow_stores_patterns() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    let patterns = vec![HostPattern::Any];
    builder.set_hosts_allow(patterns.clone());
    assert_eq!(builder.hosts_allow, Some(patterns));
}

#[test]
fn set_hosts_allow_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_hosts_allow(vec![HostPattern::Any]);
    builder.set_hosts_allow(vec![]);
    assert_eq!(builder.hosts_allow, Some(vec![]));
}

#[test]
fn set_hosts_deny_stores_patterns() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    let patterns = vec![HostPattern::Any];
    builder.set_hosts_deny(patterns.clone());
    assert_eq!(builder.hosts_deny, Some(patterns));
}

#[test]
fn set_hosts_deny_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_hosts_deny(vec![HostPattern::Any]);
    builder.set_hosts_deny(vec![]);
    assert_eq!(builder.hosts_deny, Some(vec![]));
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

/// An empty `auth users` list stays a hard error: authenticate.c:228
/// auth_server() treats a non-empty list as "authentication required", so an
/// empty one is a value mistake, not a duplication.
#[test]
fn set_auth_users_rejects_empty() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    let result = builder.set_auth_users(vec![], &test_config_path(), 5);
    assert!(result.is_err());
}

#[test]
fn set_auth_users_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder
        .set_auth_users(
            vec![AuthUser::new("alice".to_owned())],
            &test_config_path(),
            5,
        )
        .expect("first list accepted");
    builder
        .set_auth_users(
            vec![AuthUser::new("bob".to_owned())],
            &test_config_path(),
            10,
        )
        .expect("repeat overwrites");
    assert_eq!(
        builder.auth_users,
        Some(vec![AuthUser::new("bob".to_owned())])
    );
}

#[test]
fn set_refuse_options_stores_options() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    let options = vec!["delete".to_owned(), "hardlinks".to_owned()];
    let result = builder.set_refuse_options(options.clone(), &test_config_path(), 5);
    assert!(result.is_ok());
    assert_eq!(builder.refuse_options, Some(options));
}

/// An empty `refuse options` list stays a hard error - it would refuse nothing,
/// which is a value mistake rather than a repeated directive.
#[test]
fn set_refuse_options_rejects_empty() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    let result = builder.set_refuse_options(vec![], &test_config_path(), 5);
    assert!(result.is_err());
}

#[test]
fn set_refuse_options_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder
        .set_refuse_options(vec!["delete".to_owned()], &test_config_path(), 5)
        .expect("first list accepted");
    builder
        .set_refuse_options(vec!["hardlinks".to_owned()], &test_config_path(), 10)
        .expect("repeat overwrites");
    assert_eq!(builder.refuse_options, Some(vec!["hardlinks".to_owned()]));
}

#[test]
fn set_read_only_stores_value() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_read_only(false);
    assert_eq!(builder.read_only, Some(false));
}

#[test]
fn set_read_only_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_read_only(true);
    builder.set_read_only(false);
    assert_eq!(builder.read_only, Some(false));
}

#[test]
fn set_write_only_stores_value() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_write_only(true);
    assert_eq!(builder.write_only, Some(true));
}

#[test]
fn set_write_only_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_write_only(true);
    builder.set_write_only(false);
    assert_eq!(builder.write_only, Some(false));
}

#[test]
fn set_numeric_ids_stores_value() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_numeric_ids(true);
    assert_eq!(builder.numeric_ids, Some(true));
}

#[test]
fn set_numeric_ids_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_numeric_ids(true);
    builder.set_numeric_ids(false);
    assert_eq!(builder.numeric_ids, Some(false));
}

#[test]
fn set_listable_stores_value() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_listable(false);
    assert_eq!(builder.listable, Some(false));
}

#[test]
fn set_listable_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_listable(true);
    builder.set_listable(false);
    assert_eq!(builder.listable, Some(false));
}

#[test]
fn set_use_chroot_stores_value() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_use_chroot(false);
    assert_eq!(builder.use_chroot, Some(false));
}

#[test]
fn set_use_chroot_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_use_chroot(true);
    builder.set_use_chroot(false);
    assert_eq!(builder.use_chroot, Some(false));
}

#[test]
fn set_uid_stores_value() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_uid(1000);
    assert_eq!(builder.uid, Some(1000));
}

#[test]
fn set_uid_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_uid(1000);
    builder.set_uid(2000);
    assert_eq!(builder.uid, Some(2000));
}

#[test]
fn set_gid_stores_value() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_gid(GidSetting::List(vec![100]));
    assert_eq!(builder.gid, Some(GidSetting::List(vec![100])));
}

#[test]
fn set_gid_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_gid(GidSetting::List(vec![100]));
    builder.set_gid(GidSetting::List(vec![200]));
    assert_eq!(builder.gid, Some(GidSetting::List(vec![200])));
}

#[test]
fn set_timeout_stores_value() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    let timeout = NonZeroU64::new(60);
    builder.set_timeout(timeout);
    assert_eq!(builder.timeout, Some(timeout));
}

#[test]
fn set_timeout_allows_none() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_timeout(None);
    assert_eq!(builder.timeout, Some(None));
}

#[test]
fn set_timeout_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_timeout(NonZeroU64::new(60));
    builder.set_timeout(NonZeroU64::new(120));
    assert_eq!(builder.timeout, Some(NonZeroU64::new(120)));
}

#[test]
fn set_max_connections_stores_value() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    let max = MaxConnections::Limited(NonZeroU32::new(10).expect("non-zero"));
    builder.set_max_connections(max);
    assert_eq!(builder.max_connections, Some(max));
}

#[test]
fn set_max_connections_allows_unlimited() {
    // upstream: connection.c:claim_connection:27 - `max connections = 0` is a
    // valid setting meaning unlimited, distinct from the directive being unset.
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_max_connections(MaxConnections::Unlimited);
    assert_eq!(builder.max_connections, Some(MaxConnections::Unlimited));
}

#[test]
fn set_max_connections_allows_disabled() {
    // upstream: rsyncd.conf.5 - "A negative value disables the module". The
    // builder must retain the sign so the refusal echoes it verbatim.
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_max_connections(MaxConnections::Disabled(-1));
    assert_eq!(builder.max_connections, Some(MaxConnections::Disabled(-1)));
}

#[test]
fn set_max_connections_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    let first = MaxConnections::Limited(NonZeroU32::new(10).expect("non-zero"));
    let second = MaxConnections::Limited(NonZeroU32::new(20).expect("non-zero"));
    builder.set_max_connections(first);
    builder.set_max_connections(second);
    assert_eq!(builder.max_connections, Some(second));
}

#[test]
fn set_incoming_chmod_stores_value() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_incoming_chmod(Some("Dg+s,ug+w".to_owned()));
    assert_eq!(builder.incoming_chmod, Some(Some("Dg+s,ug+w".to_owned())));
}

#[test]
fn set_incoming_chmod_allows_none() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_incoming_chmod(None);
    assert_eq!(builder.incoming_chmod, Some(None));
}

#[test]
fn set_incoming_chmod_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_incoming_chmod(Some("a+r".to_owned()));
    builder.set_incoming_chmod(Some("a+w".to_owned()));
    assert_eq!(builder.incoming_chmod, Some(Some("a+w".to_owned())));
}

#[test]
fn set_outgoing_chmod_stores_value() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_outgoing_chmod(Some("Fo-w,+X".to_owned()));
    assert_eq!(builder.outgoing_chmod, Some(Some("Fo-w,+X".to_owned())));
}

#[test]
fn set_outgoing_chmod_allows_none() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_outgoing_chmod(None);
    assert_eq!(builder.outgoing_chmod, Some(None));
}

#[test]
fn set_outgoing_chmod_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_outgoing_chmod(Some("a+r".to_owned()));
    builder.set_outgoing_chmod(Some("a+w".to_owned()));
    assert_eq!(builder.outgoing_chmod, Some(Some("a+w".to_owned())));
}

#[test]
fn set_fake_super_stores_value() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_fake_super(true);
    assert_eq!(builder.fake_super, Some(true));
}

#[test]
fn set_fake_super_stores_false() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_fake_super(false);
    assert_eq!(builder.fake_super, Some(false));
}

#[test]
fn set_fake_super_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_fake_super(true);
    builder.set_fake_super(false);
    assert_eq!(builder.fake_super, Some(false));
}

#[test]
fn set_munge_symlinks_stores_some_true() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_munge_symlinks(Some(true));
    assert_eq!(builder.munge_symlinks, Some(Some(true)));
}

#[test]
fn set_munge_symlinks_stores_some_false() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_munge_symlinks(Some(false));
    assert_eq!(builder.munge_symlinks, Some(Some(false)));
}

#[test]
fn set_munge_symlinks_stores_none() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_munge_symlinks(None);
    assert_eq!(builder.munge_symlinks, Some(None));
}

#[test]
fn set_munge_symlinks_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_munge_symlinks(Some(true));
    builder.set_munge_symlinks(Some(false));
    assert_eq!(builder.munge_symlinks, Some(Some(false)));
}

#[test]
fn set_strict_modes_stores_true() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_strict_modes(true);
    assert_eq!(builder.strict_modes, Some(true));
}

#[test]
fn set_strict_modes_stores_false() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_strict_modes(false);
    assert_eq!(builder.strict_modes, Some(false));
}

#[test]
fn set_strict_modes_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_strict_modes(true);
    builder.set_strict_modes(false);
    assert_eq!(builder.strict_modes, Some(false));
}

#[test]
fn set_exclude_from_stores_value() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_exclude_from(PathBuf::from("/etc/excludes.txt"));
    assert_eq!(
        builder.exclude_from,
        Some(PathBuf::from("/etc/excludes.txt"))
    );
}

#[test]
fn set_exclude_from_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_exclude_from(PathBuf::from("/etc/excludes.txt"));
    builder.set_exclude_from(PathBuf::from("/etc/other.txt"));
    assert_eq!(builder.exclude_from, Some(PathBuf::from("/etc/other.txt")));
}

#[test]
fn set_include_from_stores_value() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_include_from(PathBuf::from("/etc/includes.txt"));
    assert_eq!(
        builder.include_from,
        Some(PathBuf::from("/etc/includes.txt"))
    );
}

#[test]
fn set_include_from_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_include_from(PathBuf::from("/etc/includes.txt"));
    builder.set_include_from(PathBuf::from("/etc/other.txt"));
    assert_eq!(builder.include_from, Some(PathBuf::from("/etc/other.txt")));
}

#[test]
fn set_open_noatime_stores_true() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_open_noatime(true);
    assert_eq!(builder.open_noatime, Some(true));
}

#[test]
fn set_open_noatime_stores_false() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_open_noatime(false);
    assert_eq!(builder.open_noatime, Some(false));
}

#[test]
fn set_open_noatime_last_assignment_wins() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_open_noatime(true);
    builder.set_open_noatime(false);
    assert_eq!(builder.open_noatime, Some(false));
}

#[test]
fn finish_succeeds_with_minimal_config() {
    let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
    builder.set_path(PathBuf::from("/data"));
    let defaults = GlobalModuleDefaults::default();
    let result = builder.finish(&test_config_path(), None, None, None, None, &defaults);
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
    let defaults = GlobalModuleDefaults::default();
    let result = builder.finish(&test_config_path(), None, None, None, None, &defaults);
    assert!(result.is_err());
}

/// A relative path under `use chroot` is RESOLVED against the current
/// directory, not refused. upstream: `normalize_path` (util1.c:1409-1416)
/// makes a non-`/` path absolute by joining it onto `curr_dir`, and
/// `rsync_module()` routes the chroot arm through it (clientserver.c:898-916).
#[test]
fn finish_resolves_a_relative_path_with_chroot() {
    let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
    builder.set_path(PathBuf::from("relative/path"));
    // use_chroot defaults to true
    let defaults = GlobalModuleDefaults::default();
    let def = builder
        .finish(&test_config_path(), None, None, None, None, &defaults)
        .expect("a relative path is resolved under chroot, not refused");
    let expected = std::env::current_dir()
        .expect("current directory")
        .join("relative/path");
    assert_eq!(def.path, expected);
}

/// The no-chroot arm takes the SAME rule: upstream normalizes both arms
/// identically (clientserver.c:916-918), so resolution is not gated on
/// `use chroot`. Keeping both arms asserted is what stops the two from
/// drifting apart again.
#[test]
fn finish_resolves_a_relative_path_without_chroot() {
    let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
    builder.set_path(PathBuf::from("relative/path"));
    builder.set_use_chroot(false);
    let defaults = GlobalModuleDefaults::default();
    let def = builder
        .finish(&test_config_path(), None, None, None, None, &defaults)
        .expect("a relative path is resolved without chroot too");
    let expected = std::env::current_dir()
        .expect("current directory")
        .join("relative/path");
    assert_eq!(def.path, expected);
}

/// Root path `/` is a legitimate module root: upstream loadparm.c P_PATH
/// preserves the bare slash (its trailing-slash strip only fires when
/// `len > 1`), and clientserver.c chroot's into it as a no-op when
/// `use chroot = yes`. The `is_absolute()` gate must pass for `/` so the
/// daemon-path-root-read scenario is accepted under chroot.
#[test]
#[cfg(unix)]
fn finish_allows_root_path_with_chroot() {
    let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
    builder.set_path(PathBuf::from("/"));
    builder.set_use_chroot(true);
    let defaults = GlobalModuleDefaults::default();
    let result = builder.finish(&test_config_path(), None, None, None, None, &defaults);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().path, PathBuf::from("/"));
}

/// Companion to `finish_allows_root_path_with_chroot`: the upstream daemon
/// also serves `path = /` when `use chroot = no`, exposing the absolute root
/// without the chroot wrapper. The validator must accept this combination.
#[test]
#[cfg(unix)]
fn finish_allows_root_path_without_chroot() {
    let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
    builder.set_path(PathBuf::from("/"));
    builder.set_use_chroot(false);
    let defaults = GlobalModuleDefaults::default();
    let result = builder.finish(&test_config_path(), None, None, None, None, &defaults);
    assert!(result.is_ok());
    let def = result.unwrap();
    assert_eq!(def.path, PathBuf::from("/"));
    assert!(!def.use_chroot);
}

#[test]
fn finish_applies_default_secrets_for_auth_users() {
    let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
    builder.set_path(PathBuf::from("/data"));
    builder
        .set_auth_users(
            vec![AuthUser::new("alice".to_owned())],
            &test_config_path(),
            3,
        )
        .expect("auth users accepted");
    let default_secrets = PathBuf::from("/etc/secrets");
    let defaults = GlobalModuleDefaults::default();
    let result = builder.finish(
        &test_config_path(),
        Some(&default_secrets),
        None,
        None,
        None,
        &defaults,
    );
    assert!(result.is_ok());
    let def = result.unwrap();
    assert_eq!(def.secrets_file, Some(PathBuf::from("/etc/secrets")));
}

#[test]
fn finish_fails_auth_users_without_secrets() {
    let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
    builder.set_path(PathBuf::from("/data"));
    builder
        .set_auth_users(
            vec![AuthUser::new("alice".to_owned())],
            &test_config_path(),
            3,
        )
        .expect("auth users accepted");
    let defaults = GlobalModuleDefaults::default();
    let result = builder.finish(&test_config_path(), None, None, None, None, &defaults);
    assert!(result.is_err());
}

#[test]
fn finish_applies_default_chmod_values() {
    let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
    builder.set_path(PathBuf::from("/data"));
    let defaults = GlobalModuleDefaults::default();
    let result = builder.finish(
        &test_config_path(),
        None,
        Some("Dg+s"),
        Some("Fo-w"),
        None,
        &defaults,
    );
    assert!(result.is_ok());
    let def = result.unwrap();
    assert_eq!(def.incoming_chmod.as_deref(), Some("Dg+s"));
    assert_eq!(def.outgoing_chmod.as_deref(), Some("Fo-w"));
}

#[test]
fn finish_preserves_explicit_chmod_over_defaults() {
    let mut builder = ModuleDefinitionBuilder::new("testmod".to_owned(), 1);
    builder.set_path(PathBuf::from("/data"));
    builder.set_incoming_chmod(Some("a+r".to_owned()));
    builder.set_outgoing_chmod(Some("a+x".to_owned()));
    let defaults = GlobalModuleDefaults::default();
    let result = builder.finish(
        &test_config_path(),
        None,
        Some("default-in"),
        Some("default-out"),
        None,
        &defaults,
    );
    assert!(result.is_ok());
    let def = result.unwrap();
    assert_eq!(def.incoming_chmod.as_deref(), Some("a+r"));
    assert_eq!(def.outgoing_chmod.as_deref(), Some("a+x"));
}

#[test]
fn finish_transfers_all_set_values() {
    let mut builder = ModuleDefinitionBuilder::new("fullmod".to_owned(), 1);
    builder.set_path(PathBuf::from("/full/path"));
    builder.set_comment(Some("Full test".to_owned()));
    builder.set_read_only(false);
    builder.set_write_only(true);
    builder.set_numeric_ids(true);
    builder.set_listable(false);
    builder.set_uid(1000);
    builder.set_gid(GidSetting::List(vec![100]));
    builder.set_timeout(NonZeroU64::new(300));
    builder.set_max_connections(MaxConnections::Limited(
        NonZeroU32::new(5).expect("non-zero"),
    ));

    let result = builder.finish(
        &test_config_path(),
        None,
        None,
        None,
        None,
        &GlobalModuleDefaults::default(),
    );
    assert!(result.is_ok());
    let def = result.unwrap();

    assert_eq!(def.name, "fullmod");
    assert_eq!(def.path, PathBuf::from("/full/path"));
    assert_eq!(def.comment.as_deref(), Some("Full test"));
    assert!(!def.read_only);
    assert!(def.write_only);
    assert_eq!(def.numeric_ids, Some(true));
    assert!(!def.listable);
    assert_eq!(def.uid, Some(1000));
    assert_eq!(def.gid, Some(GidSetting::List(vec![100])));
    assert_eq!(def.timeout, NonZeroU64::new(300));
    assert_eq!(
        def.max_connections,
        MaxConnections::Limited(NonZeroU32::new(5).expect("non-zero"))
    );
}

#[test]
fn finish_uses_default_values_for_unset_fields() {
    let mut builder = ModuleDefinitionBuilder::new("defaults".to_owned(), 1);
    builder.set_path(PathBuf::from("/data"));

    let result = builder.finish(
        &test_config_path(),
        None,
        None,
        None,
        None,
        &GlobalModuleDefaults::default(),
    );
    assert!(result.is_ok());
    let def = result.unwrap();

    assert!(def.read_only); // default true
    assert!(!def.write_only); // default false
    assert_eq!(def.numeric_ids, None); // default unset (BOOL3)
    assert!(def.listable); // default true
    assert!(def.use_chroot); // default true
    assert!(def.hosts_allow.is_empty());
    assert!(def.hosts_deny.is_empty());
    assert!(def.auth_users.is_empty());
    assert!(def.refuse_options.is_empty());
    assert!(def.uid.is_none());
    assert!(def.gid.is_none());
    assert!(def.timeout.is_none());
    assert_eq!(def.max_connections, MaxConnections::Unlimited);
    assert!(!def.fake_super); // default false
    assert!(def.munge_symlinks.is_none()); // default None (auto)
    assert_eq!(def.max_verbosity, 1); // default 1
    assert!(!def.ignore_errors); // default false
    assert!(!def.ignore_nonreadable); // default false
    assert!(!def.transfer_logging); // default false
    assert_eq!(def.log_format.as_deref(), Some("%o %h [%a] %m (%u) %f %l")); // default format
    // upstream: loadparm.c:46 - `dont compress` defaults to the built-in
    // DEFAULT_DONT_COMPRESS suffix list, so a resolved module inherits it when
    // neither the module nor the global section sets the directive.
    assert_eq!(def.dont_compress.as_deref(), Some(DEFAULT_DONT_COMPRESS));
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
    builder.set_path(PathBuf::from("/backup"));
    builder.set_fake_super(true);

    let result = builder.finish(
        &test_config_path(),
        None,
        None,
        None,
        None,
        &GlobalModuleDefaults::default(),
    );
    assert!(result.is_ok());
    let def = result.unwrap();
    assert!(def.fake_super);
}

#[test]
fn finish_munge_symlinks_default_none() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_path(PathBuf::from("/data"));

    let def = builder
        .finish(
            &test_config_path(),
            None,
            None,
            None,
            None,
            &GlobalModuleDefaults::default(),
        )
        .unwrap();
    assert!(def.munge_symlinks.is_none());
}

#[test]
fn finish_munge_symlinks_explicit_true() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_path(PathBuf::from("/data"));
    builder.set_munge_symlinks(Some(true));

    let def = builder
        .finish(
            &test_config_path(),
            None,
            None,
            None,
            None,
            &GlobalModuleDefaults::default(),
        )
        .unwrap();
    assert_eq!(def.munge_symlinks, Some(true));
}

#[test]
fn finish_munge_symlinks_explicit_false() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_path(PathBuf::from("/data"));
    builder.set_munge_symlinks(Some(false));

    let def = builder
        .finish(
            &test_config_path(),
            None,
            None,
            None,
            None,
            &GlobalModuleDefaults::default(),
        )
        .unwrap();
    assert_eq!(def.munge_symlinks, Some(false));
}

#[test]
fn finish_transfers_exclude_from() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_path(PathBuf::from("/data"));
    builder.set_exclude_from(PathBuf::from("/etc/excludes.txt"));

    let def = builder
        .finish(
            &test_config_path(),
            None,
            None,
            None,
            None,
            &GlobalModuleDefaults::default(),
        )
        .unwrap();
    assert_eq!(def.exclude_from, Some(PathBuf::from("/etc/excludes.txt")));
}

#[test]
fn finish_transfers_include_from() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_path(PathBuf::from("/data"));
    builder.set_include_from(PathBuf::from("/etc/includes.txt"));

    let def = builder
        .finish(
            &test_config_path(),
            None,
            None,
            None,
            None,
            &GlobalModuleDefaults::default(),
        )
        .unwrap();
    assert_eq!(def.include_from, Some(PathBuf::from("/etc/includes.txt")));
}

#[test]
fn finish_preserves_open_noatime_when_set() {
    let mut builder = ModuleDefinitionBuilder::new("mod".to_owned(), 1);
    builder.set_path(PathBuf::from("/data"));
    builder.set_open_noatime(true);

    let def = builder
        .finish(
            &test_config_path(),
            None,
            None,
            None,
            None,
            &GlobalModuleDefaults::default(),
        )
        .unwrap();
    assert!(def.open_noatime);
}
