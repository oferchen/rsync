//! Rule prefix builder matching upstream rsync format.
//!
//! Builds filter rule prefixes with all 13 modifier flags, respecting
//! protocol version compatibility.

use super::wire::FilterRuleWireFormat;
use crate::ProtocolVersion;

/// Builds rule prefix matching upstream format: `[+/-/:][/][!][C][n][w][e][x][s][r][p][ ]`
///
/// The prefix encodes the rule type and all active modifiers as a compact ASCII string.
/// Protocol version compatibility is enforced:
/// - Protocol < 29: Only `"+ "`, `"- "`, or empty prefix allowed (`legal_len = 1`).
///   Returns `None` for rules that cannot be represented (dir-merge, rules with modifiers).
/// - Protocol >= 29: Full modifier support; `s`/`r` gated at v29, `p` at v30.
///
/// # Returns
///
/// `Some(prefix)` on success, `None` if the rule cannot be serialized for this protocol.
///
/// # Upstream Reference
///
/// `exclude.c:1522-1587` - `get_rule_prefix()` returns NULL for unsendable rules
pub fn build_rule_prefix(rule: &FilterRuleWireFormat, protocol: ProtocolVersion) -> Option<String> {
    if protocol.uses_old_prefixes() {
        return build_old_prefix(rule);
    }
    Some(build_modern_prefix(rule, protocol))
}

/// Builds a prefix for protocol < 29 (old-style, `legal_len = 1`).
///
/// Only `"+ "` (include) and `"- "` (exclude) are valid. Dir-merge and
/// rules with any modifiers return `None` (unsendable).
///
/// # Upstream Reference
///
/// `exclude.c:1530-1582` - `legal_len = 1` branch
fn build_old_prefix(rule: &FilterRuleWireFormat) -> Option<String> {
    use super::wire::RuleType;

    // upstream: exclude.c:1532-1534 - dir-merge cannot be sent for proto < 29
    if matches!(rule.rule_type, RuleType::DirMerge) {
        return None;
    }

    // Check if any modifiers are set - they would exceed legal_len = 1
    let has_modifiers = rule.anchored
        || rule.negate
        || rule.cvs_exclude
        || rule.no_inherit
        || rule.word_split
        || rule.exclude_from_merge
        || rule.xattr_only
        || rule.sender_side
        || rule.receiver_side
        || rule.perishable;

    if has_modifiers {
        return None;
    }

    // Include rules always get "+ " prefix
    if matches!(rule.rule_type, RuleType::Include) {
        return Some("+ ".to_owned());
    }

    // Exclude rules: check if pattern needs disambiguation
    // upstream: exclude.c:1538 - only write "-" if pattern starts with "- " or "+ "
    if matches!(rule.rule_type, RuleType::Exclude) {
        let pat = &rule.pattern;
        let needs_prefix = (pat.starts_with("- ") || pat.starts_with("+ "))
            || matches!(
                rule.rule_type,
                RuleType::Protect | RuleType::Risk | RuleType::Merge | RuleType::Clear
            );
        if needs_prefix {
            return Some("- ".to_owned());
        }
        // No prefix needed - raw pattern only (legal_len set to 0)
        return Some(String::new());
    }

    // Other rule types (Protect, Risk, Merge, Clear) cannot be sent for proto < 29
    // because their prefix chars would exceed legal_len = 1
    None
}

/// Builds a prefix for protocol >= 29 (modern, full modifiers).
fn build_modern_prefix(rule: &FilterRuleWireFormat, protocol: ProtocolVersion) -> String {
    use super::wire::RuleType;

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

    // Protocol version gated modifiers.
    //
    // upstream: exclude.c:1180-1207 sets prefix_specifies_side when the rule
    // type char is R/P (and S/H if/when added), and exclude.c:1269-1278
    // rejects an explicit `r`/`s` modifier in that case as
    // "invalid modifier". The R/P/S/H prefixes already imply the side, so we
    // must not emit the redundant `r`/`s` flag for those rule types - even if
    // the wire-format struct has the corresponding side bit set.
    let side_implied_by_prefix = matches!(rule.rule_type, RuleType::Risk | RuleType::Protect);

    if protocol.supports_sender_receiver_modifiers() && !side_implied_by_prefix {
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

        let prefix = build_rule_prefix(&rule, protocol).unwrap();
        assert_eq!(prefix, "- ");
    }

    #[test]
    fn simple_include_prefix() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat::include("test".to_owned());

        let prefix = build_rule_prefix(&rule, protocol).unwrap();
        assert_eq!(prefix, "+ ");
    }

    #[test]
    fn anchored_modifier() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat::exclude("test".to_owned()).with_anchored(true);

        let prefix = build_rule_prefix(&rule, protocol).unwrap();
        assert_eq!(prefix, "-/ ");
    }

    #[test]
    fn multiple_modifiers() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let mut rule = FilterRuleWireFormat::exclude("test".to_owned());
        rule.anchored = true;
        rule.no_inherit = true;
        rule.cvs_exclude = true;

        let prefix = build_rule_prefix(&rule, protocol).unwrap();
        assert_eq!(prefix, "-/Cn ");
    }

    #[test]
    fn sender_side_v29() {
        let protocol = ProtocolVersion::from_supported(29).unwrap();
        let rule = FilterRuleWireFormat::exclude("test".to_owned()).with_sides(true, false);

        let prefix = build_rule_prefix(&rule, protocol).unwrap();
        assert_eq!(prefix, "-s ");
    }

    #[test]
    fn receiver_side_v29() {
        let protocol = ProtocolVersion::from_supported(29).unwrap();
        let rule = FilterRuleWireFormat::exclude("test".to_owned()).with_sides(false, true);

        let prefix = build_rule_prefix(&rule, protocol).unwrap();
        assert_eq!(prefix, "-r ");
    }

    #[test]
    fn both_sides_v29() {
        let protocol = ProtocolVersion::from_supported(29).unwrap();
        let rule = FilterRuleWireFormat::exclude("test".to_owned()).with_sides(true, true);

        let prefix = build_rule_prefix(&rule, protocol).unwrap();
        assert_eq!(prefix, "-sr ");
    }

    #[test]
    fn perishable_v30() {
        let protocol = ProtocolVersion::from_supported(30).unwrap();
        let rule = FilterRuleWireFormat::exclude("test".to_owned()).with_perishable(true);

        let prefix = build_rule_prefix(&rule, protocol).unwrap();
        assert_eq!(prefix, "-p ");
    }

    #[test]
    fn v28_cannot_represent_sender_receiver() {
        let protocol = ProtocolVersion::from_supported(28).unwrap();
        let rule = FilterRuleWireFormat::exclude("test".to_owned()).with_sides(true, true);

        // v28 uses old prefixes which cannot encode modifiers - returns None
        assert!(build_rule_prefix(&rule, protocol).is_none());
    }

    #[test]
    fn v28_cannot_represent_perishable() {
        let protocol = ProtocolVersion::from_supported(28).unwrap();
        let rule = FilterRuleWireFormat::exclude("test".to_owned()).with_perishable(true);

        // v28 uses old prefixes which cannot encode modifiers - returns None
        assert!(build_rule_prefix(&rule, protocol).is_none());
    }

    #[test]
    fn v29_supports_sender_receiver_but_not_perishable() {
        let protocol = ProtocolVersion::from_supported(29).unwrap();
        let rule = FilterRuleWireFormat::exclude("test".to_owned())
            .with_sides(true, true)
            .with_perishable(true);

        let prefix = build_rule_prefix(&rule, protocol).unwrap();
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

        let prefix = build_rule_prefix(&rule, protocol).unwrap();
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

        let prefix = build_rule_prefix(&rule, protocol).unwrap();
        assert_eq!(prefix, "! ");
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

        let prefix = build_rule_prefix(&rule, protocol).unwrap();
        assert_eq!(prefix, "P ");
    }

    /// Risk/Protect prefixes already imply receiver-side at the upstream
    /// parser (exclude.c:1180-1207). Emitting an explicit `r` modifier on
    /// top of those is rejected by upstream as "invalid modifier 'r' at
    /// position N in filter rule". The encoder must therefore suppress the
    /// `r` flag when the rule type is Risk or Protect, even if the wire
    /// struct carries `receiver_side: true`.
    #[test]
    fn risk_rule_does_not_emit_redundant_receiver_modifier() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat {
            rule_type: RuleType::Risk,
            pattern: "*.log".to_owned(),
            anchored: false,
            directory_only: false,
            no_inherit: false,
            cvs_exclude: false,
            word_split: false,
            exclude_from_merge: false,
            xattr_only: false,
            sender_side: false,
            receiver_side: true,
            perishable: false,
            negate: false,
        };

        let prefix = build_rule_prefix(&rule, protocol).unwrap();
        assert_eq!(prefix, "R ");
    }

    #[test]
    fn protect_rule_does_not_emit_redundant_receiver_modifier() {
        let protocol = ProtocolVersion::from_supported(32).unwrap();
        let rule = FilterRuleWireFormat {
            rule_type: RuleType::Protect,
            pattern: "*.log".to_owned(),
            anchored: false,
            directory_only: false,
            no_inherit: false,
            cvs_exclude: false,
            word_split: false,
            exclude_from_merge: false,
            xattr_only: false,
            sender_side: false,
            receiver_side: true,
            perishable: false,
            negate: false,
        };

        let prefix = build_rule_prefix(&rule, protocol).unwrap();
        assert_eq!(prefix, "P ");
    }
}
