//! Wire-format filter-rule parsing for the receiver transfer setup.
//!
//! Converts the wire-format filter rules received during setup into a
//! `FilterSet` plus the per-directory `DirMergeConfig` list the deletion pass
//! consults.

use std::borrow::Cow;
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
pub(in crate::receiver) fn parse_wire_filters_for_receiver(
    wire_rules: &[FilterRuleWireFormat],
) -> io::Result<(FilterSet, Vec<DirMergeConfig>)> {
    use ::filters::FilterRule;

    let mut rules = Vec::with_capacity(wire_rules.len());
    let mut merge_configs = Vec::new();

    for wire_rule in wire_rules {
        // The wire format carries the directory-only (`/`) modifier as a
        // separate flag, but the `filters` crate encodes it as a trailing `/`
        // in the pattern string. Re-append it so a rule like `- foo/*/` keeps
        // its directory-only semantics instead of matching plain files. Without
        // this the receiver's rule set diverges from the sender's (which uses
        // generator/filters.rs reconstruct_pattern), causing the flist re-check
        // and the deletion pass to over-match. The anchored (`/`) modifier is
        // applied below via `anchor_to_root()`.
        // upstream: exclude.c:get_rule_prefix() - directory-only is a trailing
        // slash on the pattern body.
        // The `filters` crate compiles patterns into `wildmatch`, which operates
        // on `&str`, so a non-UTF-8 wire pattern is decoded lossily here for the
        // local match set. The wire pattern itself stays byte-faithful (it is an
        // `OsString`); only this receiver-side rule-compilation boundary is
        // lossy, mirroring the fact that the whole `filters` model is `String`.
        let lossy = wire_rule.pattern.to_string_lossy();
        let pattern: Cow<'_, str> = if wire_rule.directory_only && !lossy.ends_with('/') {
            Cow::Owned(format!("{lossy}/"))
        } else {
            lossy.clone()
        };
        let mut rule = match wire_rule.rule_type {
            RuleType::Include => FilterRule::include(pattern.as_ref()),
            RuleType::Exclude => FilterRule::exclude(pattern.as_ref()),
            RuleType::Protect => FilterRule::protect(pattern.as_ref()),
            RuleType::Risk => FilterRule::risk(pattern.as_ref()),
            RuleType::Clear => {
                // upstream: exclude.c:1542-1551 parse_filter_str() - a clear
                // rule pops the WHOLE active list (`pop_filter_list(listp);
                // listp->head = NULL;`) with no side test at all, so an
                // unsided `!` wipes both sides. `FilterRule::clear()` already
                // carries that (sender and receiver both true), while
                // `apply_clear_rule` returns without clearing anything when
                // neither side is set - so narrowing unconditionally turned an
                // unsided clear into a silent no-op. Apply the sides only when
                // the wire rule actually named one, exactly as the
                // include/exclude/protect/risk arms below do.
                let mut rule = FilterRule::clear();
                if wire_rule.sender_side || wire_rule.receiver_side {
                    rule = rule.with_sides(wire_rule.sender_side, wire_rule.receiver_side);
                }
                rules.push(rule);
                continue;
            }
            RuleType::DirMerge => {
                merge_configs.push(dir_merge_config_from_wire(wire_rule));
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

/// Decodes one `DirMerge` wire rule into the receiver's per-directory merge
/// configuration.
///
/// The receiver reaches per-directory merges from two directions - the rules a
/// client transmits and the rules a daemon module's `filter` directive
/// contributes - and both must decode the same way, so this is the single
/// decoder for both.
///
/// # Upstream Reference
///
/// `exclude.c:setup_merge_file()` derives the per-directory merge FILENAME from
/// the basename after the last '/' in the rule pattern (`ex->pattern =
/// strdup(y+1)` where `y = strrchr(x, '/')`). A client's `-F` reaches the
/// receiver as `: /.rsync-filter` (exclude.c:1608), so the wire pattern is
/// `/.rsync-filter`. Using it verbatim as the merge filename makes
/// `directory.join("/.rsync-filter")` resolve to the filesystem root (Rust's
/// `Path::join` discards the base on an absolute component), so the
/// per-directory merge file is never found and its protect rules are absent
/// when the --delete pass decides candidates - deleting dir-merge-protected
/// destination entries. Splitting off the basename mirrors
/// `setup_merge_file()`; oc's own encoder emits the anchor as a `/` modifier
/// with a bare pattern, so this is a no-op for the oc<->oc wire and only
/// normalises the `/`-in-body form a real upstream client sends.
pub(in crate::receiver) fn dir_merge_config_from_wire(
    wire_rule: &FilterRuleWireFormat,
) -> DirMergeConfig {
    let lossy = wire_rule.pattern.to_string_lossy();
    let filename = lossy.rsplit('/').next().unwrap_or(lossy.as_ref());
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
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;

    fn exclude(pattern: &str) -> FilterRuleWireFormat {
        FilterRuleWireFormat {
            rule_type: RuleType::Exclude,
            pattern: pattern.into(),
            ..FilterRuleWireFormat::default()
        }
    }

    /// An unsided `!` must pop every rule that precedes it.
    ///
    /// Upstream's `parse_filter_str` (exclude.c:1542-1551) handles a clear rule
    /// by calling `pop_filter_list(listp)` and nulling the head - there is no
    /// side test, so an unsided clear wipes the whole list. oc keeps the clear
    /// as a `RuleType::Clear` wire rule and defers it to this converter, which
    /// means a converter that drops it leaves the pre-clear rules standing.
    ///
    /// This is reachable: a daemon module's `filter` directive
    /// (clientserver.c:934) reaches the receiver's deletion chain through
    /// `combine`d daemon rules, so a dropped clear keeps a `- keep.txt` alive
    /// and shields `keep.txt` from a `--delete` pass that upstream performs.
    /// Measured against rsync 3.5.0 as a daemon receiver with
    /// `filter = - keep.txt clear - ctl.txt`: upstream deletes `keep.txt`,
    /// oc kept it.
    #[test]
    fn unsided_clear_pops_the_rules_that_precede_it() {
        let wire = vec![
            exclude("keep.txt"),
            FilterRuleWireFormat {
                rule_type: RuleType::Clear,
                ..FilterRuleWireFormat::default()
            },
            exclude("ctl.txt"),
        ];

        let (set, _merge_configs) =
            parse_wire_filters_for_receiver(&wire).expect("clear rule parses");

        assert!(
            set.allows(Path::new("keep.txt"), false),
            "an unsided clear must pop the `- keep.txt` that precedes it, as \
             upstream's pop_filter_list does; leaving it standing shields \
             keep.txt from the --delete pass",
        );
        assert!(
            !set.allows(Path::new("ctl.txt"), false),
            "the rule added after the clear must survive it - this control \
             stays red-free whether or not the clear is honoured, so a green \
             run of it proves the fixture compiled rules at all",
        );
    }

    /// The same rule list, on the deletion chain the `--delete` pass consults.
    ///
    /// `compile_receiver_filter_chains` feeds this converter's `FilterSet` to
    /// both the transfer chain and the deletion chain, so the clear has to pop
    /// the deletion view too - that is the view the measured daemon cell
    /// observed.
    #[test]
    fn unsided_clear_pops_the_deletion_view_too() {
        let wire = vec![
            exclude("keep.txt"),
            FilterRuleWireFormat {
                rule_type: RuleType::Clear,
                ..FilterRuleWireFormat::default()
            },
            exclude("ctl.txt"),
        ];

        let (set, _merge_configs) =
            parse_wire_filters_for_receiver(&wire).expect("clear rule parses");

        assert!(
            set.allows_deletion(Path::new("keep.txt"), false),
            "with the pre-clear exclude popped, keep.txt is an ordinary \
             extraneous file and the --delete pass must be free to remove it",
        );
        assert!(
            !set.allows_deletion(Path::new("ctl.txt"), false),
            "the post-clear exclude still protects ctl.txt from deletion",
        );
    }

    /// A clear that names a side must still narrow to that side.
    ///
    /// upstream: exclude.c:1423-1432 parse_rule_tok() sets
    /// `FILTRULE_RECEIVER_SIDE` / `FILTRULE_SENDER_SIDE` from the rule's own
    /// `r` / `s` modifiers, so honouring an unsided clear must not make every
    /// clear both-sided. A receiver-side clear leaves a sender-side rule
    /// untouched.
    #[test]
    fn a_sided_clear_narrows_to_the_side_it_names() {
        let wire = vec![
            FilterRuleWireFormat {
                sender_side: true,
                ..exclude("sender-only.txt")
            },
            FilterRuleWireFormat {
                rule_type: RuleType::Clear,
                receiver_side: true,
                ..FilterRuleWireFormat::default()
            },
        ];

        let (set, _merge_configs) =
            parse_wire_filters_for_receiver(&wire).expect("sided clear rule parses");

        assert!(
            !set.allows(Path::new("sender-only.txt"), false),
            "a receiver-side clear must leave a sender-side rule standing; \
             widening every clear to both sides would drop it",
        );
    }

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
            pattern: "/.rsync-filter".into(),
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
            pattern: ".rsync-filter".into(),
            anchored: true,
            ..FilterRuleWireFormat::default()
        }];

        let (_set, merge_configs) =
            parse_wire_filters_for_receiver(&wire).expect("dir-merge rule parses");

        assert_eq!(merge_configs.len(), 1);
        assert_eq!(merge_configs[0].filename(), ".rsync-filter");
    }

    /// End-to-end: a `:C` dir-merge as an UPSTREAM peer emits it must yield a
    /// non-inheriting per-directory merge config on the receiver. Upstream's
    /// `parse_rule_tok` case `C` sets NO_INHERIT (exclude.c:1248-1255), and this
    /// receiver gates `DirMergeConfig::with_inherit(false)` on `wire_rule.no_inherit`.
    /// Before the wire parser re-derived the `C`-implied flags, `no_inherit` came
    /// back unset and the config inherited into subdirectories - dropping the CVS
    /// no-inherit semantics for a real upstream `:C`. Decode the raw `:C` bytes
    /// (exercising the parser) and confirm the built config does not inherit.
    #[test]
    fn upstream_colon_c_dir_merge_yields_non_inheriting_config() {
        let protocol = protocol::ProtocolVersion::from_supported(32).unwrap();
        // Wire record an upstream peer emits for a CVS per-directory merge.
        let payload: &[u8] = b":C .cvsignore";
        let mut buf = Vec::new();
        buf.extend_from_slice(&(payload.len() as i32).to_le_bytes());
        buf.extend_from_slice(payload);
        buf.extend_from_slice(&0i32.to_le_bytes());

        let wire = protocol::filters::read_filter_list(&mut &buf[..], protocol)
            .expect("`:C` record decodes");
        assert_eq!(wire.len(), 1);
        assert!(wire[0].cvs_exclude, "`C` bit decoded");
        assert!(wire[0].no_inherit, "`C` re-derives no-inherit on decode");

        let (_set, merge_configs) =
            parse_wire_filters_for_receiver(&wire).expect("dir-merge rule parses");
        assert_eq!(merge_configs.len(), 1);
        assert_eq!(merge_configs[0].filename(), ".cvsignore");
        assert!(
            !merge_configs[0].inherits(),
            "an upstream `:C` merge must NOT inherit into subdirectories",
        );
    }
}
