//! Rule prefix builder matching upstream rsync format.
//!
//! Builds filter rule prefixes with all 13 modifier flags, respecting
//! protocol version compatibility.

use super::wire::FilterRuleWireFormat;
use crate::ProtocolVersion;

/// Builds rule prefix matching upstream format: `[+/-/:][/][!][C][n][w][e][x][s][r][p][ ]`
///
/// The prefix encodes the rule type and all active modifiers as a compact ASCII string.
/// Protocol version compatibility is enforced: v28 ignores `s`/`r`/`p` modifiers.
pub fn build_rule_prefix(rule: &FilterRuleWireFormat, protocol: ProtocolVersion) -> String {
    // Maximum prefix length: type(1) + modifiers(10) + space(1) = 12 chars
    let mut prefix = String::with_capacity(12);

    // First character: rule type
    prefix.push(rule.rule_type.prefix_char());

    // Modifiers (order matters for compatibility)
    if rule.anchored {
        prefix.push('/');
    }

    if rule.negate {
        prefix.push('!');
    }

    if rule.cvs_exclude {
        prefix.push('C');
    }

    if rule.no_inherit {
        prefix.push('n');
    }

    if rule.word_split {
        prefix.push('w');
    }

    if rule.exclude_from_merge {
        prefix.push('e');
    }

    if rule.xattr_only {
        prefix.push('x');
    }

    // Protocol version gated modifiers
    if protocol.supports_sender_receiver_modifiers() {
        if rule.sender_side {
            prefix.push('s');
        }
        if rule.receiver_side {
            prefix.push('r');
        }
    }

    if protocol.supports_perishable_modifier() && rule.perishable {
        prefix.push('p');
    }

    // Trailing space separator
    prefix.push(' ');

    prefix
}

#[cfg(test)]
mod tests {
    use super::super::wire::RuleType;
    use super::*;

    #[test]
    fn simple_exclude_prefix() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat::exclude("test".to_owned());

        let prefix = build_rule_prefix(&rule, protocol);
        assert_eq!(prefix, "- ");
    }

    #[test]
    fn simple_include_prefix() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat::include("test".to_owned());

        let prefix = build_rule_prefix(&rule, protocol);
        assert_eq!(prefix, "+ ");
    }

    #[test]
    fn anchored_modifier() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat::exclude("test".to_owned()).with_anchored(true);

        let prefix = build_rule_prefix(&rule, protocol);
        assert_eq!(prefix, "-/ ");
    }

    #[test]
    fn multiple_modifiers() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let mut rule = FilterRuleWireFormat::exclude("test".to_owned());
        rule.anchored = true;
        rule.no_inherit = true;
        rule.cvs_exclude = true;

        let prefix = build_rule_prefix(&rule, protocol);
        assert_eq!(prefix, "-/Cn ");
    }

    #[test]
    fn sender_side_v29() {
        let protocol = ProtocolVersion::from_supported(29).unwrap();
        let rule = FilterRuleWireFormat::exclude("test".to_owned()).with_sides(true, false);

        let prefix = build_rule_prefix(&rule, protocol);
        assert_eq!(prefix, "-s ");
    }

    #[test]
    fn receiver_side_v29() {
        let protocol = ProtocolVersion::from_supported(29).unwrap();
        let rule = FilterRuleWireFormat::exclude("test".to_owned()).with_sides(false, true);

        let prefix = build_rule_prefix(&rule, protocol);
        assert_eq!(prefix, "-r ");
    }

    #[test]
    fn both_sides_v29() {
        let protocol = ProtocolVersion::from_supported(29).unwrap();
        let rule = FilterRuleWireFormat::exclude("test".to_owned()).with_sides(true, true);

        let prefix = build_rule_prefix(&rule, protocol);
        assert_eq!(prefix, "-sr ");
    }

    #[test]
    fn perishable_v30() {
        let protocol = ProtocolVersion::from_supported(30).unwrap();
        let rule = FilterRuleWireFormat::exclude("test".to_owned()).with_perishable(true);

        let prefix = build_rule_prefix(&rule, protocol);
        assert_eq!(prefix, "-p ");
    }

    #[test]
    fn v28_strips_sender_receiver() {
        let protocol = ProtocolVersion::from_supported(28).unwrap();
        let rule = FilterRuleWireFormat::exclude("test".to_owned()).with_sides(true, true);

        let prefix = build_rule_prefix(&rule, protocol);
        // v28 doesn't support s/r, so they should be omitted
        assert_eq!(prefix, "- ");
    }

    #[test]
    fn v28_strips_perishable() {
        let protocol = ProtocolVersion::from_supported(28).unwrap();
        let rule = FilterRuleWireFormat::exclude("test".to_owned()).with_perishable(true);

        let prefix = build_rule_prefix(&rule, protocol);
        // v28 doesn't support p, so it should be omitted
        assert_eq!(prefix, "- ");
    }

    #[test]
    fn v29_supports_sender_receiver_but_not_perishable() {
        let protocol = ProtocolVersion::from_supported(29).unwrap();
        let rule = FilterRuleWireFormat::exclude("test".to_owned())
            .with_sides(true, true)
            .with_perishable(true);

        let prefix = build_rule_prefix(&rule, protocol);
        // v29 supports s/r but not p
        assert_eq!(prefix, "-sr ");
    }

    #[test]
    fn all_modifiers_v32() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let mut rule = FilterRuleWireFormat::exclude("test".to_owned());
        rule.anchored = true;
        rule.negate = true;
        rule.cvs_exclude = true;
        rule.no_inherit = true;
        rule.word_split = true;
        rule.exclude_from_merge = true;
        rule.xattr_only = true;
        rule.sender_side = true;
        rule.receiver_side = true;
        rule.perishable = true;

        let prefix = build_rule_prefix(&rule, protocol);
        assert_eq!(prefix, "-/!Cnwexsrp ");
    }

    #[test]
    fn clear_rule_prefix() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat {
            rule_type: RuleType::Clear,
            pattern: String::new(),
            anchored: false,
            directory_only: false,
            no_inherit: false,
            cvs_exclude: false,
            word_split: false,
            exclude_from_merge: false,
            xattr_only: false,
            sender_side: false,
            receiver_side: false,
            perishable: false,
            negate: false,
        };

        let prefix = build_rule_prefix(&rule, protocol);
        assert_eq!(prefix, ": ");
    }

    #[test]
    fn protect_rule_prefix() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat {
            rule_type: RuleType::Protect,
            pattern: "important".to_owned(),
            anchored: false,
            directory_only: false,
            no_inherit: false,
            cvs_exclude: false,
            word_split: false,
            exclude_from_merge: false,
            xattr_only: false,
            sender_side: false,
            receiver_side: false,
            perishable: false,
            negate: false,
        };

        let prefix = build_rule_prefix(&rule, protocol);
        assert_eq!(prefix, "P ");
    }
}
