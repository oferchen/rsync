use std::ffi::{OsStr, OsString};

use core::client::{DirMergeEnforcedKind, FilterRuleKind, FilterRuleSpec};
use core::message::{Message, Role};
use core::rsync_error;

use super::directive::{FilterDirective, MergeDirective};

mod helpers;
mod merge;
mod modifiers;
mod shorthand;

use helpers::{split_keyword_modifiers, split_short_rule_modifiers};
use merge::parse_short_merge_directive;
use modifiers::{apply_rule_modifiers, parse_rule_modifiers};
use shorthand::parse_filter_shorthand;

pub(crate) use merge::parse_merge_modifiers;

pub(crate) fn parse_filter_directive(argument: &OsStr) -> Result<FilterDirective, Message> {
    let text = argument.to_string_lossy();
    let trimmed_leading = text.trim_start();

    if let Some(result) = parse_short_merge_directive(trimmed_leading) {
        return result;
    }

    if let Some(result) = parse_long_merge_directive(trimmed_leading) {
        return result;
    }

    parse_rule_directive(trimmed_leading)
}

fn parse_long_merge_directive(text: &str) -> Option<Result<FilterDirective, Message>> {
    let remainder = text.strip_prefix("merge")?;
    let mut remainder =
        remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    let mut modifiers = "";
    if let Some(next) = remainder.strip_prefix(',') {
        let mut split = next.splitn(2, |ch: char| ch.is_ascii_whitespace() || ch == '_');
        modifiers = split.next().unwrap_or("");
        remainder = split
            .next()
            .unwrap_or("")
            .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    }
    let (options, assume_cvsignore) = match parse_merge_modifiers(modifiers, text, false) {
        Ok(result) => result,
        Err(error) => return Some(Err(error)),
    };

    let mut path_text = remainder.trim_end();
    if path_text.is_empty() {
        if assume_cvsignore {
            path_text = ".cvsignore";
        } else {
            let message = rsync_error!(
                1,
                format!("filter merge directive '{text}' is missing a file path")
            )
            .with_role(Role::Client);
            return Some(Err(message));
        }
    }

    let enforced_kind = match options.enforced_kind() {
        Some(DirMergeEnforcedKind::Include) => Some(FilterRuleKind::Include),
        Some(DirMergeEnforcedKind::Exclude) => Some(FilterRuleKind::Exclude),
        None => None,
    };

    let directive =
        MergeDirective::new(OsString::from(path_text), enforced_kind).with_options(options);
    Some(Ok(FilterDirective::Merge(directive)))
}

fn parse_rule_directive(text: &str) -> Result<FilterDirective, Message> {
    let trimmed = text.trim_end();

    if trimmed.is_empty() {
        let message = rsync_error!(
            1,
            "filter rule is empty: supply '+', '-', '!', or 'merge FILE'"
        )
        .with_role(Role::Client);
        return Err(message);
    }

    if let Some(remainder) = trimmed.strip_prefix('!') {
        if remainder.trim().is_empty() {
            return Ok(FilterDirective::Clear);
        }

        let message = rsync_error!(1, "'!' rule has trailing characters: {}", trimmed)
            .with_role(Role::Client);
        return Err(message);
    }

    if trimmed.eq_ignore_ascii_case("clear") {
        return Ok(FilterDirective::Clear);
    }

    if let Some(result) = parse_shorthand_rules(trimmed) {
        return result;
    }

    if let Some(result) = parse_exclude_if_present(trimmed) {
        return result;
    }

    if let Some(result) = parse_short_include_rule(trimmed, '+', FilterRuleSpec::include) {
        return result;
    }

    if let Some(result) = parse_short_include_rule(trimmed, '-', FilterRuleSpec::exclude) {
        return result;
    }

    if let Some(result) = parse_dir_merge_alias(trimmed) {
        return result;
    }

    parse_keyword_rule(trimmed)
}

fn parse_shorthand_rules(trimmed: &str) -> Option<Result<FilterDirective, Message>> {
    if let Some(result) = parse_filter_shorthand(trimmed, 'P', "P", FilterRuleSpec::protect) {
        return Some(result);
    }

    if let Some(result) = parse_filter_shorthand(trimmed, 'H', "H", FilterRuleSpec::hide) {
        return Some(result);
    }

    if let Some(result) = parse_filter_shorthand(trimmed, 'S', "S", FilterRuleSpec::show) {
        return Some(result);
    }

    if let Some(result) = parse_filter_shorthand(trimmed, 'R', "R", FilterRuleSpec::risk) {
        return Some(result);
    }

    None
}

fn parse_exclude_if_present(trimmed: &str) -> Option<Result<FilterDirective, Message>> {
    const EXCLUDE_IF_PRESENT_PREFIX: &str = "exclude-if-present";
    if trimmed.len() < EXCLUDE_IF_PRESENT_PREFIX.len() {
        return None;
    }

    let prefix = &trimmed[..EXCLUDE_IF_PRESENT_PREFIX.len()];
    if !prefix.eq_ignore_ascii_case(EXCLUDE_IF_PRESENT_PREFIX) {
        return None;
    }

    let mut remainder = trimmed[EXCLUDE_IF_PRESENT_PREFIX.len()..]
        .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    if let Some(rest) = remainder.strip_prefix('=') {
        remainder = rest.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    }

    let pattern_text = remainder.trim();
    if pattern_text.is_empty() {
        let message = rsync_error!(
            1,
            format!("filter rule '{trimmed}' is missing a marker file after 'exclude-if-present'")
        )
        .with_role(Role::Client);
        return Some(Err(message));
    }

    Some(Ok(FilterDirective::Rule(
        FilterRuleSpec::exclude_if_present(pattern_text.to_owned()),
    )))
}

fn parse_short_include_rule(
    trimmed: &str,
    prefix: char,
    builder: fn(String) -> FilterRuleSpec,
) -> Option<Result<FilterDirective, Message>> {
    let remainder = trimmed.strip_prefix(prefix)?;
    let (modifier_text, remainder) = split_short_rule_modifiers(remainder);
    let modifiers = match parse_rule_modifiers(modifier_text, trimmed, true, true) {
        Ok(state) => state,
        Err(error) => return Some(Err(error)),
    };
    let pattern = remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    if pattern.is_empty() {
        let text = format!("filter rule '{trimmed}' is missing a pattern after '{prefix}'");
        let message = rsync_error!(1, text).with_role(Role::Client);
        return Some(Err(message));
    }

    let rule = builder(pattern.to_owned());
    let rule = match apply_rule_modifiers(rule, modifiers, trimmed) {
        Ok(rule) => rule,
        Err(error) => return Some(Err(error)),
    };
    Some(Ok(FilterDirective::Rule(rule)))
}

fn parse_dir_merge_alias(trimmed: &str) -> Option<Result<FilterDirective, Message>> {
    const DIR_MERGE_ALIASES: [&str; 2] = ["dir-merge", "per-dir"];

    let mut matched_prefix = None;
    for alias in DIR_MERGE_ALIASES {
        if trimmed.len() >= alias.len() && trimmed[..alias.len()].eq_ignore_ascii_case(alias) {
            matched_prefix = Some((&trimmed[..alias.len()], &trimmed[alias.len()..]));
            break;
        }
    }

    let (label, remainder) = matched_prefix?;
    let mut remainder =
        remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    let mut modifiers = "";
    if let Some(rest) = remainder.strip_prefix(',') {
        let mut split = rest.splitn(2, |ch: char| ch.is_ascii_whitespace() || ch == '_');
        modifiers = split.next().unwrap_or("");
        remainder = split
            .next()
            .unwrap_or("")
            .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    }

    let (options, assume_cvsignore) = match parse_merge_modifiers(modifiers, trimmed, true) {
        Ok(result) => result,
        Err(error) => return Some(Err(error)),
    };

    let mut path_text = remainder.trim_end();
    if path_text.is_empty() {
        if assume_cvsignore {
            path_text = ".cvsignore";
        } else {
            let text = format!("filter rule '{trimmed}' is missing a file name after '{label}'");
            return Some(Err(rsync_error!(1, text).with_role(Role::Client)));
        }
    }

    Some(Ok(FilterDirective::Rule(FilterRuleSpec::dir_merge(
        path_text.to_owned(),
        options,
    ))))
}

fn parse_keyword_rule(trimmed: &str) -> Result<FilterDirective, Message> {
    let mut parts = trimmed.splitn(2, |ch: char| ch.is_ascii_whitespace());
    let keyword = parts.next().expect("split always yields at least one part");
    let remainder = parts.next().unwrap_or("");
    let (keyword, keyword_modifiers) = split_keyword_modifiers(keyword);
    let pattern = remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());

    let build_rule = |builder: fn(String) -> FilterRuleSpec,
                      allow_perishable: bool,
                      allow_xattr: bool|
     -> Result<FilterDirective, Message> {
        if pattern.is_empty() {
            let text = format!("filter rule '{trimmed}' is missing a pattern after '{keyword}'");
            let message = rsync_error!(1, text).with_role(Role::Client);
            return Err(message);
        }

        let modifiers =
            parse_rule_modifiers(keyword_modifiers, trimmed, allow_perishable, allow_xattr)?;
        let rule = builder(pattern.to_owned());
        let rule = apply_rule_modifiers(rule, modifiers, trimmed)?;
        Ok(FilterDirective::Rule(rule))
    };

    if keyword.eq_ignore_ascii_case("include") {
        return build_rule(FilterRuleSpec::include, true, true);
    }

    if keyword.eq_ignore_ascii_case("exclude") {
        return build_rule(FilterRuleSpec::exclude, true, true);
    }

    if keyword.eq_ignore_ascii_case("show") {
        return build_rule(FilterRuleSpec::show, false, false);
    }

    if keyword.eq_ignore_ascii_case("hide") {
        return build_rule(FilterRuleSpec::hide, false, false);
    }

    if keyword.eq_ignore_ascii_case("protect") {
        return build_rule(FilterRuleSpec::protect, false, false);
    }

    if keyword.eq_ignore_ascii_case("risk") {
        return build_rule(FilterRuleSpec::risk, false, false);
    }

    let message = rsync_error!(
        1,
        "unsupported filter rule '{}': this build currently supports only '+' (include), '-' (exclude), '!' (clear), 'include PATTERN', 'exclude PATTERN', 'show PATTERN', 'hide PATTERN', 'protect PATTERN', 'risk PATTERN', 'merge[,MODS] FILE' or '.[,MODS] FILE', and 'dir-merge[,MODS] FILE' or ':[,MODS] FILE' directives",
        trimmed
    )
    .with_role(Role::Client);
    Err(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== parse_filter_directive tests ====================

    #[test]
    fn parse_include_short() {
        let result = parse_filter_directive(OsStr::new("+ *.txt"));
        assert!(result.is_ok());
        match result.unwrap() {
            FilterDirective::Rule(spec) => {
                assert_eq!(spec.kind(), FilterRuleKind::Include);
            }
            _ => panic!("expected Rule directive"),
        }
    }

    #[test]
    fn parse_exclude_short() {
        let result = parse_filter_directive(OsStr::new("- *.log"));
        assert!(result.is_ok());
        match result.unwrap() {
            FilterDirective::Rule(spec) => {
                assert_eq!(spec.kind(), FilterRuleKind::Exclude);
            }
            _ => panic!("expected Rule directive"),
        }
    }

    #[test]
    fn parse_clear_exclamation() {
        let result = parse_filter_directive(OsStr::new("!"));
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), FilterDirective::Clear));
    }

    #[test]
    fn parse_clear_keyword() {
        let result = parse_filter_directive(OsStr::new("clear"));
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), FilterDirective::Clear));
    }

    #[test]
    fn parse_clear_keyword_uppercase() {
        let result = parse_filter_directive(OsStr::new("CLEAR"));
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), FilterDirective::Clear));
    }

    #[test]
    fn parse_include_keyword() {
        let result = parse_filter_directive(OsStr::new("include *.rs"));
        assert!(result.is_ok());
        match result.unwrap() {
            FilterDirective::Rule(spec) => {
                assert_eq!(spec.kind(), FilterRuleKind::Include);
            }
            _ => panic!("expected Rule directive"),
        }
    }

    #[test]
    fn parse_exclude_keyword() {
        let result = parse_filter_directive(OsStr::new("exclude *.bak"));
        assert!(result.is_ok());
        match result.unwrap() {
            FilterDirective::Rule(spec) => {
                assert_eq!(spec.kind(), FilterRuleKind::Exclude);
            }
            _ => panic!("expected Rule directive"),
        }
    }

    #[test]
    fn parse_empty_returns_error() {
        let result = parse_filter_directive(OsStr::new(""));
        assert!(result.is_err());
    }

    #[test]
    fn parse_whitespace_only_returns_error() {
        let result = parse_filter_directive(OsStr::new("   "));
        assert!(result.is_err());
    }

    // ==================== parse_rule_directive tests ====================

    #[test]
    fn rule_directive_protect() {
        let result = parse_rule_directive("P *.keep");
        assert!(result.is_ok());
        match result.unwrap() {
            FilterDirective::Rule(spec) => {
                assert_eq!(spec.kind(), FilterRuleKind::Protect);
            }
            _ => panic!("expected Rule directive"),
        }
    }

    #[test]
    fn rule_directive_hide() {
        let result = parse_rule_directive("H .hidden");
        assert!(result.is_ok());
        match result.unwrap() {
            FilterDirective::Rule(spec) => {
                // Hide is an exclude rule that applies to sender
                assert_eq!(spec.kind(), FilterRuleKind::Exclude);
            }
            _ => panic!("expected Rule directive"),
        }
    }

    #[test]
    fn rule_directive_show() {
        let result = parse_rule_directive("S visible");
        assert!(result.is_ok());
        match result.unwrap() {
            FilterDirective::Rule(spec) => {
                // Show is an include rule that applies to sender
                assert_eq!(spec.kind(), FilterRuleKind::Include);
            }
            _ => panic!("expected Rule directive"),
        }
    }

    #[test]
    fn rule_directive_risk() {
        let result = parse_rule_directive("R deletable");
        assert!(result.is_ok());
        match result.unwrap() {
            FilterDirective::Rule(spec) => {
                assert_eq!(spec.kind(), FilterRuleKind::Risk);
            }
            _ => panic!("expected Rule directive"),
        }
    }

    #[test]
    fn rule_directive_clear_with_trailing() {
        let result = parse_rule_directive("! trailing");
        assert!(result.is_err());
    }

    #[test]
    fn rule_directive_unsupported_keyword() {
        let result = parse_rule_directive("foobar *.txt");
        assert!(result.is_err());
    }

    // ==================== parse_exclude_if_present tests ====================

    #[test]
    fn exclude_if_present_basic() {
        let result = parse_exclude_if_present("exclude-if-present .nobackup");
        assert!(result.is_some());
        let directive = result.unwrap().unwrap();
        match directive {
            FilterDirective::Rule(spec) => {
                assert_eq!(spec.kind(), FilterRuleKind::ExcludeIfPresent);
            }
            _ => panic!("expected Rule directive"),
        }
    }

    #[test]
    fn exclude_if_present_with_equals() {
        let result = parse_exclude_if_present("exclude-if-present = marker.txt");
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());
    }

    #[test]
    fn exclude_if_present_case_insensitive() {
        let result = parse_exclude_if_present("EXCLUDE-IF-PRESENT .skip");
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());
    }

    #[test]
    fn exclude_if_present_missing_pattern() {
        let result = parse_exclude_if_present("exclude-if-present");
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn exclude_if_present_empty_pattern() {
        let result = parse_exclude_if_present("exclude-if-present   ");
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn exclude_if_present_non_matching() {
        let result = parse_exclude_if_present("other-directive");
        assert!(result.is_none());
    }

    // ==================== parse_short_include_rule tests ====================

    #[test]
    fn short_include_basic() {
        let result = parse_short_include_rule("+ *.rs", '+', FilterRuleSpec::include);
        assert!(result.is_some());
        let directive = result.unwrap().unwrap();
        match directive {
            FilterDirective::Rule(spec) => {
                assert_eq!(spec.kind(), FilterRuleKind::Include);
            }
            _ => panic!("expected Rule directive"),
        }
    }

    #[test]
    fn short_exclude_basic() {
        let result = parse_short_include_rule("- *.tmp", '-', FilterRuleSpec::exclude);
        assert!(result.is_some());
        let directive = result.unwrap().unwrap();
        match directive {
            FilterDirective::Rule(spec) => {
                assert_eq!(spec.kind(), FilterRuleKind::Exclude);
            }
            _ => panic!("expected Rule directive"),
        }
    }

    #[test]
    fn short_include_missing_pattern() {
        let result = parse_short_include_rule("+ ", '+', FilterRuleSpec::include);
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn short_include_empty_after_prefix() {
        let result = parse_short_include_rule("+", '+', FilterRuleSpec::include);
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn short_include_non_matching_prefix() {
        let result = parse_short_include_rule("- foo", '+', FilterRuleSpec::include);
        assert!(result.is_none());
    }

    // ==================== parse_dir_merge_alias tests ====================

    #[test]
    fn dir_merge_basic() {
        let result = parse_dir_merge_alias("dir-merge .rsync-filter");
        assert!(result.is_some());
        let directive = result.unwrap().unwrap();
        match directive {
            FilterDirective::Rule(spec) => {
                assert_eq!(spec.kind(), FilterRuleKind::DirMerge);
            }
            _ => panic!("expected Rule directive"),
        }
    }

    #[test]
    fn dir_merge_per_dir_alias() {
        let result = parse_dir_merge_alias("per-dir filter-file");
        assert!(result.is_some());
        let directive = result.unwrap().unwrap();
        match directive {
            FilterDirective::Rule(spec) => {
                assert_eq!(spec.kind(), FilterRuleKind::DirMerge);
            }
            _ => panic!("expected Rule directive"),
        }
    }

    #[test]
    fn dir_merge_case_insensitive() {
        let result = parse_dir_merge_alias("DIR-MERGE .filter");
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());
    }

    #[test]
    fn dir_merge_missing_filename() {
        let result = parse_dir_merge_alias("dir-merge");
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn dir_merge_non_matching() {
        let result = parse_dir_merge_alias("other-command file");
        assert!(result.is_none());
    }

    // ==================== parse_keyword_rule tests ====================

    #[test]
    fn keyword_include() {
        let result = parse_keyword_rule("include *.txt");
        assert!(result.is_ok());
    }

    #[test]
    fn keyword_exclude() {
        let result = parse_keyword_rule("exclude *.bak");
        assert!(result.is_ok());
    }

    #[test]
    fn keyword_show() {
        let result = parse_keyword_rule("show pattern");
        assert!(result.is_ok());
    }

    #[test]
    fn keyword_hide() {
        let result = parse_keyword_rule("hide pattern");
        assert!(result.is_ok());
    }

    #[test]
    fn keyword_protect() {
        let result = parse_keyword_rule("protect important");
        assert!(result.is_ok());
    }

    #[test]
    fn keyword_risk() {
        let result = parse_keyword_rule("risk disposable");
        assert!(result.is_ok());
    }

    #[test]
    fn keyword_case_insensitive() {
        let result = parse_keyword_rule("INCLUDE *.rs");
        assert!(result.is_ok());
    }

    #[test]
    fn keyword_missing_pattern() {
        let result = parse_keyword_rule("include");
        assert!(result.is_err());
    }

    #[test]
    fn keyword_unknown() {
        let result = parse_keyword_rule("unknown_keyword pattern");
        assert!(result.is_err());
    }

    // ==================== parse_long_merge_directive tests ====================

    #[test]
    fn long_merge_basic() {
        let result = parse_long_merge_directive("merge filter.rules");
        assert!(result.is_some());
        let directive = result.unwrap().unwrap();
        assert!(matches!(directive, FilterDirective::Merge(_)));
    }

    #[test]
    fn long_merge_missing_path() {
        let result = parse_long_merge_directive("merge");
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn long_merge_non_matching() {
        let result = parse_long_merge_directive("include pattern");
        assert!(result.is_none());
    }

    // ==================== parse_shorthand_rules tests ====================

    #[test]
    fn shorthand_protect() {
        let result = parse_shorthand_rules("P *.important");
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());
    }

    #[test]
    fn shorthand_hide() {
        let result = parse_shorthand_rules("H .hidden");
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());
    }

    #[test]
    fn shorthand_show() {
        let result = parse_shorthand_rules("S visible");
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());
    }

    #[test]
    fn shorthand_risk() {
        let result = parse_shorthand_rules("R temp");
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());
    }

    #[test]
    fn shorthand_non_matching() {
        let result = parse_shorthand_rules("+ pattern");
        assert!(result.is_none());
    }

    // ==================== Leading whitespace handling tests ====================

    #[test]
    fn leading_whitespace_trimmed() {
        let result = parse_filter_directive(OsStr::new("   + *.txt"));
        assert!(result.is_ok());
        match result.unwrap() {
            FilterDirective::Rule(spec) => {
                assert_eq!(spec.kind(), FilterRuleKind::Include);
            }
            _ => panic!("expected Rule directive"),
        }
    }

    #[test]
    fn multiple_spaces_in_pattern() {
        let result = parse_filter_directive(OsStr::new("+   *.txt"));
        assert!(result.is_ok());
    }
}
