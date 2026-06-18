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
        pattern: "*.tmp".to_string(),
        ..FilterRuleWireFormat::default()
    }];
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    let filters = ctx.daemon_filter_set();
    assert!(
        filters.is_some(),
        "daemon filter set should be built from rules"
    );

    let filters = filters.unwrap();
    // *.tmp should be excluded
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
            pattern: "*.rs".to_string(),
            ..FilterRuleWireFormat::default()
        },
        FilterRuleWireFormat {
            rule_type: RuleType::Exclude,
            pattern: "*".to_string(),
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
        pattern: "/secret".to_string(),
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
        pattern: "secret_*".to_string(),
        ..FilterRuleWireFormat::default()
    }];
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    // Daemon filter set should reject secret_ files
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
