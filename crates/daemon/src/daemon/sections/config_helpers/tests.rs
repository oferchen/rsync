#[cfg(test)]
mod config_helpers_tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    /// Helper to extract usernames from AuthUser list for test comparisons.
    fn usernames(users: &[AuthUser]) -> Vec<&str> {
        users.iter().map(|u| u.username.as_str()).collect()
    }

    #[test]
    fn parse_auth_user_list_single() {
        let result = parse_auth_user_list("alice").unwrap();
        assert_eq!(usernames(&result), vec!["alice"]);
        assert_eq!(result[0].access_level, UserAccessLevel::Default);
    }

    #[test]
    fn parse_auth_user_list_multiple_comma() {
        let result = parse_auth_user_list("alice, bob, charlie").unwrap();
        assert_eq!(usernames(&result), vec!["alice", "bob", "charlie"]);
    }

    #[test]
    fn parse_auth_user_list_multiple_whitespace() {
        let result = parse_auth_user_list("alice bob charlie").unwrap();
        assert_eq!(usernames(&result), vec!["alice", "bob", "charlie"]);
    }

    #[test]
    fn parse_auth_user_list_deduplicates() {
        let result = parse_auth_user_list("alice, ALICE, bob").unwrap();
        assert_eq!(usernames(&result), vec!["alice", "bob"]);
    }

    #[test]
    fn parse_auth_user_list_empty() {
        let result = parse_auth_user_list("");
        assert!(result.is_err());
    }

    #[test]
    fn parse_auth_user_list_whitespace_only() {
        let result = parse_auth_user_list("   ");
        assert!(result.is_err());
    }

    #[test]
    fn parse_auth_user_list_with_ro_suffix() {
        let result = parse_auth_user_list("alice:ro").unwrap();
        assert_eq!(usernames(&result), vec!["alice"]);
        assert_eq!(result[0].access_level, UserAccessLevel::ReadOnly);
    }

    #[test]
    fn parse_auth_user_list_with_rw_suffix() {
        let result = parse_auth_user_list("alice:rw").unwrap();
        assert_eq!(usernames(&result), vec!["alice"]);
        assert_eq!(result[0].access_level, UserAccessLevel::ReadWrite);
    }

    #[test]
    fn parse_auth_user_list_with_deny_suffix() {
        let result = parse_auth_user_list("alice:deny").unwrap();
        assert_eq!(usernames(&result), vec!["alice"]);
        assert_eq!(result[0].access_level, UserAccessLevel::Deny);
    }

    #[test]
    fn parse_auth_user_list_mixed_access_levels() {
        let result = parse_auth_user_list("alice:rw, bob:ro, charlie:deny, dave").unwrap();
        assert_eq!(usernames(&result), vec!["alice", "bob", "charlie", "dave"]);
        assert_eq!(result[0].access_level, UserAccessLevel::ReadWrite);
        assert_eq!(result[1].access_level, UserAccessLevel::ReadOnly);
        assert_eq!(result[2].access_level, UserAccessLevel::Deny);
        assert_eq!(result[3].access_level, UserAccessLevel::Default);
    }

    #[test]
    fn parse_refuse_option_list_single() {
        let result = parse_refuse_option_list("delete").unwrap();
        assert_eq!(result, vec!["delete"]);
    }

    #[test]
    fn parse_refuse_option_list_multiple() {
        let result = parse_refuse_option_list("delete, chmod, chown").unwrap();
        assert_eq!(result, vec!["delete", "chmod", "chown"]);
    }

    #[test]
    fn parse_refuse_option_list_deduplicates() {
        let result = parse_refuse_option_list("delete, DELETE, chmod").unwrap();
        assert_eq!(result, vec!["delete", "chmod"]);
    }

    #[test]
    fn parse_refuse_option_list_empty() {
        let result = parse_refuse_option_list("");
        assert!(result.is_err());
    }

    #[test]
    fn parse_boolean_directive_true_variants() {
        assert_eq!(parse_boolean_directive("1"), Some(true));
        assert_eq!(parse_boolean_directive("true"), Some(true));
        assert_eq!(parse_boolean_directive("TRUE"), Some(true));
        assert_eq!(parse_boolean_directive("yes"), Some(true));
        assert_eq!(parse_boolean_directive("YES"), Some(true));
    }

    #[test]
    fn parse_boolean_directive_false_variants() {
        assert_eq!(parse_boolean_directive("0"), Some(false));
        assert_eq!(parse_boolean_directive("false"), Some(false));
        assert_eq!(parse_boolean_directive("FALSE"), Some(false));
        assert_eq!(parse_boolean_directive("no"), Some(false));
        assert_eq!(parse_boolean_directive("NO"), Some(false));
    }

    #[test]
    fn parse_boolean_directive_rejects_on_off() {
        // upstream: loadparm.c:365-370 set_boolean() accepts only
        // yes/true/1 and no/false/0 - it never recognizes on/off. Accepting
        // them would let a config parse a value upstream rsync rejects.
        assert_eq!(parse_boolean_directive("on"), None);
        assert_eq!(parse_boolean_directive("ON"), None);
        assert_eq!(parse_boolean_directive("off"), None);
        assert_eq!(parse_boolean_directive("OFF"), None);
    }

    #[test]
    fn parse_boolean_directive_invalid() {
        assert_eq!(parse_boolean_directive("maybe"), None);
        assert_eq!(parse_boolean_directive("2"), None);
        assert_eq!(parse_boolean_directive(""), None);
    }

    #[test]
    fn classify_boolean_directive_bool3_unset_is_valid() {
        // upstream: loadparm.c:369 - a P_BOOL3 directive (use chroot, numeric
        // ids, munge symlinks, open noatime) treats `unset`/`-1` as a valid
        // tri-state, distinct from a badly formed value.
        assert!(matches!(
            classify_boolean_directive("unset", true),
            BooleanDirective::Unset
        ));
        assert!(matches!(
            classify_boolean_directive("-1", true),
            BooleanDirective::Unset
        ));
        // A plain P_BOOL directive does not accept the tri-state.
        assert!(matches!(
            classify_boolean_directive("unset", false),
            BooleanDirective::Malformed
        ));
        // Concrete values are unaffected by allow_unset.
        assert!(matches!(
            classify_boolean_directive("yes", true),
            BooleanDirective::Value(true)
        ));
    }

    #[test]
    fn apply_boolean_directive_malformed_keeps_default() {
        // upstream: loadparm.c:418-423 do_parameter() ignores set_boolean()'s
        // failure, so a badly formed boolean warns and retains the default
        // rather than aborting. Here that surfaces as `None` (no value to set).
        let path = std::path::Path::new("rsyncd.conf");
        assert_eq!(
            apply_boolean_directive("maybe", false, "read only", path, 1),
            None
        );
        // A BOOL3 `unset` also yields None (leave the setting at its default).
        assert_eq!(
            apply_boolean_directive("unset", true, "use chroot", path, 1),
            None
        );
        // Concrete values are returned for the caller to apply.
        assert_eq!(
            apply_boolean_directive("yes", false, "read only", path, 1),
            Some(true)
        );
    }

    #[test]
    fn parse_atoi_matches_c_leniency() {
        // upstream: loadparm.c:431-433 stores atoi(parmvalue) for P_INTEGER
        // directives, so a trailing non-digit suffix is tolerated.
        assert_eq!(parse_atoi("5x"), 5);
        assert_eq!(parse_atoi("30 seconds"), 30);
        assert_eq!(parse_atoi("  42"), 42);
        assert_eq!(parse_atoi("-7"), -7);
        assert_eq!(parse_atoi("abc"), 0);
        assert_eq!(parse_atoi(""), 0);
    }

    #[test]
    fn parse_boolean_directive_with_whitespace() {
        assert_eq!(parse_boolean_directive("  true  "), Some(true));
        assert_eq!(parse_boolean_directive("\tfalse\t"), Some(false));
    }

    #[test]
    fn parse_numeric_identifier_valid() {
        assert_eq!(parse_numeric_identifier("0"), Some(0));
        assert_eq!(parse_numeric_identifier("1000"), Some(1000));
        assert_eq!(parse_numeric_identifier("65534"), Some(65534));
    }

    #[test]
    fn parse_numeric_identifier_with_whitespace() {
        assert_eq!(parse_numeric_identifier("  1000  "), Some(1000));
    }

    #[test]
    fn parse_numeric_identifier_empty() {
        assert_eq!(parse_numeric_identifier(""), None);
        assert_eq!(parse_numeric_identifier("   "), None);
    }

    #[test]
    fn parse_numeric_identifier_invalid() {
        assert_eq!(parse_numeric_identifier("abc"), None);
        assert_eq!(parse_numeric_identifier("-1"), None);
    }

    /// WHY: upstream clientserver.c:791-817 - a module `gid` is a
    /// whitespace/comma-separated list. Every entry must reach `setgroups`, so
    /// the parser must preserve order and count rather than collapse to one gid.
    #[test]
    fn parse_gid_setting_accepts_list() {
        assert_eq!(
            parse_gid_setting("100"),
            Ok(GidSetting::List(vec![100])),
            "single gid parses to a one-element list"
        );
        assert_eq!(
            parse_gid_setting("100, 200 300"),
            Ok(GidSetting::List(vec![100, 200, 300])),
            "comma and whitespace both separate gid list entries"
        );
    }

    /// WHY: upstream clientserver.c:793-799 - a leading `*` requests all of the
    /// target user's groups (`want_all_groups`), and may be followed by extra
    /// explicit gids.
    #[test]
    fn parse_gid_setting_accepts_star() {
        assert_eq!(
            parse_gid_setting("*"),
            Ok(GidSetting::AllUserGroups { extra: vec![] })
        );
        assert_eq!(
            parse_gid_setting("*, 42"),
            Ok(GidSetting::AllUserGroups { extra: vec![42] })
        );
    }

    /// WHY: upstream clientserver.c:793 - `The "*" gid must be the first item in
    /// the list.` A `*` appearing later is a configuration error.
    #[test]
    fn parse_gid_setting_rejects_non_leading_star() {
        assert!(parse_gid_setting("100, *").is_err());
    }

    #[test]
    fn parse_gid_setting_rejects_empty_and_unresolvable() {
        assert!(parse_gid_setting("   ").is_err());
        // WHY: upstream group_to_gid() fails when getgrnam() cannot resolve the
        // name, so a name guaranteed absent from the group database must error
        // rather than default silently. The `\0`-free control name below is not
        // a valid group anywhere.
        assert!(parse_gid_setting("oc_definitely_absent_group").is_err());
    }

    /// WHY: the canonical rsyncd.conf ships `uid = nobody` / `gid = nogroup`.
    /// upstream resolves both a numeric id and a name (user_to_uid /
    /// group_to_gid with num_ok=True); oc must accept the name form too or the
    /// default config fails to parse. `root`/gid 0 exist on every POSIX host,
    /// giving a deterministic name to resolve.
    #[test]
    #[cfg(unix)]
    fn parse_uid_setting_resolves_root_name_and_numeric() {
        assert_eq!(parse_uid_setting("0"), Some(0));
        assert_eq!(parse_uid_setting("65534"), Some(65534));
        assert_eq!(
            parse_uid_setting("root"),
            Some(0),
            "the username 'root' must resolve to uid 0"
        );
    }

    /// WHY: a name that resolves nowhere must surface as a parse failure (the
    /// caller wraps `None` into `invalid uid '<value>'`), never a silent
    /// default. Mirrors upstream `@ERROR: invalid uid <name>`.
    #[test]
    fn parse_uid_setting_rejects_empty_and_unresolvable() {
        assert_eq!(parse_uid_setting(""), None);
        assert_eq!(parse_uid_setting("   "), None);
        assert_eq!(parse_uid_setting("oc_definitely_absent_user"), None);
    }

    /// WHY: `nobody`/`nogroup` are the canonical daemon defaults. They exist on
    /// most hosts but not all CI images, so resolve-or-skip rather than assert a
    /// fixed id; when present, the resolved value must be a real numeric id.
    #[test]
    #[cfg(unix)]
    fn parse_uid_setting_resolves_nobody_when_present() {
        if let Some(uid) = parse_uid_setting("nobody") {
            assert_eq!(parse_uid_setting(&uid.to_string()), Some(uid));
        }
        if let Ok(GidSetting::List(gids)) = parse_gid_setting("nogroup") {
            assert_eq!(gids.len(), 1, "a single group name yields one gid");
        }
    }

    /// WHY: a group name resolves the same as a numeric gid. `root`/`wheel` gid 0
    /// exists on every POSIX host under one of those two names.
    #[test]
    #[cfg(unix)]
    fn parse_gid_setting_resolves_group_name() {
        let by_name = parse_gid_setting("root")
            .or_else(|_| parse_gid_setting("wheel"))
            .expect("gid 0 is named root or wheel on every POSIX host");
        assert_eq!(by_name, GidSetting::List(vec![0]));
    }

    #[test]
    fn parse_timeout_seconds_zero() {
        assert_eq!(parse_timeout_seconds("0"), Some(None));
    }

    #[test]
    fn parse_timeout_seconds_positive() {
        let result = parse_timeout_seconds("30").unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().get(), 30);
    }

    #[test]
    fn parse_timeout_seconds_empty_disables() {
        // upstream: `timeout` is P_INTEGER, so atoi("") == 0 disables it.
        assert_eq!(parse_timeout_seconds(""), Some(None));
    }

    #[test]
    fn parse_timeout_seconds_atoi_leniency() {
        // upstream: atoi("abc") == 0 (disabled); atoi("30x") == 30.
        assert_eq!(parse_timeout_seconds("abc"), Some(None));
        assert_eq!(parse_timeout_seconds("30x").unwrap().unwrap().get(), 30);
    }

    #[test]
    fn parse_max_connections_directive_zero() {
        assert_eq!(parse_max_connections_directive("0"), Some(None));
    }

    #[test]
    fn parse_max_connections_directive_positive() {
        let result = parse_max_connections_directive("10").unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().get(), 10);
    }

    #[test]
    fn parse_max_connections_directive_empty_is_unlimited() {
        // upstream: `max connections` is P_INTEGER, so atoi("") == 0 (unlimited).
        assert_eq!(parse_max_connections_directive(""), Some(None));
    }

    #[test]
    fn parse_max_connections_directive_atoi_leniency() {
        // upstream: atoi("abc") == 0 (unlimited); atoi("10x") == 10.
        assert_eq!(parse_max_connections_directive("abc"), Some(None));
        assert_eq!(
            parse_max_connections_directive("10x").unwrap().unwrap().get(),
            10
        );
    }

    #[test]
    fn host_pattern_parse_any_star() {
        let pattern = HostPattern::parse("*").unwrap();
        assert_eq!(pattern, HostPattern::Any);
    }

    #[test]
    fn host_pattern_parse_any_all() {
        let pattern = HostPattern::parse("all").unwrap();
        assert_eq!(pattern, HostPattern::Any);
        let pattern = HostPattern::parse("ALL").unwrap();
        assert_eq!(pattern, HostPattern::Any);
    }

    #[test]
    fn host_pattern_parse_ipv4_no_prefix() {
        let pattern = HostPattern::parse("192.168.1.1").unwrap();
        if let HostPattern::Ipv4 { network, prefix } = pattern {
            assert_eq!(network, Ipv4Addr::new(192, 168, 1, 1));
            assert_eq!(prefix, 32);
        } else {
            panic!("expected Ipv4 pattern");
        }
    }

    #[test]
    fn host_pattern_parse_ipv4_with_prefix() {
        let pattern = HostPattern::parse("192.168.0.0/16").unwrap();
        if let HostPattern::Ipv4 { network, prefix } = pattern {
            assert_eq!(network, Ipv4Addr::new(192, 168, 0, 0));
            assert_eq!(prefix, 16);
        } else {
            panic!("expected Ipv4 pattern");
        }
    }

    #[test]
    fn host_pattern_parse_ipv6() {
        let pattern = HostPattern::parse("::1").unwrap();
        if let HostPattern::Ipv6 { network, prefix } = pattern {
            assert_eq!(network, Ipv6Addr::LOCALHOST);
            assert_eq!(prefix, 128);
        } else {
            panic!("expected Ipv6 pattern");
        }
    }

    #[test]
    fn host_pattern_parse_hostname() {
        let pattern = HostPattern::parse("example.com").unwrap();
        assert!(matches!(pattern, HostPattern::Hostname(_)));
    }

    #[test]
    fn host_pattern_parse_empty() {
        let result = HostPattern::parse("");
        assert!(result.is_err());
    }

    // WHY: operators centrally manage trusted hosts via netgroups. A `@name`
    // token must parse into the dedicated netgroup variant (upstream
    // access.c:41-42) rather than a literal hostname, and the name must be
    // lowercased to match upstream's `strlower(list2)` (access.c:251).
    #[test]
    fn host_pattern_parse_netgroup_token() {
        let pattern = HostPattern::parse("@Trusted").unwrap();
        assert_eq!(pattern, HostPattern::Netgroup("trusted".to_owned()));
    }

    // WHY: upstream requires `tok[1]` before treating `@` as a netgroup
    // (access.c:41). A bare `@` is therefore not a netgroup; it falls through
    // to an ordinary hostname token that can never match a real hostname.
    #[test]
    fn host_pattern_parse_bare_at_is_not_netgroup() {
        let pattern = HostPattern::parse("@").unwrap();
        assert!(matches!(pattern, HostPattern::Hostname(_)));
    }

    #[test]
    fn host_pattern_parse_invalid_prefix() {
        let result = HostPattern::parse("192.168.1.1/abc");
        assert!(result.is_err());
    }

    #[test]
    fn host_pattern_parse_ipv4_prefix_too_large() {
        let result = HostPattern::parse("192.168.1.1/33");
        assert!(result.is_err());
    }

    #[test]
    fn host_pattern_parse_ipv6_prefix_too_large() {
        let result = HostPattern::parse("::1/129");
        assert!(result.is_err());
    }

    #[test]
    fn address_family_from_ip_v4() {
        let family = AddressFamily::from_ip(IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(family, AddressFamily::Ipv4);
    }

    #[test]
    fn address_family_from_ip_v6() {
        let family = AddressFamily::from_ip(IpAddr::V6(Ipv6Addr::LOCALHOST));
        assert_eq!(family, AddressFamily::Ipv6);
    }

    #[test]
    fn address_family_matches_v4() {
        assert!(AddressFamily::Ipv4.matches(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(!AddressFamily::Ipv4.matches(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    }

    #[test]
    fn address_family_matches_v6() {
        assert!(AddressFamily::Ipv6.matches(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(!AddressFamily::Ipv6.matches(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    }

    #[test]
    fn wildcard_match_exact() {
        assert!(wildcard_match("hello", "hello"));
        assert!(!wildcard_match("hello", "world"));
    }

    #[test]
    fn wildcard_match_question_mark() {
        assert!(wildcard_match("h?llo", "hello"));
        assert!(wildcard_match("h?llo", "hallo"));
        assert!(!wildcard_match("h?llo", "hllo"));
    }

    #[test]
    fn wildcard_match_star() {
        assert!(wildcard_match("*.com", "example.com"));
        assert!(wildcard_match("hello*", "hello world"));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("*", ""));
    }

    #[test]
    fn wildcard_match_combined() {
        assert!(wildcard_match("h*o", "hello"));
        assert!(wildcard_match("h*o", "ho"));
        assert!(wildcard_match("*.*", "a.b"));
        assert!(!wildcard_match("*.*", "abc"));
    }

    #[test]
    fn wildcard_match_multiple_stars() {
        assert!(wildcard_match("*.*.*", "a.b.c"));
        assert!(wildcard_match("**", "anything"));
    }

    #[test]
    fn hostname_pattern_exact_match() {
        let pattern = HostnamePattern::parse("example.com").unwrap();
        assert!(pattern.matches("example.com"));
        assert!(!pattern.matches("www.example.com"));
        assert!(!pattern.matches("example.org"));
    }

    #[test]
    fn hostname_pattern_suffix_match() {
        let pattern = HostnamePattern::parse(".example.com").unwrap();
        assert!(pattern.matches("www.example.com"));
        assert!(pattern.matches("foo.bar.example.com"));
        assert!(pattern.matches("example.com"));
        assert!(!pattern.matches("notexample.com"));
    }

    #[test]
    fn hostname_pattern_wildcard() {
        let pattern = HostnamePattern::parse("*.example.com").unwrap();
        assert!(pattern.matches("www.example.com"));
        assert!(!pattern.matches("example.com"));
    }
}
