//! Wire-format filter-rule parsing for the receiver transfer setup.
//!
//! Converts the wire-format filter rules received during setup into a
//! `FilterSet` plus the per-directory `DirMergeConfig` list the deletion pass
//! consults.

use std::io;

use protocol::filters::{FilterRuleWireFormat, RuleType};

use filters::{DirMergeConfig, FilterSet};

/// Parses wire-format filter rules into a `FilterSet` and `DirMergeConfig` list for the receiver.
///
/// Separates DirMerge rules (for per-directory merge file scanning) from regular
/// filter rules. The returned `FilterSet` contains compiled include/exclude/protect/risk
/// rules. The `DirMergeConfig` list configures per-directory merge file scanning
/// used during deletion filtering.
///
/// # Upstream Reference
///
/// - `exclude.c:recv_filter_list()` - receiver-side filter list reception
/// - `generator.c:delete_in_dir()` - deletion pass uses filter evaluation
pub(super) fn parse_wire_filters_for_receiver(
    wire_rules: &[FilterRuleWireFormat],
) -> io::Result<(FilterSet, Vec<DirMergeConfig>)> {
    use ::filters::FilterRule;

    let mut rules = Vec::with_capacity(wire_rules.len());
    let mut merge_configs = Vec::new();

    for wire_rule in wire_rules {
        let mut rule = match wire_rule.rule_type {
            RuleType::Include => FilterRule::include(&wire_rule.pattern),
            RuleType::Exclude => FilterRule::exclude(&wire_rule.pattern),
            RuleType::Protect => FilterRule::protect(&wire_rule.pattern),
            RuleType::Risk => FilterRule::risk(&wire_rule.pattern),
            RuleType::Clear => {
                rules.push(
                    FilterRule::clear().with_sides(wire_rule.sender_side, wire_rule.receiver_side),
                );
                continue;
            }
            RuleType::DirMerge => {
                let mut config = DirMergeConfig::new(&wire_rule.pattern);
                if wire_rule.no_inherit {
                    config = config.with_inherit(false);
                }
                if wire_rule.exclude_from_merge {
                    config = config.with_exclude_self(true);
                }
                if wire_rule.sender_side {
                    config = config.with_sender_only(true);
                }
                if wire_rule.receiver_side {
                    config = config.with_receiver_only(true);
                }
                if wire_rule.perishable {
                    config = config.with_perishable(true);
                }
                merge_configs.push(config);
                continue;
            }
            RuleType::Merge => continue,
        };

        if wire_rule.sender_side || wire_rule.receiver_side {
            rule = rule.with_sides(wire_rule.sender_side, wire_rule.receiver_side);
        }
        if wire_rule.perishable {
            rule = rule.with_perishable(true);
        }
        if wire_rule.xattr_only {
            rule = rule.with_xattr_only(true);
        }
        if wire_rule.negate {
            rule = rule.with_negate(true);
        }
        if wire_rule.anchored {
            rule = rule.anchor_to_root();
        }

        rules.push(rule);
    }

    let filter_set = FilterSet::from_rules(rules)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("filter error: {e}")))?;

    Ok((filter_set, merge_configs))
}
