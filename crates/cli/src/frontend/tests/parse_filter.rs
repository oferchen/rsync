use super::common::*;
use super::*;

#[test]
fn parse_filter_directive_accepts_include_and_exclude() {
    let include = parse_filter_directive(OsStr::new("+ assets/**"), filters::RuleSource::Argument)
        .expect("include rule parses");
    assert_eq!(
        include,
        FilterDirective::Rule(FilterRuleSpec::include("assets/**".to_owned()))
    );

    let exclude = parse_filter_directive(OsStr::new("- *.bak"), filters::RuleSource::Argument)
        .expect("exclude rule parses");
    assert_eq!(
        exclude,
        FilterDirective::Rule(FilterRuleSpec::exclude("*.bak".to_owned()))
    );

    let include_keyword =
        parse_filter_directive(OsStr::new("include logs/**"), filters::RuleSource::Argument)
            .expect("keyword include parses");
    assert_eq!(
        include_keyword,
        FilterDirective::Rule(FilterRuleSpec::include("logs/**".to_owned()))
    );

    let exclude_keyword =
        parse_filter_directive(OsStr::new("exclude *.tmp"), filters::RuleSource::Argument)
            .expect("keyword exclude parses");
    assert_eq!(
        exclude_keyword,
        FilterDirective::Rule(FilterRuleSpec::exclude("*.tmp".to_owned()))
    );

    let protect_keyword = parse_filter_directive(
        OsStr::new("protect backups/**"),
        filters::RuleSource::Argument,
    )
    .expect("keyword protect parses");
    assert_eq!(
        protect_keyword,
        FilterDirective::Rule(FilterRuleSpec::protect("backups/**".to_owned()))
    );
}

#[test]
fn parse_filter_directive_accepts_hide_and_show_keywords() {
    let show_keyword =
        parse_filter_directive(OsStr::new("show images/**"), filters::RuleSource::Argument)
            .expect("keyword show parses");
    assert_eq!(
        show_keyword,
        FilterDirective::Rule(FilterRuleSpec::show("images/**".to_owned()))
    );

    let hide_keyword =
        parse_filter_directive(OsStr::new("hide *.swp"), filters::RuleSource::Argument)
            .expect("keyword hide parses");
    assert_eq!(
        hide_keyword,
        FilterDirective::Rule(FilterRuleSpec::hide("*.swp".to_owned()))
    );
}

#[test]
fn parse_filter_directive_accepts_risk_keyword_and_shorthand() {
    let risk_keyword =
        parse_filter_directive(OsStr::new("risk backups/**"), filters::RuleSource::Argument)
            .expect("keyword risk parses");
    assert_eq!(
        risk_keyword,
        FilterDirective::Rule(FilterRuleSpec::risk("backups/**".to_owned()))
    );

    let risk_shorthand =
        parse_filter_directive(OsStr::new("R logs/**"), filters::RuleSource::Argument)
            .expect("shorthand risk parses");
    assert_eq!(
        risk_shorthand,
        FilterDirective::Rule(FilterRuleSpec::risk("logs/**".to_owned()))
    );
}

#[test]
fn parse_filter_directive_accepts_shorthand_hide_show_and_protect() {
    let protect = parse_filter_directive(OsStr::new("P backups/**"), filters::RuleSource::Argument)
        .expect("shorthand protect parses");
    assert_eq!(
        protect,
        FilterDirective::Rule(FilterRuleSpec::protect("backups/**".to_owned()))
    );

    let hide = parse_filter_directive(OsStr::new("H *.tmp"), filters::RuleSource::Argument)
        .expect("shorthand hide parses");
    assert_eq!(
        hide,
        FilterDirective::Rule(FilterRuleSpec::hide("*.tmp".to_owned()))
    );

    let show = parse_filter_directive(OsStr::new("S public/**"), filters::RuleSource::Argument)
        .expect("shorthand show parses");
    assert_eq!(
        show,
        FilterDirective::Rule(FilterRuleSpec::show("public/**".to_owned()))
    );
}

#[test]
fn parse_filter_directive_accepts_exclude_if_present() {
    let directive = parse_filter_directive(
        OsStr::new("exclude-if-present marker"),
        filters::RuleSource::Argument,
    )
    .expect("exclude-if-present with whitespace parses");
    assert_eq!(
        directive,
        FilterDirective::Rule(FilterRuleSpec::exclude_if_present("marker".to_owned()))
    );

    let equals_variant = parse_filter_directive(
        OsStr::new("exclude-if-present=.skip"),
        filters::RuleSource::Argument,
    )
    .expect("exclude-if-present with equals parses");
    assert_eq!(
        equals_variant,
        FilterDirective::Rule(FilterRuleSpec::exclude_if_present(".skip".to_owned()))
    );
}

#[test]
fn parse_filter_directive_rejects_exclude_if_present_without_marker() {
    let error = parse_filter_directive(
        OsStr::new("exclude-if-present   "),
        filters::RuleSource::Argument,
    )
    .expect_err("missing marker should error");
    let rendered = error.to_string();
    assert!(rendered.contains("missing a marker file"));
}

#[test]
fn parse_filter_directive_accepts_clear_directive() {
    let clear = parse_filter_directive(OsStr::new("!"), filters::RuleSource::Argument)
        .expect("clear directive parses");
    assert_eq!(clear, FilterDirective::Clear);

    // upstream: exclude.c:1211-1213 - leading whitespace is not skipped for a
    // top-level rule, so `  !   ` reaches the prefix `switch` default and errors
    // ("Unknown filter rule") rather than being treated as a clear.
    let _ = parse_filter_directive(OsStr::new("  !   "), filters::RuleSource::Argument)
        .expect_err("leading whitespace should error");
}

#[test]
fn parse_filter_directive_accepts_clear_keyword() {
    let keyword = parse_filter_directive(OsStr::new("clear"), filters::RuleSource::Argument)
        .expect("keyword parses");
    assert_eq!(keyword, FilterDirective::Clear);

    // upstream: exclude.c:1139 RULE_STRCMP(s, "clear") is a case-sensitive
    // strncmp reached only via `case 'c'`, so `CLEAR` misses it, reaches the
    // inner switch default, and errors with "Unknown filter rule". It is not a
    // clear directive.
    let _ = parse_filter_directive(OsStr::new("CLEAR"), filters::RuleSource::Argument)
        .expect_err("uppercase should error");

    // upstream: exclude.c:1211-1213 - a leading space errors before any keyword
    // is recognised, and the keyword is case-sensitive besides; neither the
    // whitespace nor the case is normalised away.
    let _ = parse_filter_directive(OsStr::new("  CLEAR  "), filters::RuleSource::Argument)
        .expect_err("surrounding whitespace should error");
}

#[test]
fn parse_filter_directive_rejects_clear_with_trailing_characters() {
    let error = parse_filter_directive(OsStr::new("! comment"), filters::RuleSource::Argument)
        .expect_err("trailing text should error");
    let rendered = error.to_string();
    assert!(rendered.contains("'!' rule has trailing characters: ! comment"));

    let error = parse_filter_directive(OsStr::new("!extra"), filters::RuleSource::Argument)
        .expect_err("suffix should error");
    let rendered = error.to_string();
    assert!(rendered.contains("'!' rule has trailing characters: !extra"));
}

#[test]
fn parse_filter_directive_rejects_missing_pattern() {
    // upstream: exclude.c:1290-1291,1326 - exactly one separator is consumed
    // after the rule char, so an empty remainder (`+ ` / `P `) is a missing
    // pattern ("unexpected end of filter rule"). A whitespace-only remainder is
    // NOT empty: `+   ` keeps the two extra spaces as the pattern "  " and is
    // accepted (verified against rsync 3.4.4), so only the single-separator
    // forms error here.
    // upstream: exclude.c:1475 filter_rule_err("unexpected end of filter rule",
    // *rulestr_ptr), rendered through rule_text (exclude.c:88-123). An argument
    // is echoed verbatim, trailing space included.
    let error = parse_filter_directive(OsStr::new("+ "), filters::RuleSource::Argument)
        .expect_err("missing pattern should error");
    let rendered = error.to_string();
    assert!(rendered.contains("unexpected end of filter rule: + "));

    let shorthand_error = parse_filter_directive(OsStr::new("P "), filters::RuleSource::Argument)
        .expect_err("shorthand protect requires pattern");
    let rendered = shorthand_error.to_string();
    assert!(rendered.contains("unexpected end of filter rule: P "));

    // A whitespace-only remainder is a valid (if unusual) pattern, not an error.
    let accepted = parse_filter_directive(OsStr::new("+   "), filters::RuleSource::Argument)
        .expect("whitespace pattern is accepted");
    assert_eq!(
        accepted,
        FilterDirective::Rule(FilterRuleSpec::include("  ".to_owned()))
    );
}

#[test]
fn parse_filter_directive_accepts_merge() {
    let directive = parse_filter_directive(
        OsStr::new("merge filters.txt"),
        filters::RuleSource::Argument,
    )
    .expect("merge directive");
    let (options, _) = parse_merge_modifiers("", "merge filters.txt", false).expect("modifiers");
    let expected = MergeDirective::new(OsString::from("filters.txt"), None).with_options(options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_rejects_merge_without_path() {
    // upstream: exclude.c:1475 - a merge with no file name is `!len`, the same
    // "unexpected end of filter rule" as a bare `-`.
    let error = parse_filter_directive(OsStr::new("merge "), filters::RuleSource::Argument)
        .expect_err("missing merge path should error");
    let rendered = error.to_string();
    assert!(rendered.contains("unexpected end of filter rule: merge "));
}

#[test]
fn parse_filter_directive_accepts_merge_with_forced_include() {
    let directive =
        parse_filter_directive(OsStr::new("merge,+ rules"), filters::RuleSource::Argument)
            .expect("merge,+ should parse");
    let (options, _) = parse_merge_modifiers("+", "merge,+ rules", false).expect("modifiers");
    let expected = MergeDirective::new(OsString::from("rules"), Some(FilterRuleKind::Include))
        .with_options(options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_accepts_merge_with_forced_exclude() {
    let directive =
        parse_filter_directive(OsStr::new("merge,- rules"), filters::RuleSource::Argument)
            .expect("merge,- should parse");
    let (options, _) = parse_merge_modifiers("-", "merge,- rules", false).expect("modifiers");
    let expected = MergeDirective::new(OsString::from("rules"), Some(FilterRuleKind::Exclude))
        .with_options(options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_accepts_xattr_only_rules() {
    let include = parse_filter_directive(OsStr::new("+x user.keep"), filters::RuleSource::Argument)
        .expect("short include with xattr modifier parses");
    assert_eq!(
        include,
        FilterDirective::Rule(
            FilterRuleSpec::include("user.keep".to_owned()).with_xattr_only(true)
        )
    );

    let exclude = parse_filter_directive(OsStr::new("-x user.skip"), filters::RuleSource::Argument)
        .expect("short exclude with xattr modifier parses");
    assert_eq!(
        exclude,
        FilterDirective::Rule(
            FilterRuleSpec::exclude("user.skip".to_owned()).with_xattr_only(true)
        )
    );

    let keyword = parse_filter_directive(
        OsStr::new("include,x user.keep"),
        filters::RuleSource::Argument,
    )
    .expect("keyword include with xattr modifier parses");
    assert_eq!(
        keyword,
        FilterDirective::Rule(
            FilterRuleSpec::include("user.keep".to_owned()).with_xattr_only(true)
        )
    );
}

#[test]
fn parse_filter_directive_accepts_xattr_on_side_bound_rules_keeping_the_side() {
    // MEASURED against rsync 3.5.0: `protect,x`, `show,x`, `Px`, `Sx` all exit 0.
    // upstream: exclude.c:1438 `case 'x'` carries no guard, so it is legal on every
    // prefix, and :1345-1358 shows H/S/P/R are nothing but (include|exclude) x
    // (sender|receiver) - the `x` bit is orthogonal and must not widen the side.
    let protect = parse_filter_directive(
        OsStr::new("protect,x secrets"),
        filters::RuleSource::Argument,
    )
    .expect("upstream accepts protect,x");
    assert_eq!(
        protect,
        FilterDirective::Rule(FilterRuleSpec::protect("secrets".to_owned()).with_xattr_only(true))
    );

    let show = parse_filter_directive(OsStr::new("show,x meta"), filters::RuleSource::Argument)
        .expect("upstream accepts show,x");
    assert_eq!(
        show,
        FilterDirective::Rule(FilterRuleSpec::show("meta".to_owned()).with_xattr_only(true))
    );

    // The single-letter forms must agree with their long spellings; upstream
    // erases the distinction before transmission (exclude.c:1824-1881
    // get_rule_prefix never emits H/S/P/R).
    assert_eq!(
        parse_filter_directive(OsStr::new("Px secrets"), filters::RuleSource::Argument)
            .expect("upstream accepts Px"),
        parse_filter_directive(
            OsStr::new("protect,x secrets"),
            filters::RuleSource::Argument
        )
        .expect("long form"),
    );
    assert_eq!(
        parse_filter_directive(OsStr::new("Sx meta"), filters::RuleSource::Argument)
            .expect("upstream accepts Sx"),
        parse_filter_directive(OsStr::new("show,x meta"), filters::RuleSource::Argument)
            .expect("long form"),
    );
}

#[test]
fn parse_filter_directive_still_rejects_a_redundant_side_modifier() {
    // Non-vacuity companion: `x` became legal on these rules, but `s`/`r` must
    // stay rejected because the prefix already binds the side
    // (upstream exclude.c:1423-1432, measured: `Ps user.a` exits 1).
    let error = parse_filter_directive(OsStr::new("Ps secrets"), filters::RuleSource::Argument)
        .expect_err("s is redundant once P binds the side");
    assert!(error.to_string().contains("unsupported modifier 's'"));
}

#[test]
fn parse_filter_directive_accepts_merge_with_cvs_alias() {
    let directive = parse_filter_directive(OsStr::new("merge,C"), filters::RuleSource::Argument)
        .expect("merge,C should parse");
    let (options, _) = parse_merge_modifiers("C", "merge,C", false).expect("modifiers");
    let expected = MergeDirective::new(OsString::from(".cvsignore"), Some(FilterRuleKind::Exclude))
        .with_options(options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_accepts_short_merge() {
    let directive = parse_filter_directive(OsStr::new(". per-dir"), filters::RuleSource::Argument)
        .expect("short merge directive parses");
    let (options, _) = parse_merge_modifiers("", ". per-dir", false).expect("modifiers");
    let expected = MergeDirective::new(OsString::from("per-dir"), None).with_options(options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_accepts_short_merge_with_cvs_alias() {
    let directive = parse_filter_directive(OsStr::new(".C"), filters::RuleSource::Argument)
        .expect("short merge directive with 'C' parses");
    let (options, _) = parse_merge_modifiers("C", ".C", false).expect("modifiers");
    let expected = MergeDirective::new(OsString::from(".cvsignore"), Some(FilterRuleKind::Exclude))
        .with_options(options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_accepts_merge_sender_modifier() {
    let directive =
        parse_filter_directive(OsStr::new("merge,s rules"), filters::RuleSource::Argument)
            .expect("merge directive with 's' parses");
    let expected_options = DirMergeOptions::default()
        .allow_list_clearing(true)
        .sender_modifier();
    let expected =
        MergeDirective::new(OsString::from("rules"), None).with_options(expected_options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_accepts_merge_anchor_and_whitespace_modifiers() {
    let directive = parse_filter_directive(
        OsStr::new("merge,/w patterns"),
        filters::RuleSource::Argument,
    )
    .expect("merge directive with '/' and 'w' parses");
    let expected_options = DirMergeOptions::default()
        .allow_list_clearing(true)
        .anchor_root(true)
        .use_whitespace()
        .allow_comments(false);
    let expected =
        MergeDirective::new(OsString::from("patterns"), None).with_options(expected_options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_rejects_merge_with_unknown_modifier() {
    // `z` is outside upstream's modifier set `- + / ! C e n p r s w x`
    // (exclude.c:1381-1441); `x` is in it and must parse.
    let error = parse_filter_directive(OsStr::new("merge,z rules"), filters::RuleSource::Argument)
        .expect_err("merge with unsupported modifier should error");
    let rendered = error.to_string();
    assert!(rendered.contains("uses unsupported modifier"));
}

#[test]
fn parse_filter_directive_unknown_rule_mirrors_upstream_wording() {
    // upstream: exclude.c:1363 filter_rule_err("Unknown filter rule",
    // *rulestr_ptr), rendered through rule_text (exclude.c:88-123) and exiting
    // RERR_SYNTAX. An argument is echoed verbatim so a typo is easy to fix.
    let error = parse_filter_directive(OsStr::new("Zpat"), filters::RuleSource::Argument)
        .expect_err("unknown prefix should error");
    assert!(
        error.to_string().contains("Unknown filter rule: Zpat"),
        "argument-sourced: {error}"
    );

    // File-sourced: rule_text replaces the peer-chosen line so the diagnostic
    // cannot echo a merged file's contents back (exclude.c:103-124).
    let file_error = parse_filter_directive(
        OsStr::new("Zpat"),
        filters::RuleSource::File {
            name: "m.rules",
            line: 3,
        },
    )
    .expect_err("unknown prefix from a file should error");
    assert!(
        file_error
            .to_string()
            .contains("Unknown filter rule: <rule from m.rules line 3>"),
        "file-sourced: {file_error}"
    );

    // Non-vacuity control: a well-formed rule must still parse, or the two
    // assertions above would pass against a parser that rejects everything.
    parse_filter_directive(OsStr::new("- *.bak"), filters::RuleSource::Argument)
        .expect("a valid exclude must still parse");
}

#[test]
fn parse_filter_directive_unexpected_end_redacts_file_source() {
    // upstream: exclude.c:1475 - an empty pattern from a file is redacted the
    // same way as the "Unknown filter rule" case above.
    let file_error = parse_filter_directive(
        OsStr::new("-"),
        filters::RuleSource::File {
            name: "m.rules",
            line: 1,
        },
    )
    .expect_err("bare '-' from a file should error");
    assert!(
        file_error
            .to_string()
            .contains("unexpected end of filter rule: <rule from m.rules line 1>"),
        "file-sourced: {file_error}"
    );
}

#[test]
fn parse_filter_directive_accepts_dir_merge_without_modifiers() {
    let directive = parse_filter_directive(
        OsStr::new("dir-merge .rsync-filter"),
        filters::RuleSource::Argument,
    )
    .expect("dir-merge without modifiers parses");
    assert_eq!(
        directive,
        FilterDirective::Rule(FilterRuleSpec::dir_merge(
            ".rsync-filter".to_owned(),
            DirMergeOptions::default(),
        )),
    );
}

#[test]
fn parse_filter_directive_rejects_per_dir_alias() {
    // "per-dir" is not an upstream keyword; it must be rejected rather than
    // treated as a dir-merge alias. upstream: exclude.c recognizes only
    // "dir-merge" (case 'd').
    assert!(
        parse_filter_directive(
            OsStr::new("per-dir .rsync-filter"),
            filters::RuleSource::Argument
        )
        .is_err()
    );
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_remove_modifier() {
    let directive = parse_filter_directive(
        OsStr::new("dir-merge,- .rsync-filter"),
        filters::RuleSource::Argument,
    )
    .expect("dir-merge with '-' modifier parses");
    assert_eq!(
        directive,
        FilterDirective::Rule(FilterRuleSpec::dir_merge(
            ".rsync-filter".to_owned(),
            DirMergeOptions::default().with_enforced_kind(Some(DirMergeEnforcedKind::Exclude)),
        ))
    );
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_include_modifier() {
    let directive = parse_filter_directive(
        OsStr::new("dir-merge,+ .rsync-filter"),
        filters::RuleSource::Argument,
    )
    .expect("dir-merge with '+' modifier parses");

    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };

    assert_eq!(rule.pattern(), ".rsync-filter");
    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Include));
    assert!(options.inherit_rules());
    assert!(!options.excludes_self());
}

#[test]
fn parse_filter_directive_accepts_short_dir_merge() {
    let directive = parse_filter_directive(OsStr::new(": rules"), filters::RuleSource::Argument)
        .expect("short dir-merge directive parses");

    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };

    assert_eq!(rule.pattern(), "rules");
    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert!(options.inherit_rules());
    assert!(!options.excludes_self());
}

#[test]
fn parse_filter_directive_accepts_short_dir_merge_with_exclude_modifier() {
    let directive = parse_filter_directive(OsStr::new(":- per-dir"), filters::RuleSource::Argument)
        .expect("short dir-merge with '-' modifier parses");

    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };

    assert_eq!(rule.pattern(), "per-dir");
    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_no_inherit_modifier() {
    let directive = parse_filter_directive(
        OsStr::new("dir-merge,n per-dir"),
        filters::RuleSource::Argument,
    )
    .expect("dir-merge with 'n' modifier parses");

    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };

    assert_eq!(rule.pattern(), "per-dir");
    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert!(!options.inherit_rules());
    assert!(options.allows_comments());
    assert!(!options.uses_whitespace());
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_exclude_self_modifier() {
    let directive = parse_filter_directive(
        OsStr::new("dir-merge,e per-dir"),
        filters::RuleSource::Argument,
    )
    .expect("dir-merge with 'e' modifier parses");

    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };

    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert!(options.excludes_self());
    assert!(options.inherit_rules());
    assert!(!options.uses_whitespace());
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_whitespace_modifier() {
    let directive = parse_filter_directive(
        OsStr::new("dir-merge,w per-dir"),
        filters::RuleSource::Argument,
    )
    .expect("dir-merge with 'w' modifier parses");

    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };

    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert!(options.uses_whitespace());
    assert!(!options.allows_comments());
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_cvs_modifier() {
    let directive =
        parse_filter_directive(OsStr::new("dir-merge,C"), filters::RuleSource::Argument)
            .expect("dir-merge with 'C' modifier parses");

    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };

    assert_eq!(rule.pattern(), ".cvsignore");
    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
    assert!(options.uses_whitespace());
    assert!(!options.allows_comments());
    assert!(!options.inherit_rules());
    assert!(options.list_clear_allowed());
}

#[test]
fn parse_filter_directive_rejects_dir_merge_with_conflicting_modifiers() {
    let error = parse_filter_directive(
        OsStr::new("dir-merge,+- per-dir"),
        filters::RuleSource::Argument,
    )
    .expect_err("conflicting modifiers should error");
    let rendered = error.to_string();
    assert!(rendered.contains("cannot combine '+' and '-'"));
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_sender_modifier() {
    let directive = parse_filter_directive(
        OsStr::new("dir-merge,s per-dir"),
        filters::RuleSource::Argument,
    )
    .expect("dir-merge with 's' modifier parses");
    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };
    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert!(options.applies_to_sender());
    assert!(!options.applies_to_receiver());
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_receiver_modifier() {
    let directive = parse_filter_directive(
        OsStr::new("dir-merge,r per-dir"),
        filters::RuleSource::Argument,
    )
    .expect("dir-merge with 'r' modifier parses");
    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };
    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert!(!options.applies_to_sender());
    assert!(options.applies_to_receiver());
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_anchor_modifier() {
    let directive = parse_filter_directive(
        OsStr::new("dir-merge,/ .rules"),
        filters::RuleSource::Argument,
    )
    .expect("dir-merge with '/' modifier parses");
    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };
    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert!(options.anchor_root_enabled());
}
