//! Filter rule compilation for client transfers.
//!
//! Translates [`FilterRuleSpec`] entries from the client configuration into the
//! engine's [`FilterProgram`] representation. This mirrors the filter setup
//! phase of upstream `main.c` where CLI filter flags are compiled before
//! the transfer begins.

use engine::local_copy::{DirMergeRule, FilterProgram, FilterProgramEntry};
use filters::FilterRule as EngineFilterRule;

use super::super::config::{FilterRuleKind, FilterRuleSpec};
use super::super::error::{ClientError, compile_filter_error};

pub(crate) fn compile_filter_program(
    rules: &[FilterRuleSpec],
    delete_excluded: bool,
) -> Result<Option<FilterProgram>, ClientError> {
    if rules.is_empty() {
        return Ok(None);
    }

    let mut entries = Vec::new();
    for rule in rules {
        // upstream: exclude.c:1478-1481 parse_rule_tok() applies an implicit
        // FILTRULE_SENDER_SIDE when --delete-excluded is active and the
        // rule carries neither FILTRULES_SIDES nor merge/dir-merge.
        let mut rule = rule.clone();
        if delete_excluded {
            rule.apply_implicit_sender_side_for_delete_excluded();
        }

        match rule.kind() {
            FilterRuleKind::Include => entries.push(FilterProgramEntry::Rule(
                with_spec_sides(EngineFilterRule::include(rule.pattern().to_owned()), &rule)
                    .with_perishable(rule.is_perishable())
                    .with_xattr_only(rule.is_xattr_only())
                    .with_negate(rule.is_negated()),
            )),
            FilterRuleKind::Exclude => entries.push(FilterProgramEntry::Rule(
                with_spec_sides(EngineFilterRule::exclude(rule.pattern().to_owned()), &rule)
                    .with_perishable(rule.is_perishable())
                    .with_xattr_only(rule.is_xattr_only())
                    .with_negate(rule.is_negated()),
            )),
            // `P`/`R` carry the `x` modifier exactly as `-`/`+` do: upstream's
            // modifier loop runs for EVERY prefix (exclude.c:1365-1443) and
            // `x` (:1438) has no side guard. Dropping the flag here turned
            // `Px user.foo` into an ordinary PATH protect rule, so the xattr
            // chain never saw it and the destination prune ran unfiltered.
            FilterRuleKind::Protect => entries.push(FilterProgramEntry::Rule(
                with_spec_sides(EngineFilterRule::protect(rule.pattern().to_owned()), &rule)
                    .with_perishable(rule.is_perishable())
                    .with_xattr_only(rule.is_xattr_only())
                    .with_negate(rule.is_negated()),
            )),
            FilterRuleKind::Risk => entries.push(FilterProgramEntry::Rule(
                with_spec_sides(EngineFilterRule::risk(rule.pattern().to_owned()), &rule)
                    .with_perishable(rule.is_perishable())
                    .with_xattr_only(rule.is_xattr_only())
                    .with_negate(rule.is_negated()),
            )),
            // The `:e` self-exclude is NOT synthesised here. `FilterProgram`
            // owns it, so a program built by any route - this one, or a caller
            // assembling entries directly - registers it identically
            // (exclude.c:1558-1571).
            FilterRuleKind::DirMerge => {
                entries.push(FilterProgramEntry::DirMerge(DirMergeRule::new(
                    rule.pattern().to_owned(),
                    rule.dir_merge_options().cloned().unwrap_or_default(),
                )))
            }
        }
    }

    FilterProgram::new(entries)
        .map(Some)
        .map_err(|error| compile_filter_error(error.pattern(), &error))
}

/// Carries a spec's sides onto its engine rule, keeping whether the rule's text
/// named them (exclude.c:1423-1432): `-sr` stays sided, a plain rule does not.
fn with_spec_sides(rule: EngineFilterRule, spec: &FilterRuleSpec) -> EngineFilterRule {
    if spec.names_sides() {
        rule.with_sides(spec.applies_to_sender(), spec.applies_to_receiver())
    } else {
        rule.with_sender(spec.applies_to_sender())
            .with_receiver(spec.applies_to_receiver())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain rule reaches the engine unsided and `-sr` sided, so the engine's
    /// own side checks see what the operator wrote.
    #[test]
    fn engine_rule_keeps_whether_the_spec_named_its_sides() {
        let plain = with_spec_sides(
            EngineFilterRule::exclude("a"),
            &FilterRuleSpec::exclude("a"),
        );
        assert!(!plain.specifies_side());
        let both = with_spec_sides(
            EngineFilterRule::exclude("a"),
            &FilterRuleSpec::exclude("a").with_sides(true, true),
        );
        assert!(both.specifies_side());
        assert!(both.applies_to_sender() && both.applies_to_receiver());
    }
}
