//! Unit tests for the filter-rule parsing submodules.

use super::*;
use filters::RuleSource;

/// These are all ARGUMENT-sourced rules: the operator typed them, so
/// `rule_text` returns the text unchanged (exclude.c:110-116).
fn arg(text: &str) -> rule_line::RuleLine<'_> {
    rule_line::RuleLine::new(text, RuleSource::Argument)
}

#[test]
fn parse_include_short() {
    let result = parse_filter_directive(OsStr::new("+ *.txt"), RuleSource::Argument);
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
    let result = parse_filter_directive(OsStr::new("- *.log"), RuleSource::Argument);
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
    let result = parse_filter_directive(OsStr::new("!"), RuleSource::Argument);
    assert!(result.is_ok());
    assert!(matches!(result.unwrap(), FilterDirective::Clear));
}

#[test]
fn parse_clear_keyword() {
    let result = parse_filter_directive(OsStr::new("clear"), RuleSource::Argument);
    assert!(result.is_ok());
    assert!(matches!(result.unwrap(), FilterDirective::Clear));
}

#[test]
fn parse_clear_keyword_uppercase_is_error() {
    // upstream: exclude.c:1139 RULE_STRCMP(s, "clear") is a case-sensitive
    // strncmp reached only via `case 'c'`. `CLEAR` misses it, reaches the inner
    // switch default, and raises "Unknown filter rule" (RERR_SYNTAX). A drop-in
    // must reject it, not silently coerce the case into a clear directive.
    let result = parse_filter_directive(OsStr::new("CLEAR"), RuleSource::Argument);
    assert!(result.is_err());
}

#[test]
fn parse_clear_keyword_mixed_case_is_error() {
    let result = parse_filter_directive(OsStr::new("Clear"), RuleSource::Argument);
    assert!(result.is_err());
}

#[test]
fn parse_include_keyword() {
    let result = parse_filter_directive(OsStr::new("include *.rs"), RuleSource::Argument);
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
    let result = parse_filter_directive(OsStr::new("exclude *.bak"), RuleSource::Argument);
    assert!(result.is_ok());
    match result.unwrap() {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Exclude);
        }
        _ => panic!("expected Rule directive"),
    }
}

#[test]
fn parse_empty_is_noop() {
    // upstream: exclude.c:1107 parse_rule_tok returns NULL for an empty rule
    // string, so a blank `--filter` value adds nothing and exits 0.
    let result = parse_filter_directive(OsStr::new(""), RuleSource::Argument);
    assert!(matches!(result, Ok(FilterDirective::Noop)));
}

#[test]
fn parse_whitespace_only_returns_error() {
    let result = parse_filter_directive(OsStr::new("   "), RuleSource::Argument);
    assert!(result.is_err());
}

#[test]
fn rule_directive_protect() {
    let result = parse_rule_directive(arg("P *.keep"));
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
    let result = parse_rule_directive(arg("H .hidden"));
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
    let result = parse_rule_directive(arg("S visible"));
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
    let result = parse_rule_directive(arg("R deletable"));
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
    let result = parse_rule_directive(arg("! trailing"));
    assert!(result.is_err());
}

#[test]
fn rule_directive_unsupported_keyword() {
    let result = parse_rule_directive(arg("foobar *.txt"));
    assert!(result.is_err());
}

#[test]
fn exclude_if_present_basic() {
    let result = parse_exclude_if_present(arg("exclude-if-present .nobackup"));
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
    let result = parse_exclude_if_present(arg("exclude-if-present = marker.txt"));
    assert!(result.is_some());
    assert!(result.unwrap().is_ok());
}

#[test]
fn exclude_if_present_case_insensitive() {
    let result = parse_exclude_if_present(arg("EXCLUDE-IF-PRESENT .skip"));
    assert!(result.is_some());
    assert!(result.unwrap().is_ok());
}

#[test]
fn exclude_if_present_missing_pattern() {
    let result = parse_exclude_if_present(arg("exclude-if-present"));
    assert!(result.is_some());
    assert!(result.unwrap().is_err());
}

#[test]
fn exclude_if_present_empty_pattern() {
    let result = parse_exclude_if_present(arg("exclude-if-present   "));
    assert!(result.is_some());
    assert!(result.unwrap().is_err());
}

#[test]
fn exclude_if_present_non_matching() {
    let result = parse_exclude_if_present(arg("other-directive"));
    assert!(result.is_none());
}

#[test]
fn short_include_basic() {
    let result = parse_short_include_rule(arg("+ *.rs"), '+', FilterRuleSpec::include);
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
    let result = parse_short_include_rule(arg("- *.tmp"), '-', FilterRuleSpec::exclude);
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
    let result = parse_short_include_rule(arg("+ "), '+', FilterRuleSpec::include);
    assert!(result.is_some());
    assert!(result.unwrap().is_err());
}

#[test]
fn short_include_empty_after_prefix() {
    let result = parse_short_include_rule(arg("+"), '+', FilterRuleSpec::include);
    assert!(result.is_some());
    assert!(result.unwrap().is_err());
}

#[test]
fn short_include_non_matching_prefix() {
    let result = parse_short_include_rule(arg("- foo"), '+', FilterRuleSpec::include);
    assert!(result.is_none());
}

#[test]
fn dir_merge_basic() {
    let result = parse_dir_merge_alias(arg("dir-merge .rsync-filter"));
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
fn per_dir_is_not_a_keyword() {
    // upstream: exclude.c recognizes only "dir-merge" (case 'd' RULE_STRCMP);
    // there is no "per-dir" spelling. oc must decline it (returns None) so the
    // caller rejects the unknown rule, rather than accepting an oc-only alias.
    assert!(parse_dir_merge_alias(arg("per-dir filter-file")).is_none());
    // The canonical keyword still parses as a dir-merge directive.
    let ok = parse_dir_merge_alias(arg("dir-merge filter-file"));
    assert!(ok.is_some());
    assert!(ok.unwrap().is_ok());
}

#[test]
fn dir_merge_uppercase_is_not_keyword() {
    // upstream: exclude.c:1143 RULE_STRCMP(s, "dir-merge") is a case-sensitive
    // strncmp reached via `case 'd'`. `DIR-MERGE` never matches, so this parser
    // must decline it (returns None) and let the caller error, rather than
    // treating a mixed-case spelling as the dir-merge directive.
    assert!(parse_dir_merge_alias(arg("DIR-MERGE .filter")).is_none());
    assert!(parse_dir_merge_alias(arg("Dir-Merge .filter")).is_none());
    // The lowercase keyword still parses as the dir-merge directive.
    let ok = parse_dir_merge_alias(arg("dir-merge .filter"));
    assert!(ok.is_some());
    assert!(ok.unwrap().is_ok());
}

#[test]
fn dir_merge_missing_filename() {
    let result = parse_dir_merge_alias(arg("dir-merge"));
    assert!(result.is_some());
    assert!(result.unwrap().is_err());
}

#[test]
fn dir_merge_non_matching() {
    let result = parse_dir_merge_alias(arg("other-command file"));
    assert!(result.is_none());
}

#[test]
fn dir_merge_leading_slash_strips_filename_without_anchoring_rules() {
    // upstream: exclude.c:599-617 parse_merge_name - the leading '/' on a merge
    // FILENAME (as generated by `-F` => `dir-merge /.rsync-filter`) only affects
    // where the merge file is located; it is stripped from the name and must NOT
    // anchor the rules loaded from that file. Anchoring is per-rule in add_rule
    // and driven by the `/` MODIFIER, not the filename slash. Setting anchor_root
    // here regressed the filter-depth test: `- secret*` in `d1/d2/.rsync-filter`
    // became `/d1/d2/secret*` and stopped matching `d1/d2/d3/secret.deeper`.
    let result = parse_dir_merge_alias(arg("dir-merge /.rsync-filter"));
    assert!(result.is_some());
    let directive = result.unwrap().unwrap();
    match directive {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::DirMerge);
            // Leading '/' is stripped from the pattern.
            assert_eq!(spec.pattern(), ".rsync-filter");
            // The filename slash must NOT set anchor_root.
            let opts = spec.dir_merge_options().unwrap();
            assert!(!opts.anchor_root_enabled());
        }
        _ => panic!("expected Rule directive"),
    }
}

#[test]
fn dir_merge_slash_modifier_still_anchors_rules() {
    // The `/` MODIFIER (after the comma) IS the real anchor_root source and
    // must keep working: `dir-merge,/ .rsync-filter` anchors loaded rules to
    // the transfer root (upstream FILTRULE_ABS_PATH via the '/' modifier).
    let result = parse_dir_merge_alias(arg("dir-merge,/ .rsync-filter"));
    assert!(result.is_some());
    let directive = result.unwrap().unwrap();
    match directive {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.pattern(), ".rsync-filter");
            let opts = spec.dir_merge_options().unwrap();
            assert!(opts.anchor_root_enabled());
        }
        _ => panic!("expected Rule directive"),
    }
}

#[test]
fn keyword_include() {
    let result = parse_keyword_rule(arg("include *.txt"));
    assert!(result.is_ok());
}

#[test]
fn keyword_exclude() {
    let result = parse_keyword_rule(arg("exclude *.bak"));
    assert!(result.is_ok());
}

#[test]
fn keyword_show() {
    let result = parse_keyword_rule(arg("show pattern"));
    assert!(result.is_ok());
}

#[test]
fn keyword_hide() {
    let result = parse_keyword_rule(arg("hide pattern"));
    assert!(result.is_ok());
}

#[test]
fn keyword_protect() {
    let result = parse_keyword_rule(arg("protect important"));
    assert!(result.is_ok());
}

#[test]
fn keyword_risk() {
    let result = parse_keyword_rule(arg("risk disposable"));
    assert!(result.is_ok());
}

#[test]
fn keyword_mixed_case_is_error() {
    // upstream: exclude.c:1069-1078 rule_strcmp - long-form keywords are matched
    // with a case-sensitive strncmp dispatched from a switch on the lowercase
    // first byte (exclude.c:1147/1155). `INCLUDE`/`Exclude` never match; they
    // reach the inner switch default and raise "Unknown filter rule". A drop-in
    // must error rather than coerce the case into the include/exclude rule.
    assert!(parse_keyword_rule(arg("INCLUDE *.rs")).is_err());
    assert!(parse_keyword_rule(arg("Include *.rs")).is_err());
    assert!(parse_keyword_rule(arg("EXCLUDE *.bak")).is_err());
    assert!(parse_keyword_rule(arg("Merge foo")).is_err());
    // Lowercase keywords still parse as their rule.
    assert!(parse_keyword_rule(arg("include *.rs")).is_ok());
    assert!(parse_keyword_rule(arg("exclude *.bak")).is_ok());
}

#[test]
fn keyword_missing_pattern() {
    let result = parse_keyword_rule(arg("include"));
    assert!(result.is_err());
}

#[test]
fn keyword_unknown() {
    let result = parse_keyword_rule(arg("unknown_keyword pattern"));
    assert!(result.is_err());
}

#[test]
fn long_merge_basic() {
    let result = parse_long_merge_directive(arg("merge filter.rules"));
    assert!(result.is_some());
    let directive = result.unwrap().unwrap();
    assert!(matches!(directive, FilterDirective::Merge(_)));
}

#[test]
fn long_merge_missing_path() {
    let result = parse_long_merge_directive(arg("merge"));
    assert!(result.is_some());
    assert!(result.unwrap().is_err());
}

#[test]
fn long_merge_non_matching() {
    let result = parse_long_merge_directive(arg("include pattern"));
    assert!(result.is_none());
}

#[test]
fn long_merge_mixed_case_is_not_keyword() {
    // upstream: exclude.c:1159 RULE_STRCMP(s, "merge") is a case-sensitive
    // strncmp reached via `case 'm'`. `Merge`/`MERGE` never match the merge
    // directive, so this parser declines them (returns None); the lowercase
    // keyword still parses as a merge directive.
    assert!(parse_long_merge_directive(arg("Merge filter.rules")).is_none());
    assert!(parse_long_merge_directive(arg("MERGE filter.rules")).is_none());
    assert!(parse_long_merge_directive(arg("merge filter.rules")).is_some());
}

#[test]
fn filter_directive_mixed_case_keywords_are_errors() {
    // End-to-end through the top-level dispatcher: mixed-case long-form keywords
    // must be rejected exactly as upstream rsync rejects them (RERR_SYNTAX),
    // while the lowercase spelling parses. upstream: exclude.c:1137-1173.
    for rule in [
        "Merge foo",
        "INCLUDE bar",
        "Exclude baz",
        "Dir-Merge .f",
        "CLEAR",
    ] {
        assert!(
            parse_filter_directive(OsStr::new(rule), RuleSource::Argument).is_err(),
            "mixed-case keyword `{rule}` must be rejected"
        );
    }
    assert!(parse_filter_directive(OsStr::new("merge foo"), RuleSource::Argument).is_ok());
    assert!(parse_filter_directive(OsStr::new("include bar"), RuleSource::Argument).is_ok());
    assert!(parse_filter_directive(OsStr::new("clear"), RuleSource::Argument).is_ok());
}

#[test]
fn shorthand_protect() {
    let result = parse_shorthand_rules(arg("P *.important"));
    assert!(result.is_some());
    assert!(result.unwrap().is_ok());
}

#[test]
fn shorthand_hide() {
    let result = parse_shorthand_rules(arg("H .hidden"));
    assert!(result.is_some());
    assert!(result.unwrap().is_ok());
}

#[test]
fn shorthand_show() {
    let result = parse_shorthand_rules(arg("S visible"));
    assert!(result.is_some());
    assert!(result.unwrap().is_ok());
}

#[test]
fn shorthand_risk() {
    let result = parse_shorthand_rules(arg("R temp"));
    assert!(result.is_some());
    assert!(result.unwrap().is_ok());
}

#[test]
fn shorthand_non_matching() {
    let result = parse_shorthand_rules(arg("+ pattern"));
    assert!(result.is_none());
}

#[test]
fn leading_whitespace_is_rejected() {
    // upstream: exclude.c:1100-1213 parse_rule_tok - a top-level rule never
    // carries FILTRULE_WORD_SPLIT, so leading whitespace is not skipped; it
    // reaches the prefix `switch` default and errors. It must not be trimmed.
    let result = parse_filter_directive(OsStr::new("   + *.txt"), RuleSource::Argument);
    assert!(result.is_err());
}

#[test]
fn trailing_whitespace_is_kept_in_pattern() {
    // upstream: exclude.c:1313 - trailing whitespace is part of the pattern
    // (strlen length), so `- *.o ` keeps its trailing space and `x.o` stays
    // included.
    let result = parse_filter_directive(OsStr::new("- *.o "), RuleSource::Argument);
    match result.unwrap() {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Exclude);
            assert_eq!(spec.pattern(), "*.o ");
        }
        _ => panic!("expected Rule directive"),
    }
}

#[test]
fn multiple_spaces_in_pattern() {
    // upstream: exclude.c:1290-1291,1313 - exactly one separator is consumed
    // after the rule char, so `+   *.txt` (three spaces) keeps the two extra
    // leading spaces in the pattern.
    match parse_filter_directive(OsStr::new("+   *.txt"), RuleSource::Argument).unwrap() {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Include);
            assert_eq!(spec.pattern(), "  *.txt");
        }
        other => panic!("expected Rule directive, got {other:?}"),
    }
}

#[test]
fn single_space_separator_consumed_short_rule() {
    // `-  x` (two spaces) keeps one leading space; verified against rsync 3.4.4
    // which excludes only a file literally named " x".
    match parse_filter_directive(OsStr::new("-  x"), RuleSource::Argument).unwrap() {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Exclude);
            assert_eq!(spec.pattern(), " x");
        }
        other => panic!("expected Rule directive, got {other:?}"),
    }
}

#[test]
fn one_space_separator_leaves_no_leading_space() {
    // `- x` (one space) consumes the single separator; pattern is `x`.
    match parse_filter_directive(OsStr::new("- x"), RuleSource::Argument).unwrap() {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Exclude);
            assert_eq!(spec.pattern(), "x");
        }
        other => panic!("expected Rule directive, got {other:?}"),
    }
}

#[test]
fn underscore_separator_leaves_following_space() {
    // `-_ x` uses `_` as the single separator, leaving the following space in
    // the pattern (` x`), matching rsync 3.4.4.
    match parse_filter_directive(OsStr::new("-_ x"), RuleSource::Argument).unwrap() {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Exclude);
            assert_eq!(spec.pattern(), " x");
        }
        other => panic!("expected Rule directive, got {other:?}"),
    }
}

#[test]
fn keyword_rule_keeps_extra_separator_in_pattern() {
    // The keyword and its pattern are split on the first whitespace only, so
    // `exclude   x` (three spaces) keeps the two extra leading spaces verbatim.
    match parse_keyword_rule(arg("exclude   x")).unwrap() {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Exclude);
            assert_eq!(spec.pattern(), "  x");
        }
        other => panic!("expected Rule directive, got {other:?}"),
    }
}

#[test]
fn shorthand_rule_keeps_extra_separator_in_pattern() {
    // `P  x` (two spaces) consumes one separator, keeping one leading space.
    match parse_shorthand_rules(arg("P  x")).unwrap().unwrap() {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Protect);
            assert_eq!(spec.pattern(), " x");
        }
        other => panic!("expected Rule directive, got {other:?}"),
    }
}

#[test]
fn exclude_negate_modifier_short() {
    let result = parse_filter_directive(OsStr::new("-! */"), RuleSource::Argument);
    assert!(result.is_ok());
    match result.unwrap() {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Exclude);
            assert!(spec.is_negated());
            assert_eq!(spec.pattern(), "*/");
        }
        _ => panic!("expected Rule directive"),
    }
}

#[test]
fn exclude_negate_modifier_keyword() {
    let result = parse_filter_directive(OsStr::new("exclude,! */"), RuleSource::Argument);
    assert!(result.is_ok());
    match result.unwrap() {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Exclude);
            assert!(spec.is_negated());
            assert_eq!(spec.pattern(), "*/");
        }
        _ => panic!("expected Rule directive"),
    }
}

#[test]
fn include_negate_modifier() {
    let result = parse_filter_directive(OsStr::new("+! *.txt"), RuleSource::Argument);
    assert!(result.is_ok());
    match result.unwrap() {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Include);
            assert!(spec.is_negated());
        }
        _ => panic!("expected Rule directive"),
    }
}

#[test]
fn old_prefix_minus_space_flips_to_exclude() {
    let result =
        parse_old_prefix_rule("- to", FilterRuleKind::Include, RuleSource::Argument).unwrap();
    match result {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Exclude);
            assert_eq!(spec.pattern(), "to");
        }
        other => panic!("expected Rule, got {other:?}"),
    }
}

#[test]
fn old_prefix_plus_space_flips_to_include() {
    let result =
        parse_old_prefix_rule("+ *.rs", FilterRuleKind::Exclude, RuleSource::Argument).unwrap();
    match result {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Include);
            assert_eq!(spec.pattern(), "*.rs");
        }
        other => panic!("expected Rule, got {other:?}"),
    }
}

/// Only a line that is exactly `!` clears under the old-prefix options.
///
/// upstream: exclude.c parse_rule_tok() XFLG_OLD_PREFIXES branch - `*s == '!'`
/// sets FILTRULE_CLEAR_LIST but does NOT advance `s`, so the later
/// `if (len > 1) rule->rflags &= ~FILTRULE_CLEAR_LIST` measures the whole line
/// and demotes anything longer back to a literal pattern. `! ` is a pattern
/// spelled "! ", not a clear; rsync 3.5.0 keeps a `keep2.txt` exclude standing
/// across `--exclude='! '` for exactly this reason.
#[test]
fn old_prefix_clear_requires_an_exact_bang() {
    assert!(matches!(
        parse_old_prefix_rule("!", FilterRuleKind::Exclude, RuleSource::Argument).unwrap(),
        FilterDirective::Clear
    ));
    for line in ["! ", "!  ", "!\t", "!   "] {
        match parse_old_prefix_rule(line, FilterRuleKind::Exclude, RuleSource::Argument).unwrap() {
            FilterDirective::Rule(spec) => assert_eq!(spec.pattern(), line),
            other => panic!("`{line}` must be a literal pattern, got {other:?}"),
        }
    }
}

#[test]
fn old_prefix_bang_with_pattern_is_raw_pattern() {
    // upstream: `!pattern` is NOT a clear - `len > 1` demotes the tentative
    // FILTRULE_CLEAR_LIST back to a literal pattern. Whitespace does not
    // rescue it either; see old_prefix_clear_requires_an_exact_bang.
    let result =
        parse_old_prefix_rule("!keepme", FilterRuleKind::Exclude, RuleSource::Argument).unwrap();
    match result {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Exclude);
            assert_eq!(spec.pattern(), "!keepme");
        }
        other => panic!("expected Rule, got {other:?}"),
    }
}

#[test]
fn old_prefix_bare_pattern_uses_default_kind() {
    let result =
        parse_old_prefix_rule("*.log", FilterRuleKind::Include, RuleSource::Argument).unwrap();
    match result {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Include);
            assert_eq!(spec.pattern(), "*.log");
        }
        other => panic!("expected Rule, got {other:?}"),
    }
}

#[test]
fn old_prefix_minus_without_space_is_raw_pattern() {
    // upstream: `-` without a trailing space is not the exclude prefix -
    // it's a literal pattern character. Same for `+`.
    let result =
        parse_old_prefix_rule("-foo", FilterRuleKind::Exclude, RuleSource::Argument).unwrap();
    match result {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Exclude);
            assert_eq!(spec.pattern(), "-foo");
        }
        other => panic!("expected Rule, got {other:?}"),
    }
}

#[test]
fn old_prefix_empty_is_noop() {
    // upstream: exclude.c:1107 - a blank `--exclude`/`--include` value adds
    // nothing (exit 0), so an empty old-prefix rule is a no-op, not an error.
    assert!(matches!(
        parse_old_prefix_rule("", FilterRuleKind::Exclude, RuleSource::Argument),
        Ok(FilterDirective::Noop)
    ));
}

#[test]
fn old_prefix_short_prefix_only_is_error() {
    // upstream: `parse_rule_tok` reports "unexpected end of filter rule"
    // when no pattern follows the prefix.
    assert!(parse_old_prefix_rule("- ", FilterRuleKind::Include, RuleSource::Argument).is_err());
    assert!(parse_old_prefix_rule("+ ", FilterRuleKind::Exclude, RuleSource::Argument).is_err());
}

#[test]
fn is_cvs_convenience_rule_detects_exclude_and_include_forms() {
    // upstream: exclude.c:1252 - the `C` (cvs-ignore) modifier is valid on
    // both `-` and `+` rule chars, with an optional comma separator.
    assert!(is_cvs_convenience_rule("-C"));
    assert!(is_cvs_convenience_rule("+C"));
    assert!(is_cvs_convenience_rule("-,C"));
    assert!(is_cvs_convenience_rule("+,C"));
}

#[test]
fn is_cvs_convenience_rule_rejects_non_cvs_forms() {
    // A lowercase `c` is an invalid modifier upstream, and a space or any
    // trailing pattern means this is an ordinary exclude/include rule.
    assert!(!is_cvs_convenience_rule("-c"));
    assert!(!is_cvs_convenience_rule("- C"));
    assert!(!is_cvs_convenience_rule("-Cp"));
    assert!(!is_cvs_convenience_rule("-foo"));
    assert!(!is_cvs_convenience_rule("C"));
    assert!(!is_cvs_convenience_rule(":C"));
}

#[test]
fn parse_cvs_convenience_rule_emits_cvs_defaults() {
    // `-C` / `+C` as a filter rule expand to the global CVS default
    // excludes rather than a literal pattern "C".
    assert_eq!(
        parse_filter_directive(OsStr::new("-C"), RuleSource::Argument).unwrap(),
        FilterDirective::CvsDefaults
    );
    assert_eq!(
        parse_filter_directive(OsStr::new("+C"), RuleSource::Argument).unwrap(),
        FilterDirective::CvsDefaults
    );
    assert_eq!(
        parse_filter_directive(OsStr::new("-,C"), RuleSource::Argument).unwrap(),
        FilterDirective::CvsDefaults
    );
}

#[test]
fn parse_literal_dash_pattern_is_not_cvs() {
    // `- C` (with a space) is an ordinary exclude of the pattern "C", not
    // the cvs-convenience rule.
    match parse_filter_directive(OsStr::new("- C"), RuleSource::Argument).unwrap() {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Exclude);
            assert_eq!(spec.pattern(), "C");
        }
        other => panic!("expected exclude Rule, got {other:?}"),
    }
}

// The following tests pin the single-char prefix / modifier case-sensitivity and
// the side-bound keyword guard to upstream rsync 3.4.4 (protocol 32). Every
// expectation was ground-truthed against the `rsync --filter=...` binary; the
// cited exit codes are RERR_SYNTAX (1). upstream: exclude.c:1137-1287.

#[test]
fn short_prefix_uppercase_modifier_is_rejected() {
    // upstream: `+S` / `-R` -> "invalid modifier 'S'/'R'" (exclude.c:1226). The
    // byte after a `+`/`-` prefix is a modifier, matched case-sensitively, so the
    // uppercase form is not silently remapped to a sender/receiver rule.
    let _ = parse_filter_directive(OsStr::new("+S"), RuleSource::Argument)
        .expect_err("+S must be rejected");
    let _ = parse_filter_directive(OsStr::new("-R"), RuleSource::Argument)
        .expect_err("-R must be rejected");
}

#[test]
fn merge_prefix_lowercase_c_modifier_is_rejected() {
    // upstream: `:c` -> "invalid modifier 'c'" (exclude.c:1226). The cvs-ignore
    // merge modifier is the uppercase `C`; the lowercase `c` is invalid.
    let _ = parse_filter_directive(OsStr::new(":c"), RuleSource::Argument)
        .expect_err(":c must be rejected");
}

#[test]
fn lowercase_single_char_side_prefix_is_unknown_rule() {
    // upstream: `s foo` / `h foo` -> "Unknown filter rule" (exclude.c:1213). The
    // single-char side prefixes are the uppercase S/H/P/R; a lowercase spelling
    // is only ever the first byte of a long keyword, so a bare `s`/`h` is not a
    // rule and must be rejected rather than treated as show/hide.
    let _ = parse_filter_directive(OsStr::new("s foo"), RuleSource::Argument)
        .expect_err("s foo must be rejected");
    let _ = parse_filter_directive(OsStr::new("h foo"), RuleSource::Argument)
        .expect_err("h foo must be rejected");
}

#[test]
fn keyword_uppercase_modifier_is_rejected() {
    // upstream: `include,X foo` / `exclude,S foo` -> "invalid modifier 'X'/'S'"
    // (exclude.c:1226). Keyword modifiers are case-sensitive too.
    let _ = parse_filter_directive(OsStr::new("include,X foo"), RuleSource::Argument)
        .expect_err("include,X must be rejected");
    let _ = parse_filter_directive(OsStr::new("exclude,S foo"), RuleSource::Argument)
        .expect_err("exclude,S must be rejected");
}

#[test]
fn side_keyword_rejects_s_and_r_modifiers() {
    // upstream: exclude.c:1269-1277 - show/hide/protect/risk set
    // prefix_specifies_side, so the `s`/`r` modifiers are invalid there
    // ("invalid modifier", exclude.c:1226). These were previously accepted as a
    // silent side-flip.
    let _ = parse_filter_directive(OsStr::new("show,r foo"), RuleSource::Argument)
        .expect_err("show,r must be rejected");
    let _ = parse_filter_directive(OsStr::new("protect,s foo"), RuleSource::Argument)
        .expect_err("protect,s must be rejected");
    let _ = parse_filter_directive(OsStr::new("hide,s foo"), RuleSource::Argument)
        .expect_err("hide,s must be rejected");
    let _ = parse_filter_directive(OsStr::new("risk,r foo"), RuleSource::Argument)
        .expect_err("risk,r must be rejected");
}

#[test]
fn side_keyword_accepts_perishable_modifier() {
    // upstream: exclude.c:1265-1267 - `p` (perishable) carries no side guard, so
    // it is valid on protect/show/etc. (upstream `protect,p foo` exits 0). This
    // was previously rejected as an unsupported modifier.
    match parse_filter_directive(OsStr::new("protect,p foo"), RuleSource::Argument)
        .expect("protect,p foo must parse")
    {
        FilterDirective::Rule(spec) => assert_eq!(spec.kind(), FilterRuleKind::Protect),
        other => panic!("expected protect Rule, got {other:?}"),
    }
    parse_filter_directive(OsStr::new("show,p foo"), RuleSource::Argument)
        .expect("show,p foo must parse");
}

#[test]
fn lowercase_modifiers_and_uppercase_prefixes_still_parse() {
    // Regression guard: the case-sensitivity fix must not reject the forms that
    // upstream accepts. Lowercase `s`/`x` modifiers on include/exclude, and the
    // uppercase single-char S/P prefixes, all remain valid (upstream exit 0).
    parse_filter_directive(OsStr::new("+s foo"), RuleSource::Argument).expect("+s foo must parse");
    parse_filter_directive(OsStr::new("include,s foo"), RuleSource::Argument)
        .expect("include,s foo must parse");
    parse_filter_directive(OsStr::new("include,x foo"), RuleSource::Argument)
        .expect("include,x foo must parse");
    match parse_filter_directive(OsStr::new("S public/**"), RuleSource::Argument)
        .expect("S shorthand must parse")
    {
        FilterDirective::Rule(spec) => assert_eq!(spec.kind(), FilterRuleKind::Include),
        other => panic!("expected show(include) Rule, got {other:?}"),
    }
    match parse_filter_directive(OsStr::new("P backups/**"), RuleSource::Argument)
        .expect("P shorthand must parse")
    {
        FilterDirective::Rule(spec) => assert_eq!(spec.kind(), FilterRuleKind::Protect),
        other => panic!("expected protect Rule, got {other:?}"),
    }
}

/// Every spelling the clear-list token can take, and what upstream rsync 3.5.0
/// does with it in the two parser contexts this module owns.
///
/// The grid is the guard: the token has two spellings (`!` and `clear`), two
/// separators (whitespace and `_`) that do NOT rescue it, one (`,`) that does,
/// and two contexts whose rules genuinely differ - so a handful of cases would
/// not pin it. Expectations were measured against a locally built rsync 3.5.0.
///
/// upstream: exclude.c parse_rule_tok() - the full-syntax branch runs
/// `if (*s) s++` and then errors on any remaining `len`, while the
/// XFLG_OLD_PREFIXES branch never errors and instead demotes on `len > 1`.
const CLEAR_TOKEN_GRID: &[(&str, FullSyntax, OldPrefix)] = &[
    ("!", FullSyntax::Clear, OldPrefix::Clear),
    ("!,", FullSyntax::Clear, OldPrefix::Pattern),
    ("! ", FullSyntax::TrailingCharacters, OldPrefix::Pattern),
    ("!  ", FullSyntax::TrailingCharacters, OldPrefix::Pattern),
    ("!_", FullSyntax::TrailingCharacters, OldPrefix::Pattern),
    ("!x", FullSyntax::TrailingCharacters, OldPrefix::Pattern),
    ("!,x", FullSyntax::TrailingCharacters, OldPrefix::Pattern),
    ("!clear", FullSyntax::TrailingCharacters, OldPrefix::Pattern),
    ("clear", FullSyntax::Clear, OldPrefix::Pattern),
    ("clear ", FullSyntax::TrailingCharacters, OldPrefix::Pattern),
    (
        "clear  ",
        FullSyntax::TrailingCharacters,
        OldPrefix::Pattern,
    ),
    ("clear_", FullSyntax::TrailingCharacters, OldPrefix::Pattern),
    ("clear,", FullSyntax::Clear, OldPrefix::Pattern),
    (
        "clear,x",
        FullSyntax::TrailingCharacters,
        OldPrefix::Pattern,
    ),
    ("clearx", FullSyntax::Rejected, OldPrefix::Pattern),
    ("Clear", FullSyntax::Rejected, OldPrefix::Pattern),
];

/// Outcome of a spelling in full `--filter` syntax.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FullSyntax {
    /// Clears the list in scope.
    Clear,
    /// `'!' rule has trailing characters`, RERR_SYNTAX.
    TrailingCharacters,
    /// Not the clear token at all; rejected as an unrecognised rule.
    Rejected,
}

/// Outcome of a spelling under `--exclude`/`--include` old-prefix parsing,
/// where the trailing-characters error cannot fire.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OldPrefix {
    /// Clears the list in scope.
    Clear,
    /// Demoted to a literal pattern equal to the whole line.
    Pattern,
}

#[test]
fn clear_token_grid_matches_upstream_in_full_filter_syntax() {
    for (spelling, expected, _) in CLEAR_TOKEN_GRID {
        let result = parse_filter_directive(OsStr::new(spelling), RuleSource::Argument);
        match (expected, result) {
            (FullSyntax::Clear, Ok(FilterDirective::Clear)) => {}
            (FullSyntax::TrailingCharacters, Err(message)) => {
                let rendered = message.to_string();
                assert!(
                    rendered.contains(&format!("'!' rule has trailing characters: {spelling}")),
                    "`--filter={spelling}` must report trailing characters, got: {rendered}"
                );
            }
            (FullSyntax::Rejected, Err(message)) => {
                let rendered = message.to_string();
                assert!(
                    !rendered.contains("trailing characters"),
                    "`--filter={spelling}` is not the clear token, so it must not \
                     report trailing characters, got: {rendered}"
                );
            }
            (expected, actual) => {
                panic!("`--filter={spelling}`: expected {expected:?}, got {actual:?}")
            }
        }
    }
}

#[test]
fn clear_token_grid_matches_upstream_in_old_prefix_syntax() {
    for (spelling, _, expected) in CLEAR_TOKEN_GRID {
        let result = parse_old_prefix_rule(spelling, FilterRuleKind::Exclude, RuleSource::Argument)
            .unwrap_or_else(|e| panic!("`--exclude={spelling}` must parse: {e}"));
        match (expected, result) {
            (OldPrefix::Clear, FilterDirective::Clear) => {}
            (OldPrefix::Pattern, FilterDirective::Rule(spec)) => {
                assert_eq!(
                    spec.pattern(),
                    *spelling,
                    "`--exclude={spelling}` must keep the whole line as the pattern"
                );
            }
            (expected, actual) => {
                panic!("`--exclude={spelling}`: expected {expected:?}, got {actual:?}")
            }
        }
    }
}

/// The grid must cover both spellings crossed with every separator class, so a
/// row dropped from the table cannot silently shrink the guard.
#[test]
fn clear_token_grid_covers_both_spellings_and_every_separator() {
    for token in ["!", "clear"] {
        for suffix in ["", " ", "_", ",", "x"] {
            let spelling = format!("{token}{suffix}");
            assert!(
                CLEAR_TOKEN_GRID.iter().any(|(s, _, _)| *s == spelling),
                "clear-token grid is missing `{spelling}`"
            );
        }
    }
    // Both contexts must be exercised for every row.
    assert!(CLEAR_TOKEN_GRID.len() >= 2 * 5);
}

/// An over-long pattern, built the way upstream's cell does
/// (`filter-merge-content-echo_test.py`): `MAXPATHLEN + 200`, so the refusal is
/// unambiguous and the full-length assertion has room to fail.
fn over_long_pattern() -> String {
    "Q".repeat(fast_io::path_limit::max_path_len() + 200)
}

/// Drains the diagnostics this thread emitted, as rendered text.
fn drained_messages() -> Vec<String> {
    logging::drain_events()
        .into_iter()
        .filter_map(|event| match event {
            logging::DiagnosticEvent::Info { message, .. } => Some(message),
            _ => None,
        })
        .collect()
}

/// WHY: upstream discards the rule and CONTINUES (`exclude.c:1533-1538`), so
/// the directive must be a no-op rather than an error - and a pattern past
/// `MAXPATHLEN` can still match (a long run of `*` matches everything), so
/// keeping it would change which files transfer, not merely what is printed.
#[test]
fn an_over_long_argument_rule_is_discarded_and_reported() {
    let _ = drained_messages();
    let pattern = over_long_pattern();

    let directive =
        parse_filter_directive(OsStr::new(&format!("- {pattern}")), RuleSource::Argument)
            .expect("an over-long rule is discarded, not a syntax error");

    assert_eq!(directive, FilterDirective::Noop);
    assert_eq!(
        drained_messages(),
        vec![filters::over_long_filter(&pattern)],
        "the refusal must name the operator's own rule at full length",
    );
}

/// The non-vacuity companion: an ordinary rule must still become a rule and
/// must stay silent, or the guard would discard every filter ever written.
#[test]
fn an_ordinary_argument_rule_survives_and_is_silent() {
    let _ = drained_messages();

    let directive = parse_filter_directive(OsStr::new("- *.tmp"), RuleSource::Argument)
        .expect("an ordinary rule parses");

    match directive {
        FilterDirective::Rule(spec) => assert_eq!(spec.pattern(), "*.tmp"),
        other => panic!("expected a Rule directive, got {other:?}"),
    }
    assert!(drained_messages().is_empty());
}

/// WHY: upstream's check sits ABOVE the `FILTRULE_MERGE_FILE` branch
/// (`exclude.c:1550`), so an over-long merge name is dropped before its file is
/// ever named to the filesystem.
#[test]
fn an_over_long_merge_directive_is_discarded() {
    let _ = drained_messages();
    let pattern = over_long_pattern();

    let directive = parse_filter_directive(
        OsStr::new(&format!("merge {pattern}")),
        RuleSource::Argument,
    )
    .expect("an over-long merge directive is discarded, not an error");

    assert_eq!(directive, FilterDirective::Noop);
    assert_eq!(
        drained_messages(),
        vec![filters::over_long_filter(&pattern)]
    );
}

/// `--exclude` / `--include` reach `parse_filter_str` through the same token
/// loop upstream - `XFLG_OLD_PREFIXES` is a flag on it, not a separate path -
/// so the refusal governs them too.
#[test]
fn an_over_long_old_prefix_rule_is_discarded() {
    let _ = drained_messages();
    let pattern = over_long_pattern();

    let directive = parse_old_prefix_rule(&pattern, FilterRuleKind::Exclude, RuleSource::Argument)
        .expect("an over-long --exclude value is discarded, not an error");

    assert_eq!(directive, FilterDirective::Noop);
    assert_eq!(
        drained_messages(),
        vec![filters::over_long_filter(&pattern)]
    );
}

/// WHY: the refusal crosses `rule_text` (`exclude.c:88-123`). A rule read out
/// of a merge file is the peer's choice of content, so the diagnostic names the
/// file, never the text - without this the guard would leak at no verbosity.
#[test]
fn an_over_long_file_sourced_rule_is_reported_without_its_text() {
    let _ = drained_messages();
    let pattern = over_long_pattern();
    let source = RuleSource::File {
        name: ".rsync-filter",
        line: 1,
    };

    let directive = parse_filter_directive(OsStr::new(&format!("- {pattern}")), source)
        .expect("an over-long rule is discarded, not a syntax error");

    assert_eq!(directive, FilterDirective::Noop);
    let messages = drained_messages();
    assert_eq!(
        messages,
        vec!["discarding over-long filter: <rule from .rsync-filter line 1>".to_owned()],
    );
    assert!(!messages[0].contains('Q'));
}
