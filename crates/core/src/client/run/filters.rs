//! Filter rule compilation for client transfers.
//!
//! Translates [`FilterRuleSpec`] entries from the client configuration into the
//! engine's [`FilterProgram`] representation. This mirrors the filter setup
//! phase of upstream `main.c` where CLI filter flags are compiled before
//! the transfer begins.

use engine::local_copy::{DirMergeRule, ExcludeIfPresentRule, FilterProgram, FilterProgramEntry};
use filters::FilterRule as EngineFilterRule;

use super::super::config::{FilterRuleKind, FilterRuleSpec};
use super::super::error::{compile_filter_error, ClientError};

pub(crate) fn compile_filter_program(
    rules: &[FilterRuleSpec],
) -> Result<Option<FilterProgram>, ClientError> {
    if rules.is_empty() {
        return Ok(None);
    }

    let mut entries = Vec::new();
    for rule in rules {
        match rule.kind() {
            FilterRuleKind::Include => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::include(rule.pattern().to_owned())
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver())
                    .with_perishable(rule.is_perishable())
                    .with_xattr_only(rule.is_xattr_only()),
            )),
            FilterRuleKind::Exclude => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::exclude(rule.pattern().to_owned())
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver())
                    .with_perishable(rule.is_perishable())
                    .with_xattr_only(rule.is_xattr_only()),
            )),
            FilterRuleKind::Clear => entries.push(FilterProgramEntry::Clear),
            FilterRuleKind::Protect => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::protect(rule.pattern().to_owned())
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver())
                    .with_perishable(rule.is_perishable()),
            )),
            FilterRuleKind::Risk => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::risk(rule.pattern().to_owned())
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver())
                    .with_perishable(rule.is_perishable()),
            )),
            FilterRuleKind::DirMerge => {
                entries.push(FilterProgramEntry::DirMerge(DirMergeRule::new(
                    rule.pattern().to_owned(),
                    rule.dir_merge_options().cloned().unwrap_or_default(),
                )))
            }
            FilterRuleKind::ExcludeIfPresent => entries.push(FilterProgramEntry::ExcludeIfPresent(
                ExcludeIfPresentRule::new(rule.pattern().to_owned()),
            )),
        }
    }

    FilterProgram::new(entries)
        .map(Some)
        .map_err(|error| compile_filter_error(error.pattern(), &error))
}
