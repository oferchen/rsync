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
                // upstream: exclude.c:setup_merge_file() derives the
                // per-directory merge FILENAME from the basename after the last
                // '/' in the rule pattern (`ex->pattern = strdup(y+1)` where
                // `y = strrchr(x, '/')`). A client's `-F` reaches the receiver as
                // `: /.rsync-filter` (exclude.c:1608), so the wire pattern is
                // `/.rsync-filter`. Using it verbatim as the merge filename makes
                // `directory.join("/.rsync-filter")` resolve to the filesystem
                // root (Rust's `Path::join` discards the base on an absolute
                // component), so the per-directory merge file is never found and
                // its protect rules are absent when the --delete pass decides
                // candidates - deleting dir-merge-protected destination entries.
                // Split off the basename to mirror setup_merge_file(); oc's own
                // encoder emits the anchor as a `/` modifier with a bare pattern,
                // so this is a no-op for the oc<->oc wire and only normalises the
                // `/`-in-body form a real upstream client sends.
                let filename = wire_rule
                    .pattern
                    .rsplit('/')
                    .next()
                    .unwrap_or(wire_rule.pattern.as_str());
                let mut config = DirMergeConfig::new(filename);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A real upstream client transmits `-F` as the rule `: /.rsync-filter`
    /// (exclude.c:1608), so the receiver decodes a `DirMerge` wire rule whose
    /// pattern is `/.rsync-filter`. Upstream's `setup_merge_file()` splits at the
    /// last '/' to recover the per-directory filename `.rsync-filter`; keeping the
    /// leading slash makes `directory.join("/.rsync-filter")` escape to the
    /// filesystem root, so the merge file is never found and its protect rules
    /// are absent when the --delete pass runs - deleting entries the client's
    /// dir-merge protects. Encode that the decoded config filename is the
    /// basename, matching upstream.
    #[test]
    fn dir_merge_wire_pattern_yields_basename_filename() {
        let wire = vec![FilterRuleWireFormat {
            rule_type: RuleType::DirMerge,
            pattern: "/.rsync-filter".to_string(),
            ..FilterRuleWireFormat::default()
        }];

        let (_set, merge_configs) =
            parse_wire_filters_for_receiver(&wire).expect("dir-merge rule parses");

        assert_eq!(merge_configs.len(), 1, "one dir-merge config expected");
        assert_eq!(
            merge_configs[0].filename(),
            ".rsync-filter",
            "the leading slash from the wire pattern must be stripped so the \
             per-directory merge file is looked up as `dir/.rsync-filter`, not \
             at the filesystem root",
        );
    }

    /// oc's own encoder emits the anchor as a `/` modifier with a bare pattern,
    /// so an oc<->oc dir-merge arrives with pattern `.rsync-filter` (no slash).
    /// The basename split must leave that untouched so the oc<->oc wire path is
    /// unchanged.
    #[test]
    fn dir_merge_bare_pattern_is_unchanged() {
        let wire = vec![FilterRuleWireFormat {
            rule_type: RuleType::DirMerge,
            pattern: ".rsync-filter".to_string(),
            anchored: true,
            ..FilterRuleWireFormat::default()
        }];

        let (_set, merge_configs) =
            parse_wire_filters_for_receiver(&wire).expect("dir-merge rule parses");

        assert_eq!(merge_configs.len(), 1);
        assert_eq!(merge_configs[0].filename(), ".rsync-filter");
    }
}
