//! Unit tests for the filter-rule parsing submodules.

use super::*;

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

#[test]
fn dir_merge_leading_slash_strips_filename_without_anchoring_rules() {
    // upstream: exclude.c:599-617 parse_merge_name - the leading '/' on a merge
    // FILENAME (as generated by `-F` => `dir-merge /.rsync-filter`) only affects
    // where the merge file is located; it is stripped from the name and must NOT
    // anchor the rules loaded from that file. Anchoring is per-rule in add_rule
    // and driven by the `/` MODIFIER, not the filename slash. Setting anchor_root
    // here regressed the filter-depth test: `- secret*` in `d1/d2/.rsync-filter`
    // became `/d1/d2/secret*` and stopped matching `d1/d2/d3/secret.deeper`.
    let result = parse_dir_merge_alias("dir-merge /.rsync-filter");
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
    let result = parse_dir_merge_alias("dir-merge,/ .rsync-filter");
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

#[test]
fn leading_whitespace_is_rejected() {
    // upstream: exclude.c:1100-1213 parse_rule_tok - a top-level rule never
    // carries FILTRULE_WORD_SPLIT, so leading whitespace is not skipped; it
    // reaches the prefix `switch` default and errors. It must not be trimmed.
    let result = parse_filter_directive(OsStr::new("   + *.txt"));
    assert!(result.is_err());
}

#[test]
fn trailing_whitespace_is_kept_in_pattern() {
    // upstream: exclude.c:1313 - trailing whitespace is part of the pattern
    // (strlen length), so `- *.o ` keeps its trailing space and `x.o` stays
    // included.
    let result = parse_filter_directive(OsStr::new("- *.o "));
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
    match parse_filter_directive(OsStr::new("+   *.txt")).unwrap() {
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
    match parse_filter_directive(OsStr::new("-  x")).unwrap() {
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
    match parse_filter_directive(OsStr::new("- x")).unwrap() {
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
    match parse_filter_directive(OsStr::new("-_ x")).unwrap() {
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
    match parse_keyword_rule("exclude   x").unwrap() {
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
    match parse_shorthand_rules("P  x").unwrap().unwrap() {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Protect);
            assert_eq!(spec.pattern(), " x");
        }
        other => panic!("expected Rule directive, got {other:?}"),
    }
}

#[test]
fn exclude_negate_modifier_short() {
    let result = parse_filter_directive(OsStr::new("-! */"));
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
    let result = parse_filter_directive(OsStr::new("exclude,! */"));
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
    let result = parse_filter_directive(OsStr::new("+! *.txt"));
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
    let result = parse_old_prefix_rule("- to", FilterRuleKind::Include).unwrap();
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
    let result = parse_old_prefix_rule("+ *.rs", FilterRuleKind::Exclude).unwrap();
    match result {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Include);
            assert_eq!(spec.pattern(), "*.rs");
        }
        other => panic!("expected Rule, got {other:?}"),
    }
}

#[test]
fn old_prefix_bang_emits_clear() {
    assert!(matches!(
        parse_old_prefix_rule("!", FilterRuleKind::Exclude).unwrap(),
        FilterDirective::Clear
    ));
    assert!(matches!(
        parse_old_prefix_rule("!   ", FilterRuleKind::Exclude).unwrap(),
        FilterDirective::Clear
    ));
}

#[test]
fn old_prefix_bang_with_pattern_is_raw_pattern() {
    // upstream: `!pattern` (no space) is NOT a clear - it's the raw
    // pattern because XFLG_OLD_PREFIXES only recognizes `!` as clear
    // when followed by whitespace or end-of-line.
    let result = parse_old_prefix_rule("!keepme", FilterRuleKind::Exclude).unwrap();
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
    let result = parse_old_prefix_rule("*.log", FilterRuleKind::Include).unwrap();
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
    let result = parse_old_prefix_rule("-foo", FilterRuleKind::Exclude).unwrap();
    match result {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Exclude);
            assert_eq!(spec.pattern(), "-foo");
        }
        other => panic!("expected Rule, got {other:?}"),
    }
}

#[test]
fn old_prefix_empty_is_error() {
    assert!(parse_old_prefix_rule("", FilterRuleKind::Exclude).is_err());
}

#[test]
fn old_prefix_short_prefix_only_is_error() {
    // upstream: `parse_rule_tok` reports "unexpected end of filter rule"
    // when no pattern follows the prefix.
    assert!(parse_old_prefix_rule("- ", FilterRuleKind::Include).is_err());
    assert!(parse_old_prefix_rule("+ ", FilterRuleKind::Exclude).is_err());
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
        parse_filter_directive(OsStr::new("-C")).unwrap(),
        FilterDirective::CvsDefaults
    );
    assert_eq!(
        parse_filter_directive(OsStr::new("+C")).unwrap(),
        FilterDirective::CvsDefaults
    );
    assert_eq!(
        parse_filter_directive(OsStr::new("-,C")).unwrap(),
        FilterDirective::CvsDefaults
    );
}

#[test]
fn parse_literal_dash_pattern_is_not_cvs() {
    // `- C` (with a space) is an ordinary exclude of the pattern "C", not
    // the cvs-convenience rule.
    match parse_filter_directive(OsStr::new("- C")).unwrap() {
        FilterDirective::Rule(spec) => {
            assert_eq!(spec.kind(), FilterRuleKind::Exclude);
            assert_eq!(spec.pattern(), "C");
        }
        other => panic!("expected exclude Rule, got {other:?}"),
    }
}
