use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::num::{NonZeroU32, NonZeroU64};
use std::path::PathBuf;
use std::sync::atomic::Ordering;

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
            original: ".bad.example".to_owned(),
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
            original: ".example.com".to_owned(),
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
fn aborted_transfer_releases_connection_slot() {
    // Regression for #504 (the deadlock-holds-slot symptom of #503).
    //
    // A daemon transfer holds its connection slot via the RAII
    // `ModuleConnectionGuard` acquired in `process_approved_module`. When a
    // transfer fails or aborts, the guard drops on unwind and must release the
    // slot so the module keeps accepting new connections. Before #503 was
    // fixed, a deadlocked connection thread never unwound, so its guard never
    // dropped: four wedged connections exhausted a `max connections = 4`
    // module. This test pins the invariant that a slot acquired and then
    // released (the drop that a failed/aborted transfer performs) frees the
    // module for a fresh connection - so even N aborted transfers never wedge
    // the module at its limit.
    let limit = NonZeroU32::new(4).unwrap();
    let def = ModuleDefinition {
        name: "abort_release".to_owned(),
        max_connections: Some(limit),
        ..Default::default()
    };
    let runtime: ModuleRuntime = def.into();

    // Simulate five sequential failed/aborted transfers on a 4-slot module.
    // Each acquisition must succeed because the previous guard was dropped
    // (as it would be when a transfer returns Err or the thread unwinds).
    for _ in 0..5 {
        let guard = runtime
            .try_acquire_connection()
            .expect("slot must be free after the prior aborted transfer released it");
        assert_eq!(runtime.active_connections.load(Ordering::Acquire), 1);
        // Dropping the guard is exactly what a failed/aborted transfer does.
        drop(guard);
        assert_eq!(
            runtime.active_connections.load(Ordering::Acquire),
            0,
            "aborted transfer must release its connection slot"
        );
    }

    // Fill every slot, confirm the limit is enforced, then release one and
    // confirm a new connection is admitted - the module never stays wedged.
    let mut guards = Vec::new();
    for _ in 0..limit.get() {
        guards.push(
            runtime
                .try_acquire_connection()
                .expect("slots below the limit must be acquirable"),
        );
    }
    assert!(
        matches!(
            runtime.try_acquire_connection(),
            Err(ModuleConnectionError::Limit(_))
        ),
        "the module must refuse once the limit is reached"
    );
    guards.pop();
    runtime
        .try_acquire_connection()
        .expect("releasing a slot must let a new connection in");
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
            original: ".example.com".to_owned(),
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
            original: ".example.com".to_owned(),
        })],
        ..Default::default()
    };
    let mut cache = Some(Some("cached.example.com".to_owned()));
    let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let result = module_peer_hostname(&def, &mut cache, addr, true);
    assert_eq!(result, Some("cached.example.com"));
}

// upstream: clientserver.c:746 `claim_connection(lp_lock_file(i), ...)` - the
// lock file is P_LOCAL, so a module that sets its own `lock file` claims slots
// in that file while modules without an override share the daemon-wide file.
#[test]
fn build_module_runtimes_honours_per_module_lock_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let global = std::sync::Arc::new(
        ConnectionLimiter::open(dir.path().join("global.lock")).expect("global lock"),
    );
    let own = dir.path().join("own.lock");

    let shared_def = ModuleDefinition {
        name: "shared".to_owned(),
        ..Default::default()
    };
    let own_def = ModuleDefinition {
        name: "own".to_owned(),
        lock_file: Some(own.clone()),
        ..Default::default()
    };
    let own_twin = ModuleDefinition {
        name: "twin".to_owned(),
        lock_file: Some(own.clone()),
        ..Default::default()
    };

    let runtimes = build_module_runtimes(
        vec![shared_def, own_def, own_twin],
        &Some(std::sync::Arc::clone(&global)),
    )
    .expect("build runtimes");

    // A module without an override shares the daemon-wide limiter.
    assert!(std::sync::Arc::ptr_eq(
        runtimes[0]
            .connection_limiter
            .as_ref()
            .expect("shared limiter"),
        &global,
    ));
    // A module with its own lock file gets a distinct limiter.
    assert!(!std::sync::Arc::ptr_eq(
        runtimes[1]
            .connection_limiter
            .as_ref()
            .expect("own limiter"),
        &global,
    ));
    // Two modules naming the same lock file share one handle.
    assert!(std::sync::Arc::ptr_eq(
        runtimes[1]
            .connection_limiter
            .as_ref()
            .expect("own limiter"),
        runtimes[2]
            .connection_limiter
            .as_ref()
            .expect("twin limiter"),
    ));
}

// upstream: clientserver.c:723 - when the global default disables reverse
// lookup (host stays undetermined), a module that enables it resolves the peer
// via `lp_reverse_lookup(i)`. The call site computes the effective value as
// `global || module.reverse_lookup`; this proves the module override reaches
// the resolver while an unset/disabled module inherits the disabled global.
#[test]
fn per_module_reverse_lookup_gates_resolution_when_global_disabled() {
    let addr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));
    set_test_hostname_override(addr, Some("host.example.com"));
    set_test_forward_override("host.example.com", &[addr]);

    let hosts_allow = vec![HostPattern::Hostname(HostnamePattern {
        kind: HostnamePatternKind::Suffix("example.com".to_owned()),
        original: ".example.com".to_owned(),
    })];
    let global_reverse_lookup = false;

    let enabled = ModuleDefinition {
        hosts_allow: hosts_allow.clone(),
        reverse_lookup: true,
        forward_lookup: true,
        ..Default::default()
    };
    let mut cache = None;
    let effective = global_reverse_lookup || enabled.reverse_lookup;
    assert_eq!(
        module_peer_hostname(&enabled, &mut cache, addr, effective),
        Some("host.example.com"),
    );

    let disabled = ModuleDefinition {
        hosts_allow,
        reverse_lookup: false,
        forward_lookup: true,
        ..Default::default()
    };
    let mut cache = None;
    let effective = global_reverse_lookup || disabled.reverse_lookup;
    assert_eq!(
        module_peer_hostname(&disabled, &mut cache, addr, effective),
        None
    );

    clear_test_hostname_overrides();
}
