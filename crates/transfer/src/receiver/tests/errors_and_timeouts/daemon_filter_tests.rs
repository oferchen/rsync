//! Daemon-side filter rules applied from `daemon_filter_rules`. Verifies
//! that the receiver builds a `FilterSet` from the wire-format rules
//! prepended at daemon negotiation time and that include/exclude,
//! anchored, and pure-exclude patterns match upstream rsync semantics.

use super::super::super::ReceiverContext;
use super::super::support::{test_config, test_handshake};

#[test]
fn daemon_filter_set_empty_when_no_rules() {
    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    assert!(ctx.daemon_filter_set().is_none());
}

#[test]
fn daemon_filter_set_built_from_config_rules() {
    use protocol::filters::{FilterRuleWireFormat, RuleType};

    let handshake = test_handshake();
    let mut config = test_config();
    config.daemon_filter_rules = vec![FilterRuleWireFormat {
        rule_type: RuleType::Exclude,
        pattern: "*.tmp".into(),
        ..FilterRuleWireFormat::default()
    }];
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    let filters = ctx.daemon_filter_set();
    assert!(
        filters.is_some(),
        "daemon filter set should be built from rules"
    );

    let filters = filters.unwrap();
    assert!(
        !filters.allows(std::path::Path::new("test.tmp"), false),
        "*.tmp should be excluded by daemon filter"
    );
    // *.txt should be allowed (no matching rule)
    assert!(
        filters.allows(std::path::Path::new("test.txt"), false),
        "*.txt should be allowed through daemon filter"
    );
}

#[test]
fn daemon_filter_set_include_and_exclude() {
    use protocol::filters::{FilterRuleWireFormat, RuleType};

    let handshake = test_handshake();
    let mut config = test_config();
    config.daemon_filter_rules = vec![
        FilterRuleWireFormat {
            rule_type: RuleType::Include,
            pattern: "*.rs".into(),
            ..FilterRuleWireFormat::default()
        },
        FilterRuleWireFormat {
            rule_type: RuleType::Exclude,
            pattern: "*".into(),
            ..FilterRuleWireFormat::default()
        },
    ];
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    let filters = ctx.daemon_filter_set().unwrap();
    // *.rs should be included (explicit include before wildcard exclude)
    assert!(
        filters.allows(std::path::Path::new("main.rs"), false),
        "*.rs should be included by daemon filter"
    );
    // *.txt should be excluded (wildcard exclude)
    assert!(
        !filters.allows(std::path::Path::new("readme.txt"), false),
        "*.txt should be excluded by daemon filter"
    );
}

#[test]
fn daemon_filter_set_anchored_pattern() {
    use protocol::filters::{FilterRuleWireFormat, RuleType};

    let handshake = test_handshake();
    let mut config = test_config();
    config.daemon_filter_rules = vec![FilterRuleWireFormat {
        rule_type: RuleType::Exclude,
        pattern: "/secret".into(),
        anchored: true,
        ..FilterRuleWireFormat::default()
    }];
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    let filters = ctx.daemon_filter_set().unwrap();
    // /secret should be excluded (anchored)
    assert!(
        !filters.allows(std::path::Path::new("secret"), false),
        "anchored /secret should be excluded"
    );
    // nested/secret should be allowed (anchored patterns only match at root)
    assert!(
        filters.allows(std::path::Path::new("nested/secret"), false),
        "nested/secret should be allowed (anchored only matches root)"
    );
}

#[test]
fn daemon_filter_rules_prepended_to_receiver_deletion_chain() {
    // Verify that daemon_filter_rules from config are prepended to
    // wire rules when building the filter chain for deletion.
    // This is tested indirectly by verifying the daemon_filter_set
    // is available and that the setup_transfer code path handles
    // the daemon_filter_rules field.
    use protocol::filters::{FilterRuleWireFormat, RuleType};

    let handshake = test_handshake();
    let mut config = test_config();
    config.daemon_filter_rules = vec![FilterRuleWireFormat {
        rule_type: RuleType::Exclude,
        pattern: "secret_*".into(),
        ..FilterRuleWireFormat::default()
    }];
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    let filters = ctx.daemon_filter_set().unwrap();
    assert!(
        !filters.allows(std::path::Path::new("secret_data.bin"), false),
        "secret_data.bin should be excluded by daemon filter"
    );
    assert!(
        filters.allows(std::path::Path::new("public_data.bin"), false),
        "public_data.bin should be allowed through daemon filter"
    );
}

/// Upstream reports a refused directory once and then sets `skip_dir`, so every
/// entry below it leaves `recv_generator()` before the filter check is reached
/// (`generator.c:1258-1266`). The ancestor probe reproduces that: it is what
/// keeps oc from emitting a second "daemon refused" line - one upstream never
/// prints - for each file inside an already-refused directory.
#[test]
fn refused_directory_swallows_its_contents_without_a_second_report() {
    use protocol::filters::{FilterRuleWireFormat, RuleType};

    use crate::receiver::daemon_filter_refuses_ancestor;

    let handshake = test_handshake();
    let mut config = test_config();
    config.daemon_filter_rules = vec![FilterRuleWireFormat {
        rule_type: RuleType::Exclude,
        pattern: "*.secret".into(),
        ..FilterRuleWireFormat::default()
    }];
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    let filters = ctx.daemon_filter_set().unwrap();

    assert!(
        daemon_filter_refuses_ancestor(filters, "dir.secret/inner.txt"),
        "a file under a refused directory is dropped silently, not reported again"
    );
    assert!(
        daemon_filter_refuses_ancestor(filters, "dir.secret/deep/inner.txt"),
        "the skip applies at every depth below the refused directory"
    );
    assert!(
        !daemon_filter_refuses_ancestor(filters, "sub/nested.secret"),
        "the entry's own name is the outer refusal's business, not the ancestor probe's"
    );
    assert!(
        !daemon_filter_refuses_ancestor(filters, "top.secret"),
        "a top-level entry has no ancestor to inherit a refusal from"
    );
    assert!(
        !daemon_filter_refuses_ancestor(filters, "sub/fine.txt"),
        "an allowed tree must not be swallowed"
    );
}

/// Builds a receiver whose daemon module excludes `secret`, the shape the
/// `operator-path-traversal-*-daemon` conformance cells use.
fn ctx_excluding(pattern: &str) -> ReceiverContext {
    use protocol::filters::{FilterRuleWireFormat, RuleType};

    let handshake = test_handshake();
    let mut config = test_config();
    config.daemon_filter_rules = vec![FilterRuleWireFormat {
        rule_type: RuleType::Exclude,
        pattern: pattern.into(),
        ..FilterRuleWireFormat::default()
    }];
    ReceiverContext::new_for_test(&handshake, config)
}

fn refusal(ctx: &ReceiverContext, dest: &str) -> Option<String> {
    ctx.reject_daemon_excluded_destination(std::path::Path::new(dest))
        .err()
        .map(|e| e.to_string())
}

/// THE ESCAPE (task 819): on a `path = /` module the daemon `exclude` is the
/// only thing stopping a peer-supplied `..` traversal, because upstream's
/// `abspath_outside_confinement` short-circuits at `rootlen <= 1`
/// (`syscall.c:206-207`). The name check must therefore run on the collapsed
/// path, so `pub/../secret` is refused exactly as a bare `secret` is.
///
/// upstream: `main.c:718-737` `get_local_name()`.
#[test]
fn daemon_excluded_destination_is_refused_after_dot_dot_collapse() {
    let ctx = ctx_excluding("secret");

    let direct = refusal(&ctx, "secret").expect("a directly excluded dest must be refused");
    assert!(
        direct.contains("daemon has excluded destination"),
        "upstream wording expected, got: {direct}"
    );

    let traversed =
        refusal(&ctx, "pub/../secret").expect("a '..' traversal into the excluded subtree escapes");
    assert!(
        traversed.contains("daemon has excluded destination"),
        "upstream wording expected, got: {traversed}"
    );
    // upstream quotes the ORIGINAL argument, not the collapsed copy.
    assert!(
        traversed.contains("\"pub/../secret\""),
        "the original operand must be quoted, got: {traversed}"
    );
}

/// Non-vacuity companion: without this the refusal test would still pass if
/// the check simply refused everything.
#[test]
fn daemon_allows_a_destination_no_rule_excludes() {
    let ctx = ctx_excluding("secret");
    assert!(
        refusal(&ctx, "public/data").is_none(),
        "an unexcluded destination must be accepted"
    );
    assert!(
        refusal(&ctx, ".").is_none(),
        "upstream skips the check entirely for a bare '.'"
    );
}

/// A receiver with no daemon filter must not gain a new refusal path.
#[test]
fn destination_check_is_inert_without_a_daemon_filter() {
    let handshake = test_handshake();
    let ctx = ReceiverContext::new_for_test(&handshake, test_config());
    assert!(refusal(&ctx, "anything/at/all").is_none());
}

/// The trailing-slash rule form must keep its `XFLG_DIR2WILD3` meaning
/// (`exclude.c:308-313`): `/excluded/` becomes `<pat>/***`, matching the
/// directory and its contents. Testing BOTH `is_dir` values is why upstream
/// ORs its two `check_filter()` calls - a dir-only rule is invisible to a
/// file-only probe.
#[test]
fn daemon_excluded_destination_honours_dir_only_rules() {
    let ctx = ctx_excluding("excluded/");
    assert!(
        refusal(&ctx, "excluded").is_some(),
        "a dir-only rule must still refuse the directory itself"
    );
}

/// Builds a receiver whose module excludes `secret` and whose client asked for
/// `basis` as an alternate-basis directory.
///
/// `requested` is what the peer wrote; `path` is set to the resolved shape the
/// daemon's module-root confinement produces, so the fixture reproduces the
/// real divergence between the two fields rather than assuming they agree.
fn ctx_with_basis(requested: &str) -> ReceiverContext {
    use engine::local_copy::{ReferenceDirectory, ReferenceDirectoryKind};
    use protocol::filters::{FilterRuleWireFormat, RuleType};

    let handshake = test_handshake();
    let mut config = test_config();
    // ANCHORED, as a module `exclude = /secret` is. An UNANCHORED rule would
    // match the basename at any depth and so match the resolved path too -
    // making the fixture unable to tell `requested` from `path`, which is
    // exactly the bug under test.
    config.daemon_filter_rules = vec![FilterRuleWireFormat {
        rule_type: RuleType::Exclude,
        pattern: "secret".into(),
        anchored: true,
        ..FilterRuleWireFormat::default()
    }];
    config.connection.daemon_module_root = Some("/srv/mod".into());
    let mut entry = ReferenceDirectory::new(ReferenceDirectoryKind::Link, requested);
    // What the daemon's confinement pass leaves behind: the basis resolved
    // against the destination and confined under the module root. Relative to
    // the module that reads `dest/secret`, which an anchored rule cannot match.
    entry.path = std::path::PathBuf::from(format!("/srv/mod/dest/{requested}"));
    config.reference_directories = vec![entry];
    ReceiverContext::new_for_test(&handshake, config)
}

/// upstream: `main.c:1243-1270` - a daemon receiver runs every `basis_dir[]`
/// entry through the module filter list and refuses the whole session when one
/// matches, with `"Your options have been rejected by the server."` and
/// `RERR_SYNTAX`.
///
/// This is a READ barrier distinct from the destination check: `--copy-dest`
/// at an excluded directory copies excluded content into a destination the
/// client can then pull back.
///
/// ⚠ The match must use the name the peer WROTE. oc resolves and confines the
/// basis before the receiver sees it, so `path` reads `<module>/<dest>/secret`
/// and an anchored `secret` rule can never match it - which is exactly how this
/// check shipped inert the first time.
#[test]
fn daemon_excluded_basis_dir_is_refused_by_requested_name() {
    let ctx = ctx_with_basis("secret");
    let err = ctx
        .reject_daemon_excluded_basis_dirs()
        .expect_err("an excluded alternate-basis directory must refuse the session");

    assert_eq!(
        err.to_string(),
        "Your options have been rejected by the server.",
        "upstream's exact refusal text (main.c:1267)"
    );
    // `crates/transfer` sits below `crates/core`, so the ExitCode mapping
    // itself is pinned in `core::exit_code`. What is observable here is the
    // marker that selects it - without the tag the refusal would fall through
    // the mapper's catch-all to RERR_FILEIO (11) instead of upstream's 1.
    assert!(
        err.get_ref()
            .is_some_and(|inner| inner.is::<protocol::SyntaxViolation>()),
        "the refusal must carry the RERR_SYNTAX marker"
    );
}

/// Non-vacuity companion: with the SAME module filter, a basis the rules do not
/// exclude must pass. Without this a refusal that fired unconditionally - or a
/// fixture that could not produce a pass - would look identical.
#[test]
fn daemon_allows_a_basis_dir_the_module_does_not_exclude() {
    let ctx = ctx_with_basis("nosuch");
    assert!(
        ctx.reject_daemon_excluded_basis_dirs().is_ok(),
        "an allowed basis must not refuse the session"
    );
}

/// A receiver with no daemon filter list at all must never refuse: upstream
/// gates the whole block on `daemon_filter_list.head` (`main.c:1243`), so the
/// client and SSH-server receivers keep passing basis dirs through untouched.
#[test]
fn no_daemon_filter_list_never_refuses_a_basis_dir() {
    use engine::local_copy::{ReferenceDirectory, ReferenceDirectoryKind};

    let handshake = test_handshake();
    let mut config = test_config();
    config.reference_directories = vec![ReferenceDirectory::new(
        ReferenceDirectoryKind::Link,
        "secret",
    )];
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    assert!(
        ctx.reject_daemon_excluded_basis_dirs().is_ok(),
        "without a module filter list there is no rule to enforce"
    );
}
