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
        assert_eq!(parse_boolean_directive("on"), Some(true));
        assert_eq!(parse_boolean_directive("ON"), Some(true));
    }

    #[test]
    fn parse_boolean_directive_false_variants() {
        assert_eq!(parse_boolean_directive("0"), Some(false));
        assert_eq!(parse_boolean_directive("false"), Some(false));
        assert_eq!(parse_boolean_directive("FALSE"), Some(false));
        assert_eq!(parse_boolean_directive("no"), Some(false));
        assert_eq!(parse_boolean_directive("NO"), Some(false));
        assert_eq!(parse_boolean_directive("off"), Some(false));
        assert_eq!(parse_boolean_directive("OFF"), Some(false));
    }

    #[test]
    fn parse_boolean_directive_invalid() {
        assert_eq!(parse_boolean_directive("maybe"), None);
        assert_eq!(parse_boolean_directive("2"), None);
        assert_eq!(parse_boolean_directive(""), None);
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
    fn parse_timeout_seconds_empty() {
        assert_eq!(parse_timeout_seconds(""), None);
    }

    #[test]
    fn parse_timeout_seconds_invalid() {
        assert_eq!(parse_timeout_seconds("abc"), None);
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
    fn parse_max_connections_directive_empty() {
        assert_eq!(parse_max_connections_directive(""), None);
    }

    #[test]
    fn parse_max_connections_directive_invalid() {
        assert_eq!(parse_max_connections_directive("abc"), None);
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
