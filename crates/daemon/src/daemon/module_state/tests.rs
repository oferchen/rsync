use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::num::{NonZeroU32, NonZeroU64};
use std::path::PathBuf;

use super::*;
// Host pattern types are defined in the parent daemon module (via include!() of config_helpers.rs).
use crate::daemon::{HostPattern, HostnamePattern, HostnamePatternKind};

#[test]
fn module_definition_default() {
    let def = ModuleDefinition::default();
    assert!(def.name.is_empty());
    assert!(def.path.as_os_str().is_empty());
    assert!(def.comment.is_none());
    assert!(def.hosts_allow.is_empty());
    assert!(def.hosts_deny.is_empty());
    assert!(def.auth_users.is_empty());
    assert!(!def.read_only);
    assert!(!def.write_only);
    assert!(!def.listable);
    assert!(def.munge_symlinks.is_none());
    assert!(def.exclude_from.is_none());
    assert!(def.include_from.is_none());
    assert!(!def.open_noatime);
}

#[test]
fn module_definition_permits_all_when_no_rules() {
    let def = ModuleDefinition::default();
    let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
    assert!(def.permits(addr, None));
    assert!(def.permits(addr, Some("example.com")));
}

#[test]
fn module_definition_permits_respects_hosts_allow() {
    let def = ModuleDefinition {
        hosts_allow: vec![HostPattern::Ipv4 {
            network: Ipv4Addr::new(192, 168, 0, 0),
            prefix: 16,
        }],
        ..Default::default()
    };
    let allowed = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
    let denied = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    assert!(def.permits(allowed, None));
    assert!(!def.permits(denied, None));
}

#[test]
fn module_definition_permits_respects_hosts_deny() {
    let def = ModuleDefinition {
        hosts_deny: vec![HostPattern::Ipv4 {
            network: Ipv4Addr::new(10, 0, 0, 0),
            prefix: 8,
        }],
        ..Default::default()
    };
    let allowed = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
    let denied = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    assert!(def.permits(allowed, None));
    assert!(!def.permits(denied, None));
}

#[test]
fn module_definition_allow_match_short_circuits_deny() {
    // upstream: access.c:277-279 - "If we match an allow-list item, we
    // always allow access." A peer matching any allow pattern is admitted
    // before the deny list is consulted, even when a deny pattern would
    // otherwise match.
    let def = ModuleDefinition {
        hosts_allow: vec![HostPattern::Any],
        hosts_deny: vec![HostPattern::Ipv4 {
            network: Ipv4Addr::new(10, 0, 0, 0),
            prefix: 8,
        }],
        ..Default::default()
    };
    let peer = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    assert!(def.permits(peer, None));
}

#[test]
fn module_definition_deny_applies_when_allow_does_not_match() {
    // upstream: access.c:281-291 - when the allow list is non-empty but
    // the peer matches none of its entries, fall through to the deny list.
    // A deny-list match here refuses the connection; a non-match admits
    // (access.c:290-291 "Allow all other access").
    let def = ModuleDefinition {
        hosts_allow: vec![HostPattern::Ipv4 {
            network: Ipv4Addr::new(192, 168, 0, 0),
            prefix: 16,
        }],
        hosts_deny: vec![HostPattern::Ipv4 {
            network: Ipv4Addr::new(10, 0, 0, 0),
            prefix: 8,
        }],
        ..Default::default()
    };
    let denied = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    assert!(!def.permits(denied, None));

    // Peer outside both allow and deny: admitted because access.c:287
    // returns 0 only on a deny-list match; otherwise access.c:291 allows.
    // The allow-list non-match short-circuits to refuse only when the
    // deny list is empty (access.c:281-282).
    let outside_both = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1));
    assert!(def.permits(outside_both, None));
}

#[test]
fn module_definition_allow_short_circuit_skips_dns_fail_closed_guard() {
    // upstream: access.c:277-283 - an allow-list match returns 1 before
    // the deny list is consulted. A hostname-pattern deny rule combined
    // with unresolvable reverse DNS must not refuse a peer that already
    // matched an IP-based allow rule, because upstream never reaches the
    // deny path in that case. Without the short-circuit the GHSA-rjfm
    // fail-closed guard would punish a perfectly-allowed peer for a
    // separate hostname-deny rule that targets a different host.
    let def = ModuleDefinition {
        hosts_allow: vec![HostPattern::Ipv4 {
            network: Ipv4Addr::new(192, 168, 0, 0),
            prefix: 16,
        }],
        hosts_deny: vec![HostPattern::Hostname(HostnamePattern {
            kind: HostnamePatternKind::Suffix("bad.example".to_owned()),
        })],
        ..Default::default()
    };
    let allowed = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50));
    assert!(def.permits(allowed, None));
}

#[test]
fn module_definition_matches_upstream_rsync_fns_allow_list() {
    // upstream: testsuite/rsync.fns:381 - the testsuite's standard daemon
    // config carries `hosts allow = localhost 127.0.0.0/24 192.168.0.0/16
    // 10.0.0.0/8 $hostname` with no `hosts deny`. Every IPv4 in those
    // ranges must be admitted; every IPv4 outside must be refused. This
    // pins the CIDR matcher against upstream's testsuite expectations.
    let def = ModuleDefinition {
        hosts_allow: vec![
            HostPattern::Ipv4 {
                network: Ipv4Addr::new(127, 0, 0, 0),
                prefix: 24,
            },
            HostPattern::Ipv4 {
                network: Ipv4Addr::new(192, 168, 0, 0),
                prefix: 16,
            },
            HostPattern::Ipv4 {
                network: Ipv4Addr::new(10, 0, 0, 0),
                prefix: 8,
            },
        ],
        ..Default::default()
    };
    for ip in [
        Ipv4Addr::new(127, 0, 0, 1),
        Ipv4Addr::new(127, 0, 0, 255),
        Ipv4Addr::new(192, 168, 1, 1),
        Ipv4Addr::new(192, 168, 255, 254),
        Ipv4Addr::new(10, 0, 0, 1),
        Ipv4Addr::new(10, 255, 255, 254),
    ] {
        assert!(
            def.permits(IpAddr::V4(ip), None),
            "{ip} must be permitted by testsuite allow list",
        );
    }
    for ip in [
        Ipv4Addr::new(127, 0, 1, 1),
        Ipv4Addr::new(11, 0, 0, 1),
        Ipv4Addr::new(192, 169, 0, 1),
        Ipv4Addr::new(203, 0, 113, 5),
    ] {
        assert!(
            !def.permits(IpAddr::V4(ip), None),
            "{ip} must be refused by testsuite allow list",
        );
    }
}

#[test]
fn module_definition_requires_hostname_lookup_when_hostname_pattern() {
    let def = ModuleDefinition {
        hosts_allow: vec![HostPattern::Hostname(HostnamePattern {
            kind: HostnamePatternKind::Suffix("example.com".to_owned()),
        })],
        ..Default::default()
    };
    assert!(def.requires_hostname_lookup());
}

#[test]
fn module_definition_no_hostname_lookup_for_ip_patterns() {
    let def = ModuleDefinition {
        hosts_allow: vec![HostPattern::Ipv4 {
            network: Ipv4Addr::new(192, 168, 0, 0),
            prefix: 16,
        }],
        ..Default::default()
    };
    assert!(!def.requires_hostname_lookup());
}

#[test]
fn module_definition_requires_authentication_when_auth_users_set() {
    let def = ModuleDefinition {
        auth_users: vec![AuthUser::new("alice".to_owned())],
        ..Default::default()
    };
    assert!(def.requires_authentication());
}

#[test]
fn module_definition_no_authentication_when_no_auth_users() {
    let def = ModuleDefinition::default();
    assert!(!def.requires_authentication());
}

#[test]
fn module_definition_inherit_refuse_options() {
    let mut def = ModuleDefinition::default();
    let options = vec!["delete".to_owned(), "delete-after".to_owned()];
    def.inherit_refuse_options(&options);
    assert_eq!(def.refuse_options, options);
}

#[test]
fn module_definition_inherit_refuse_options_preserves_existing() {
    let mut def = ModuleDefinition {
        refuse_options: vec!["hardlinks".to_owned()],
        ..Default::default()
    };
    let options = vec!["delete".to_owned()];
    def.inherit_refuse_options(&options);
    assert_eq!(def.refuse_options, vec!["hardlinks".to_owned()]);
}

#[test]
fn module_definition_inherit_chmod() {
    let mut def = ModuleDefinition::default();
    def.inherit_incoming_chmod(Some("Dg+s,ug+w"));
    def.inherit_outgoing_chmod(Some("Fo-w,+X"));
    assert_eq!(def.incoming_chmod.as_deref(), Some("Dg+s,ug+w"));
    assert_eq!(def.outgoing_chmod.as_deref(), Some("Fo-w,+X"));
}

#[test]
fn module_definition_inherit_chmod_preserves_existing() {
    let mut def = ModuleDefinition {
        incoming_chmod: Some("existing".to_owned()),
        outgoing_chmod: Some("existing".to_owned()),
        ..Default::default()
    };
    def.inherit_incoming_chmod(Some("new"));
    def.inherit_outgoing_chmod(Some("new"));
    assert_eq!(def.incoming_chmod.as_deref(), Some("existing"));
    assert_eq!(def.outgoing_chmod.as_deref(), Some("existing"));
}

#[test]
fn module_definition_bandwidth_accessors() {
    let def = ModuleDefinition {
        bandwidth_limit: NonZeroU64::new(1000),
        bandwidth_limit_specified: true,
        bandwidth_burst: NonZeroU64::new(2000),
        bandwidth_burst_specified: true,
        bandwidth_limit_configured: true,
        ..Default::default()
    };
    assert_eq!(def.bandwidth_limit(), NonZeroU64::new(1000));
    assert!(def.bandwidth_limit_specified());
    assert_eq!(def.bandwidth_burst(), NonZeroU64::new(2000));
    assert!(def.bandwidth_burst_specified());
    assert!(def.bandwidth_limit_configured());
}

#[test]
fn module_definition_max_connections() {
    let def = ModuleDefinition {
        max_connections: NonZeroU32::new(10),
        ..Default::default()
    };
    assert_eq!(def.max_connections(), NonZeroU32::new(10));
}

#[test]
fn module_runtime_from_definition() {
    let def = ModuleDefinition {
        name: "test".to_owned(),
        path: PathBuf::from("/test"),
        ..Default::default()
    };
    let runtime: ModuleRuntime = def.into();
    assert_eq!(runtime.definition.name, "test");
}

#[test]
fn module_runtime_deref_to_definition() {
    let def = ModuleDefinition {
        name: "deref_test".to_owned(),
        ..Default::default()
    };
    let runtime: ModuleRuntime = def.into();
    assert_eq!(runtime.name, "deref_test");
}

#[test]
fn module_runtime_requires_authentication() {
    let def = ModuleDefinition {
        auth_users: vec![AuthUser::new("user".to_owned())],
        ..Default::default()
    };
    let runtime: ModuleRuntime = def.into();
    assert!(runtime.requires_authentication());
}

#[test]
fn module_connection_error_io() {
    let io_err = io::Error::new(io::ErrorKind::NotFound, "test");
    let err = ModuleConnectionError::io(io_err);
    match err {
        ModuleConnectionError::Io(_) => (),
        ModuleConnectionError::Limit(_) => panic!("Expected Io variant"),
    }
}

#[test]
fn module_connection_error_from_io() {
    let io_err = io::Error::new(io::ErrorKind::PermissionDenied, "test");
    let err: ModuleConnectionError = io_err.into();
    match err {
        ModuleConnectionError::Io(_) => (),
        ModuleConnectionError::Limit(_) => panic!("Expected Io variant"),
    }
}

#[test]
fn module_connection_error_debug() {
    let limit = NonZeroU32::new(5).unwrap();
    let err = ModuleConnectionError::Limit(limit);
    let debug = format!("{err:?}");
    assert!(debug.contains("Limit"));
}

#[test]
fn module_connection_guard_unlimited() {
    let guard = ModuleConnectionGuard::unlimited();
    assert!(guard.module.is_none());
    assert!(guard.lock_guard.is_none());
}

#[test]
fn normalize_hostname_removes_trailing_dot() {
    let result = hostname::normalize_hostname_owned("example.com.".to_owned());
    assert_eq!(result, "example.com");
}

#[test]
fn normalize_hostname_lowercases() {
    let result = hostname::normalize_hostname_owned("EXAMPLE.COM".to_owned());
    assert_eq!(result, "example.com");
}

#[test]
fn normalize_hostname_combined() {
    let result = hostname::normalize_hostname_owned("Example.COM.".to_owned());
    assert_eq!(result, "example.com");
}

#[test]
fn module_peer_hostname_returns_none_when_lookup_disabled() {
    let def = ModuleDefinition {
        hosts_allow: vec![HostPattern::Hostname(HostnamePattern {
            kind: HostnamePatternKind::Suffix("example.com".to_owned()),
        })],
        ..Default::default()
    };
    let mut cache = None;
    let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let result = module_peer_hostname(&def, &mut cache, addr, false);
    assert!(result.is_none());
}

#[test]
fn module_peer_hostname_returns_none_when_no_hostname_patterns() {
    let def = ModuleDefinition {
        hosts_allow: vec![HostPattern::Any],
        ..Default::default()
    };
    let mut cache = None;
    let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let result = module_peer_hostname(&def, &mut cache, addr, true);
    assert!(result.is_none());
}

#[test]
fn module_peer_hostname_uses_cache() {
    let def = ModuleDefinition {
        hosts_allow: vec![HostPattern::Hostname(HostnamePattern {
            kind: HostnamePatternKind::Suffix("example.com".to_owned()),
        })],
        ..Default::default()
    };
    let mut cache = Some(Some("cached.example.com".to_owned()));
    let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let result = module_peer_hostname(&def, &mut cache, addr, true);
    assert_eq!(result, Some("cached.example.com"));
}
