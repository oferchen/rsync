use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::num::NonZeroU32;
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
    assert!(def.permits(addr, PeerHost::new(None, true)));
    assert!(def.permits(addr, PeerHost::new(Some("example.com"), true)));
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
    assert!(def.permits(allowed, PeerHost::new(None, true)));
    assert!(!def.permits(denied, PeerHost::new(None, true)));
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
    assert!(def.permits(allowed, PeerHost::new(None, true)));
    assert!(!def.permits(denied, PeerHost::new(None, true)));
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
    assert!(def.permits(peer, PeerHost::new(None, true)));
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
    assert!(!def.permits(denied, PeerHost::new(None, true)));

    // Peer outside both allow and deny: admitted because access.c:287
    // returns 0 only on a deny-list match; otherwise access.c:291 allows.
    // The allow-list non-match short-circuits to refuse only when the
    // deny list is empty (access.c:281-282).
    let outside_both = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1));
    assert!(def.permits(outside_both, PeerHost::new(None, true)));
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
    assert!(def.permits(allowed, PeerHost::new(None, true)));
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
            def.permits(IpAddr::V4(ip), PeerHost::new(None, true)),
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
            !def.permits(IpAddr::V4(ip), PeerHost::new(None, true)),
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
fn module_definition_max_connections() {
    let def = ModuleDefinition {
        max_connections: MaxConnections::Limited(NonZeroU32::new(10).expect("non-zero")),
        ..Default::default()
    };
    assert_eq!(
        def.max_connections(),
        MaxConnections::Limited(NonZeroU32::new(10).expect("non-zero"))
    );
}

#[test]
fn module_definition_max_connections_defaults_to_unlimited() {
    // upstream: loadparm.c gives `max connections` a default of 0, and
    // connection.c:claim_connection:27 returns success for 0 without taking a
    // lock, so a module that never sets the directive is unlimited.
    assert_eq!(
        ModuleDefinition::default().max_connections(),
        MaxConnections::Unlimited
    );
}

#[test]
fn negative_max_connections_refuses_every_connection() {
    // upstream: connection.c:33 `for (i = 0; i < max_connections; i++)` cannot
    // run for a negative limit, so claim_connection falls through to
    // `errno = 0; return 0` and clientserver.c:746-757 refuses with the
    // configured number echoed verbatim. rsyncd.conf.5: "A negative value
    // disables the module". Clamping the sign to zero would invert this into
    // "serve the module with no limit at all", so the refusal - and the sign
    // carried into the diagnostic - is the behaviour under test.
    let def = ModuleDefinition {
        name: "disabled".to_owned(),
        max_connections: MaxConnections::Disabled(-1),
        ..Default::default()
    };
    let runtime: ModuleRuntime = def.into();

    match runtime.try_acquire_connection() {
        Err(ModuleConnectionError::Limit(limit)) => assert_eq!(limit, -1),
        _ => panic!("a disabled module must refuse the first connection"),
    }

    // The refusal never enters upstream's slot loop, so no slot is consumed.
    assert_eq!(runtime.active_connections.load(Ordering::Acquire), 0);
}

#[test]
fn negative_max_connections_still_opens_the_lock_file_first() {
    // upstream: connection.c:31 opens the lock file for every non-zero limit
    // and only reaches the "max connections (%d) reached" fall-through at
    // connection.c:44-46 afterwards. The two failures are distinguishable on
    // the wire - an unopenable lock file reports "failed to open lock file" -
    // so a disabled module must not short-circuit ahead of the open.
    //
    // ⚠ The unopenable condition is a DIRECTORY at the lock path, not a
    // missing file. Upstream opens `O_RDWR|O_CREAT` on every claim
    // (connection.c:35), so a merely absent lock file is recreated rather than
    // reported - deleting it would silently stop detecting the short-circuit
    // this test exists to catch. A directory fails `EISDIR` even with
    // `O_CREAT`, so the detector survives the create. Do not "simplify" this
    // back to `remove_file`.
    let dir = tempfile::tempdir().expect("tempdir");
    let lock_path = dir.path().join("module.lock");
    let limiter =
        std::sync::Arc::new(ConnectionLimiter::open(lock_path.clone()).expect("lock file"));
    std::fs::remove_file(&lock_path).expect("remove lock file");
    std::fs::create_dir(&lock_path).expect("obstruct the lock path with a directory");

    let def = ModuleDefinition {
        name: "disabled".to_owned(),
        max_connections: MaxConnections::Disabled(-1),
        ..Default::default()
    };
    let runtime = ModuleRuntime::new(def, Some(limiter));

    match runtime.try_acquire_connection() {
        Err(ModuleConnectionError::Io(_)) => (),
        _ => panic!("a missing lock file must be reported as an I/O failure"),
    }
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
    let err = ModuleConnectionError::Limit(5);
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
        max_connections: MaxConnections::Limited(limit),
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

// WHY: every daemon entry point - the accept loop, the async listener, the
// `--server` stdio session and the inetd session - builds its module table
// through this one helper. Three of them previously hand-rolled the same two
// steps and passed a hardcoded `None`, silently disabling `max connections`
// on those transports. Opening the daemon-wide lock file inside the helper is
// what makes that spelling unavailable to a future entry point.
//
// The second assertion is the one that discriminates: an implementation that
// ignores `lock_file` and returns `None` still satisfies the first arm.
//
// upstream: clientserver.c:791 - `rsync_module()` calls `claim_connection()`
// for every daemon connection regardless of how it arrived.
#[test]
fn build_module_runtimes_with_lock_file_shares_the_daemon_wide_limiter() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lock = dir.path().join("daemon.lock");

    let def = ModuleDefinition {
        name: "shared".to_owned(),
        ..Default::default()
    };

    // No `lock file` configured: nothing is opened, matching upstream's
    // `if (max_connections == 0) return 1` short-circuit ahead of the open.
    let (runtimes, limiter) =
        build_module_runtimes_with_lock_file(vec![def.clone()], None).expect("build without lock");
    assert!(limiter.is_none(), "no lock file must open no limiter");
    assert!(runtimes[0].connection_limiter.is_none());

    // A daemon-wide `lock file` reaches a module that has no override.
    let (runtimes, limiter) = build_module_runtimes_with_lock_file(vec![def], Some(lock.clone()))
        .expect("build with lock");
    let limiter = limiter.expect("daemon-wide limiter opened");
    assert!(
        std::sync::Arc::ptr_eq(
            runtimes[0]
                .connection_limiter
                .as_ref()
                .expect("module limiter"),
            &limiter,
        ),
        "a module without its own `lock file` must share the daemon-wide limiter",
    );
    assert!(lock.exists(), "the daemon-wide lock file must be created");
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

// WHY: only a module that carries a resolved syslog tag/facility should reopen
// the process-wide syslog handle for its connection; a module inheriting the
// daemon-global logger must leave it untouched (returns None) so the startup
// tag/facility keep serving. upstream: log.c:169 log_init reopens per module.
#[cfg(unix)]
#[test]
fn reconfigure_syslog_only_when_module_sets_a_value() {
    let inherit = ModuleDefinition::default();
    assert!(
        inherit.reconfigure_syslog().is_none(),
        "a module with no syslog override must not reconfigure the shared handle"
    );

    let with_facility = ModuleDefinition {
        syslog_facility: Some("local4".to_owned()),
        ..ModuleDefinition::default()
    };
    let guard = with_facility.reconfigure_syslog();
    assert!(
        guard.is_some(),
        "a module with a syslog facility must reconfigure syslog"
    );
    drop(guard);

    let with_tag = ModuleDefinition {
        syslog_tag: Some("mytag".to_owned()),
        ..ModuleDefinition::default()
    };
    assert!(with_tag.reconfigure_syslog().is_some());
}
