use super::common::*;
use super::*;

#[test]
fn parse_filter_directive_accepts_include_and_exclude() {
    let include = parse_filter_directive(OsStr::new("+ assets/**")).expect("include rule parses");
    assert_eq!(
        include,
        FilterDirective::Rule(FilterRuleSpec::include("assets/**".to_owned()))
    );

    let exclude = parse_filter_directive(OsStr::new("- *.bak")).expect("exclude rule parses");
    assert_eq!(
        exclude,
        FilterDirective::Rule(FilterRuleSpec::exclude("*.bak".to_owned()))
    );

    let include_keyword =
        parse_filter_directive(OsStr::new("include logs/**")).expect("keyword include parses");
    assert_eq!(
        include_keyword,
        FilterDirective::Rule(FilterRuleSpec::include("logs/**".to_owned()))
    );

    let exclude_keyword =
        parse_filter_directive(OsStr::new("exclude *.tmp")).expect("keyword exclude parses");
    assert_eq!(
        exclude_keyword,
        FilterDirective::Rule(FilterRuleSpec::exclude("*.tmp".to_owned()))
    );

    let protect_keyword =
        parse_filter_directive(OsStr::new("protect backups/**")).expect("keyword protect parses");
    assert_eq!(
        protect_keyword,
        FilterDirective::Rule(FilterRuleSpec::protect("backups/**".to_owned()))
    );
}

#[test]
fn parse_filter_directive_accepts_hide_and_show_keywords() {
    let show_keyword =
        parse_filter_directive(OsStr::new("show images/**")).expect("keyword show parses");
    assert_eq!(
        show_keyword,
        FilterDirective::Rule(FilterRuleSpec::show("images/**".to_owned()))
    );

    let hide_keyword =
        parse_filter_directive(OsStr::new("hide *.swp")).expect("keyword hide parses");
    assert_eq!(
        hide_keyword,
        FilterDirective::Rule(FilterRuleSpec::hide("*.swp".to_owned()))
    );
}

#[test]
fn parse_filter_directive_accepts_risk_keyword_and_shorthand() {
    let risk_keyword =
        parse_filter_directive(OsStr::new("risk backups/**")).expect("keyword risk parses");
    assert_eq!(
        risk_keyword,
        FilterDirective::Rule(FilterRuleSpec::risk("backups/**".to_owned()))
    );

    let risk_shorthand =
        parse_filter_directive(OsStr::new("R logs/**")).expect("shorthand risk parses");
    assert_eq!(
        risk_shorthand,
        FilterDirective::Rule(FilterRuleSpec::risk("logs/**".to_owned()))
    );
}

#[test]
fn parse_filter_directive_accepts_shorthand_hide_show_and_protect() {
    let protect =
        parse_filter_directive(OsStr::new("P backups/**")).expect("shorthand protect parses");
    assert_eq!(
        protect,
        FilterDirective::Rule(FilterRuleSpec::protect("backups/**".to_owned()))
    );

    let hide = parse_filter_directive(OsStr::new("H *.tmp")).expect("shorthand hide parses");
    assert_eq!(
        hide,
        FilterDirective::Rule(FilterRuleSpec::hide("*.tmp".to_owned()))
    );

    let show = parse_filter_directive(OsStr::new("S public/**")).expect("shorthand show parses");
    assert_eq!(
        show,
        FilterDirective::Rule(FilterRuleSpec::show("public/**".to_owned()))
    );
}

#[test]
fn parse_filter_directive_accepts_exclude_if_present() {
    let directive = parse_filter_directive(OsStr::new("exclude-if-present marker"))
        .expect("exclude-if-present with whitespace parses");
    assert_eq!(
        directive,
        FilterDirective::Rule(FilterRuleSpec::exclude_if_present("marker".to_owned()))
    );

    let equals_variant = parse_filter_directive(OsStr::new("exclude-if-present=.skip"))
        .expect("exclude-if-present with equals parses");
    assert_eq!(
        equals_variant,
        FilterDirective::Rule(FilterRuleSpec::exclude_if_present(".skip".to_owned()))
    );
}

#[test]
fn parse_filter_directive_rejects_exclude_if_present_without_marker() {
    let error = parse_filter_directive(OsStr::new("exclude-if-present   "))
        .expect_err("missing marker should error");
    let rendered = error.to_string();
    assert!(rendered.contains("missing a marker file"));
}

#[test]
fn parse_filter_directive_accepts_clear_directive() {
    let clear = parse_filter_directive(OsStr::new("!")).expect("clear directive parses");
    assert_eq!(clear, FilterDirective::Clear);

    let clear_with_whitespace =
        parse_filter_directive(OsStr::new("  !   ")).expect("clear with whitespace parses");
    assert_eq!(clear_with_whitespace, FilterDirective::Clear);
}

#[test]
fn parse_filter_directive_accepts_clear_keyword() {
    let keyword = parse_filter_directive(OsStr::new("clear")).expect("keyword parses");
    assert_eq!(keyword, FilterDirective::Clear);

    let uppercase = parse_filter_directive(OsStr::new("  CLEAR  ")).expect("uppercase parses");
    assert_eq!(uppercase, FilterDirective::Clear);
}

#[test]
fn parse_filter_directive_rejects_clear_with_trailing_characters() {
    let error =
        parse_filter_directive(OsStr::new("! comment")).expect_err("trailing text should error");
    let rendered = error.to_string();
    assert!(rendered.contains("'!' rule has trailing characters: ! comment"));

    let error = parse_filter_directive(OsStr::new("!extra")).expect_err("suffix should error");
    let rendered = error.to_string();
    assert!(rendered.contains("'!' rule has trailing characters: !extra"));
}

#[test]
fn parse_filter_directive_rejects_missing_pattern() {
    let error =
        parse_filter_directive(OsStr::new("+   ")).expect_err("missing pattern should error");
    let rendered = error.to_string();
    assert!(rendered.contains("missing a pattern"));

    let shorthand_error =
        parse_filter_directive(OsStr::new("P   ")).expect_err("shorthand protect requires pattern");
    let rendered = shorthand_error.to_string();
    assert!(rendered.contains("missing a pattern"));
}

#[test]
fn parse_filter_directive_accepts_merge() {
    let directive =
        parse_filter_directive(OsStr::new("merge filters.txt")).expect("merge directive");
    let (options, _) = parse_merge_modifiers("", "merge filters.txt", false).expect("modifiers");
    let expected = MergeDirective::new(OsString::from("filters.txt"), None).with_options(options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_rejects_merge_without_path() {
    let error =
        parse_filter_directive(OsStr::new("merge ")).expect_err("missing merge path should error");
    let rendered = error.to_string();
    assert!(rendered.contains("missing a file path"));
}

#[test]
fn parse_filter_directive_accepts_merge_with_forced_include() {
    let directive =
        parse_filter_directive(OsStr::new("merge,+ rules")).expect("merge,+ should parse");
    let (options, _) = parse_merge_modifiers("+", "merge,+ rules", false).expect("modifiers");
    let expected = MergeDirective::new(OsString::from("rules"), Some(FilterRuleKind::Include))
        .with_options(options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_accepts_merge_with_forced_exclude() {
    let directive =
        parse_filter_directive(OsStr::new("merge,- rules")).expect("merge,- should parse");
    let (options, _) = parse_merge_modifiers("-", "merge,- rules", false).expect("modifiers");
    let expected = MergeDirective::new(OsString::from("rules"), Some(FilterRuleKind::Exclude))
        .with_options(options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_accepts_xattr_only_rules() {
    let include = parse_filter_directive(OsStr::new("+x user.keep"))
        .expect("short include with xattr modifier parses");
    assert_eq!(
        include,
        FilterDirective::Rule(
            FilterRuleSpec::include("user.keep".to_owned()).with_xattr_only(true)
        )
    );

    let exclude = parse_filter_directive(OsStr::new("-x user.skip"))
        .expect("short exclude with xattr modifier parses");
    assert_eq!(
        exclude,
        FilterDirective::Rule(
            FilterRuleSpec::exclude("user.skip".to_owned()).with_xattr_only(true)
        )
    );

    let keyword = parse_filter_directive(OsStr::new("include,x user.keep"))
        .expect("keyword include with xattr modifier parses");
    assert_eq!(
        keyword,
        FilterDirective::Rule(
            FilterRuleSpec::include("user.keep".to_owned()).with_xattr_only(true)
        )
    );
}

#[test]
fn parse_filter_directive_rejects_xattr_on_unsupported_keywords() {
    let protect_error =
        parse_filter_directive(OsStr::new("protect,x secrets")).expect_err("protect,x should fail");
    let rendered = protect_error.to_string();
    assert!(rendered.contains("uses unsupported modifier 'x'"));

    let show_error =
        parse_filter_directive(OsStr::new("show,x meta")).expect_err("show,x should fail");
    let rendered = show_error.to_string();
    assert!(rendered.contains("uses unsupported modifier 'x'"));
}

#[test]
fn parse_filter_directive_accepts_merge_with_cvs_alias() {
    let directive = parse_filter_directive(OsStr::new("merge,C")).expect("merge,C should parse");
    let (options, _) = parse_merge_modifiers("C", "merge,C", false).expect("modifiers");
    let expected = MergeDirective::new(OsString::from(".cvsignore"), Some(FilterRuleKind::Exclude))
        .with_options(options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_accepts_short_merge() {
    let directive =
        parse_filter_directive(OsStr::new(". per-dir")).expect("short merge directive parses");
    let (options, _) = parse_merge_modifiers("", ". per-dir", false).expect("modifiers");
    let expected = MergeDirective::new(OsString::from("per-dir"), None).with_options(options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_accepts_short_merge_with_cvs_alias() {
    let directive =
        parse_filter_directive(OsStr::new(".C")).expect("short merge directive with 'C' parses");
    let (options, _) = parse_merge_modifiers("C", ".C", false).expect("modifiers");
    let expected = MergeDirective::new(OsString::from(".cvsignore"), Some(FilterRuleKind::Exclude))
        .with_options(options);
    assert_eq!(directive, FilterDirective::Merge(expected));
}

#[test]
fn parse_filter_directive_accepts_merge_sender_modifier() {
    let directive = parse_filter_directive(OsStr::new("merge,s rules"))
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
    let directive = parse_filter_directive(OsStr::new("merge,/w patterns"))
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
    let error = parse_filter_directive(OsStr::new("merge,x rules"))
        .expect_err("merge with unsupported modifier should error");
    let rendered = error.to_string();
    assert!(rendered.contains("uses unsupported modifier"));
}

#[test]
fn parse_filter_directive_accepts_dir_merge_without_modifiers() {
    let directive = parse_filter_directive(OsStr::new("dir-merge .rsync-filter"))
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
fn parse_filter_directive_accepts_per_dir_alias() {
    let directive =
        parse_filter_directive(OsStr::new("per-dir .rsync-filter")).expect("per-dir alias parses");
    assert_eq!(
        directive,
        FilterDirective::Rule(FilterRuleSpec::dir_merge(
            ".rsync-filter".to_owned(),
            DirMergeOptions::default(),
        )),
    );
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_remove_modifier() {
    let directive = parse_filter_directive(OsStr::new("dir-merge,- .rsync-filter"))
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
    let directive = parse_filter_directive(OsStr::new("dir-merge,+ .rsync-filter"))
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
    let directive =
        parse_filter_directive(OsStr::new(": rules")).expect("short dir-merge directive parses");

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
    let directive = parse_filter_directive(OsStr::new(":- per-dir"))
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
    let directive = parse_filter_directive(OsStr::new("dir-merge,n per-dir"))
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
    let directive = parse_filter_directive(OsStr::new("dir-merge,e per-dir"))
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
    let directive = parse_filter_directive(OsStr::new("dir-merge,w per-dir"))
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
    let directive = parse_filter_directive(OsStr::new("dir-merge,C"))
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
    let error = parse_filter_directive(OsStr::new("dir-merge,+- per-dir"))
        .expect_err("conflicting modifiers should error");
    let rendered = error.to_string();
    assert!(rendered.contains("cannot combine '+' and '-'"));
}

#[test]
fn parse_filter_directive_accepts_dir_merge_with_sender_modifier() {
    let directive = parse_filter_directive(OsStr::new("dir-merge,s per-dir"))
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
    let directive = parse_filter_directive(OsStr::new("dir-merge,r per-dir"))
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
    let directive = parse_filter_directive(OsStr::new("dir-merge,/ .rules"))
        .expect("dir-merge with '/' modifier parses");
    let FilterDirective::Rule(rule) = directive else {
        panic!("expected dir-merge rule");
    };
    let options = rule
        .dir_merge_options()
        .expect("dir-merge rule returns options");
    assert!(options.anchor_root_enabled());
}
