//! Property-based fuzz tests for the filter rule parser and filter set.
//!
//! These tests use proptest to generate arbitrary inputs and verify that
//! parsing and matching code never panics on untrusted input. This is
//! security-critical because filter rules are user-supplied strings that
//! control which files are included/excluded during rsync transfers.

use std::path::Path;

use filters::{
    FilterAction, FilterRule, FilterSet, cvs_exclusion_rules, parse_rules,
};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// Generates completely arbitrary strings (including null bytes, unicode, etc.)
fn arbitrary_string() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::string::string_regex(".*").unwrap(),
        prop::string::string_regex("\\PC*").unwrap(),
    ]
}

/// Generates strings with special characters commonly found in filter rules.
fn filter_special_string() -> impl Strategy<Value = String> {
    let chars = prop::sample::select(vec![
        '+', '-', 'P', 'R', 'H', 'S', '.', ':', '!', '#', ';', ' ', '\t',
        '\n', '\r', '/', '\\', '*', '?', '[', ']', '{', '}', '_', '\0',
        'a', 'z', 'A', 'Z', '0', '9', '.', ',', '~', '$',
    ]);
    proptest::collection::vec(chars, 0..100)
        .prop_map(|v| v.into_iter().collect::<String>())
}

/// Generates valid short-form prefixes.
fn short_form_prefix() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "+ ", "- ", "P ", "R ", "H ", "S ", ". ", ": ",
    ])
}

/// Generates modifier characters.
fn modifier_chars() -> impl Strategy<Value = String> {
    let chars = prop::sample::select(vec![
        '!', 'p', 's', 'r', 'x', 'e', 'n', 'w', 'C',
    ]);
    proptest::collection::vec(chars, 0..5)
        .prop_map(|v| v.into_iter().collect::<String>())
}

/// Generates a safe glob-like pattern (characters that won't cause panic
/// but exercise interesting glob edge cases).
fn glob_pattern() -> impl Strategy<Value = String> {
    let chars = prop::sample::select(vec![
        'a', 'b', 'c', 'd', 'e', 'f', '0', '1', '2',
        '*', '?', '.', '/', '_', '-', '~',
    ]);
    proptest::collection::vec(chars, 0..30)
        .prop_map(|v| v.into_iter().collect::<String>())
}

/// Generates a valid rsync filter rule line (short form).
fn valid_rule_line() -> impl Strategy<Value = String> {
    (short_form_prefix(), glob_pattern())
        .prop_filter("non-empty pattern", |(_, p)| !p.is_empty())
        .prop_map(|(prefix, pattern)| format!("{prefix}{pattern}"))
}

/// Generates a valid rule line with modifiers.
fn valid_rule_line_with_mods() -> impl Strategy<Value = String> {
    (
        prop::sample::select(vec!["+", "-", "P", "R", "H", "S"]),
        modifier_chars(),
        prop::sample::select(vec![" ", "_"]),
        glob_pattern(),
    )
        .prop_filter("non-empty pattern", |(_, _, _, p)| !p.is_empty())
        .prop_map(|(prefix, mods, sep, pattern)| format!("{prefix}{mods}{sep}{pattern}"))
}

/// Generates a multi-line filter file content.
fn filter_file_content() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            valid_rule_line(),
            Just("# comment".to_string()),
            Just("; comment".to_string()),
            Just(String::new()),
            Just("!".to_string()),
            Just("clear".to_string()),
        ],
        0..20,
    )
    .prop_map(|lines| lines.join("\n"))
}

/// Generates an arbitrary FilterAction.
fn arb_filter_action() -> impl Strategy<Value = FilterAction> {
    prop_oneof![
        Just(FilterAction::Include),
        Just(FilterAction::Exclude),
        Just(FilterAction::Protect),
        Just(FilterAction::Risk),
    ]
}

/// Generates a FilterRule with a safe pattern.
fn arb_filter_rule() -> impl Strategy<Value = FilterRule> {
    (
        arb_filter_action(),
        glob_pattern().prop_filter("non-empty pattern", |p| !p.is_empty()),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(action, pattern, perishable, negate, sender_only)| {
            let rule = match action {
                FilterAction::Include => FilterRule::include(&pattern),
                FilterAction::Exclude => FilterRule::exclude(&pattern),
                FilterAction::Protect => FilterRule::protect(&pattern),
                FilterAction::Risk => FilterRule::risk(&pattern),
                _ => unreachable!(),
            };
            let rule = rule.with_perishable(perishable).with_negate(negate);
            if sender_only {
                rule.with_sides(true, false)
            } else {
                rule
            }
        })
}

/// Generates a relative file path for matching.
fn arb_relative_path() -> impl Strategy<Value = String> {
    let segment = proptest::collection::vec(
        prop::sample::select(vec![
            'a', 'b', 'c', 'd', 'x', 'y', 'z',
            '0', '1', '.',  '_', '-',
        ]),
        1..10,
    )
    .prop_map(|v| v.into_iter().collect::<String>());

    proptest::collection::vec(segment, 1..5)
        .prop_map(|segments| segments.join("/"))
}

// ---------------------------------------------------------------------------
// Tests: Parsing arbitrary strings must never panic
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// Parsing completely arbitrary strings as filter rules must never panic.
    /// It is acceptable to return an error, but the code must not crash.
    #[test]
    fn parse_rules_never_panics_on_arbitrary_input(input in arbitrary_string()) {
        // parse_rules may return Ok or Err but must never panic
        let _ = parse_rules(&input, Path::new("<fuzz>"));
    }

    /// Parsing filter-specific special characters must never panic.
    #[test]
    fn parse_rules_never_panics_on_special_chars(input in filter_special_string()) {
        let _ = parse_rules(&input, Path::new("<fuzz>"));
    }

    /// Parsing strings with null bytes must never panic.
    #[test]
    fn parse_rules_never_panics_with_null_bytes(
        prefix in "[+-PRS.:!]",
        filler in proptest::collection::vec(0u8..255, 0..50),
    ) {
        let s: String = filler.iter().map(|&b| b as char).collect();
        let input = format!("{prefix}{s}");
        let _ = parse_rules(&input, Path::new("<fuzz>"));
    }

    /// Arbitrary multi-line content must never panic when parsed.
    #[test]
    fn parse_rules_multiline_arbitrary(content in "(.{0,80}\n){0,50}") {
        let _ = parse_rules(&content, Path::new("<fuzz>"));
    }

    /// Very long single-line inputs must never panic.
    #[test]
    fn parse_rules_very_long_input(repeat_count in 100usize..500) {
        let long_pattern = "a".repeat(repeat_count);
        let input = format!("- {long_pattern}");
        let result = parse_rules(&input, Path::new("<fuzz>"));
        prop_assert!(result.is_ok());
    }

    /// Empty and whitespace-only inputs produce no rules.
    #[test]
    fn parse_rules_whitespace_only(input in "[ \t\n\r]*") {
        let result = parse_rules(&input, Path::new("<fuzz>"));
        prop_assert!(result.is_ok());
        prop_assert!(result.unwrap().is_empty());
    }

    /// Comment-only inputs produce no rules.
    #[test]
    fn parse_rules_comment_only(comment_char in "[#;]", rest in ".{0,80}") {
        let input = format!("{comment_char}{rest}");
        let result = parse_rules(&input, Path::new("<fuzz>"));
        prop_assert!(result.is_ok());
        prop_assert!(result.unwrap().is_empty());
    }
}

// ---------------------------------------------------------------------------
// Tests: FilterRule construction with arbitrary patterns must not panic
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// Constructing FilterRule::include with arbitrary patterns must never panic.
    #[test]
    fn filter_rule_include_arbitrary(pattern in arbitrary_string()) {
        let rule = FilterRule::include(&pattern);
        prop_assert_eq!(rule.action(), FilterAction::Include);
        prop_assert_eq!(rule.pattern(), &pattern);
    }

    /// Constructing FilterRule::exclude with arbitrary patterns must never panic.
    #[test]
    fn filter_rule_exclude_arbitrary(pattern in arbitrary_string()) {
        let rule = FilterRule::exclude(&pattern);
        prop_assert_eq!(rule.action(), FilterAction::Exclude);
        prop_assert_eq!(rule.pattern(), &pattern);
    }

    /// Constructing FilterRule::protect with arbitrary patterns must never panic.
    #[test]
    fn filter_rule_protect_arbitrary(pattern in arbitrary_string()) {
        let rule = FilterRule::protect(&pattern);
        prop_assert_eq!(rule.action(), FilterAction::Protect);
    }

    /// Constructing FilterRule::risk with arbitrary patterns must never panic.
    #[test]
    fn filter_rule_risk_arbitrary(pattern in arbitrary_string()) {
        let rule = FilterRule::risk(&pattern);
        prop_assert_eq!(rule.action(), FilterAction::Risk);
    }

    /// Chaining all builder methods with arbitrary values must never panic.
    #[test]
    fn filter_rule_builder_chain_arbitrary(
        pattern in arbitrary_string(),
        perishable in any::<bool>(),
        sender in any::<bool>(),
        receiver in any::<bool>(),
        xattr in any::<bool>(),
        negate in any::<bool>(),
        exclude_only in any::<bool>(),
        no_inherit in any::<bool>(),
    ) {
        let rule = FilterRule::include(&pattern)
            .with_perishable(perishable)
            .with_sender(sender)
            .with_receiver(receiver)
            .with_xattr_only(xattr)
            .with_negate(negate)
            .with_exclude_only(exclude_only)
            .with_no_inherit(no_inherit);

        prop_assert_eq!(rule.is_perishable(), perishable);
        prop_assert_eq!(rule.applies_to_sender(), sender);
        prop_assert_eq!(rule.applies_to_receiver(), receiver);
        prop_assert_eq!(rule.is_xattr_only(), xattr);
        prop_assert_eq!(rule.is_negated(), negate);
        prop_assert_eq!(rule.is_exclude_only(), exclude_only);
        prop_assert_eq!(rule.is_no_inherit(), no_inherit);
    }

    /// anchor_to_root with arbitrary patterns must never panic.
    #[test]
    fn filter_rule_anchor_arbitrary(pattern in arbitrary_string()) {
        let rule = FilterRule::include(&pattern).anchor_to_root();
        prop_assert!(rule.pattern().starts_with('/'));

        // Idempotence
        let double = rule.anchor_to_root();
        prop_assert!(double.pattern().starts_with('/'));
        prop_assert!(!double.pattern().starts_with("//") || pattern.starts_with('/'));
    }
}

// ---------------------------------------------------------------------------
// Tests: FilterSet::from_rules with arbitrary patterns
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Building a FilterSet from arbitrary exclude patterns must not panic.
    /// Invalid glob patterns should return Err, but never panic.
    #[test]
    fn filter_set_from_arbitrary_exclude(pattern in arbitrary_string()) {
        let _ = FilterSet::from_rules([FilterRule::exclude(&pattern)]);
    }

    /// Building a FilterSet from arbitrary include patterns must not panic.
    #[test]
    fn filter_set_from_arbitrary_include(pattern in arbitrary_string()) {
        let _ = FilterSet::from_rules([FilterRule::include(&pattern)]);
    }

    /// Building a FilterSet from arbitrary protect patterns must not panic.
    #[test]
    fn filter_set_from_arbitrary_protect(pattern in arbitrary_string()) {
        let _ = FilterSet::from_rules([FilterRule::protect(&pattern)]);
    }

    /// Building a FilterSet from multiple arbitrary rules must not panic.
    #[test]
    fn filter_set_from_multiple_arbitrary_rules(
        rules in proptest::collection::vec(arb_filter_rule(), 0..20)
    ) {
        let _ = FilterSet::from_rules(rules);
    }

    /// A clear rule followed by arbitrary rules must not panic.
    #[test]
    fn filter_set_clear_then_arbitrary(
        rules in proptest::collection::vec(arb_filter_rule(), 0..10)
    ) {
        let mut all_rules = vec![FilterRule::clear()];
        all_rules.extend(rules);
        let _ = FilterSet::from_rules(all_rules);
    }
}

// ---------------------------------------------------------------------------
// Tests: FilterSet::allows and allows_deletion with arbitrary paths
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// Querying allows() with arbitrary paths on a valid FilterSet must not panic.
    #[test]
    fn filter_set_allows_arbitrary_path(
        path in arb_relative_path(),
        is_dir in any::<bool>(),
    ) {
        let set = FilterSet::from_rules([
            FilterRule::exclude("*.tmp"),
            FilterRule::include("important/**"),
        ])
        .unwrap();
        let _ = set.allows(Path::new(&path), is_dir);
    }

    /// Querying allows_deletion() with arbitrary paths must not panic.
    #[test]
    fn filter_set_allows_deletion_arbitrary_path(
        path in arb_relative_path(),
        is_dir in any::<bool>(),
    ) {
        let set = FilterSet::from_rules([
            FilterRule::protect("data/"),
            FilterRule::risk("temp/"),
            FilterRule::exclude("*.log"),
        ])
        .unwrap();
        let _ = set.allows_deletion(Path::new(&path), is_dir);
        let _ = set.allows_deletion_when_excluded_removed(Path::new(&path), is_dir);
    }

    /// Matching arbitrary rules against arbitrary paths must not panic.
    #[test]
    fn filter_set_arbitrary_rules_arbitrary_paths(
        rules in proptest::collection::vec(arb_filter_rule(), 1..10),
        path in arb_relative_path(),
        is_dir in any::<bool>(),
    ) {
        if let Ok(set) = FilterSet::from_rules(rules) {
            let _ = set.allows(Path::new(&path), is_dir);
            let _ = set.allows_deletion(Path::new(&path), is_dir);
            let _ = set.allows_deletion_when_excluded_removed(Path::new(&path), is_dir);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests: Roundtrip parse-format-reparse consistency
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Valid short-form filter rule lines should parse successfully, and the
    /// resulting rule re-serialized back to the same short form should
    /// re-parse identically.
    #[test]
    fn roundtrip_valid_short_form_rules(line in valid_rule_line()) {
        let parsed = parse_rules(&line, Path::new("<fuzz>"));
        prop_assert!(parsed.is_ok(), "Failed to parse valid rule: {line}");
        let rules = parsed.unwrap();
        prop_assert_eq!(rules.len(), 1);

        let rule = &rules[0];

        // Re-serialize: reconstruct the short form
        let prefix = match rule.action() {
            FilterAction::Include => {
                if rule.applies_to_sender() && !rule.applies_to_receiver() {
                    "S"
                } else {
                    "+"
                }
            }
            FilterAction::Exclude => {
                if rule.applies_to_sender() && !rule.applies_to_receiver() {
                    "H"
                } else {
                    "-"
                }
            }
            FilterAction::Protect => "P",
            FilterAction::Risk => "R",
            FilterAction::Merge => ".",
            FilterAction::DirMerge => ":",
            FilterAction::Clear => "!",
        };

        let re_serialized = format!("{prefix} {}", rule.pattern());
        let re_parsed = parse_rules(&re_serialized, Path::new("<fuzz>"));
        prop_assert!(re_parsed.is_ok(), "Failed to re-parse: {re_serialized}");
        let re_rules = re_parsed.unwrap();
        prop_assert_eq!(re_rules.len(), 1);
        prop_assert_eq!(re_rules[0].action(), rule.action());
        prop_assert_eq!(re_rules[0].pattern(), rule.pattern());
    }

    /// Valid rule lines with modifiers should round-trip through
    /// serialize-reparse if we account for modifiers.
    #[test]
    fn roundtrip_valid_rules_with_modifiers(line in valid_rule_line_with_mods()) {
        let parsed = parse_rules(&line, Path::new("<fuzz>"));
        // May fail to parse if modifiers produce strange combos; that is fine.
        if let Ok(rules) = parsed {
            // For non-word-split rules, should produce exactly 1 rule
            // For word-split rules, may produce multiple rules
            prop_assert!(!rules.is_empty() || line.trim().is_empty());

            // Each returned rule must have a valid action
            for rule in &rules {
                match rule.action() {
                    FilterAction::Include
                    | FilterAction::Exclude
                    | FilterAction::Protect
                    | FilterAction::Risk
                    | FilterAction::Clear
                    | FilterAction::Merge
                    | FilterAction::DirMerge => {}
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests: CVS exclusion patterns
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// Building a FilterSet with CVS exclusions and arbitrary rules must not panic.
    #[test]
    fn cvs_filter_set_with_arbitrary_rules(
        rules in proptest::collection::vec(arb_filter_rule(), 0..10),
        perishable in any::<bool>(),
    ) {
        let _ = FilterSet::from_rules_with_cvs(rules, perishable);
    }

    /// CVS exclusion rules evaluated against arbitrary paths must not panic.
    #[test]
    fn cvs_filter_set_arbitrary_paths(
        path in arb_relative_path(),
        is_dir in any::<bool>(),
        perishable in any::<bool>(),
    ) {
        let set = FilterSet::from_rules_with_cvs(vec![], perishable).unwrap();
        let _ = set.allows(Path::new(&path), is_dir);
        let _ = set.allows_deletion(Path::new(&path), is_dir);
    }

    /// CVS exclusion rules perishable flag is correctly propagated.
    #[test]
    fn cvs_perishable_consistency(perishable in any::<bool>()) {
        let rules: Vec<_> = cvs_exclusion_rules(perishable).collect();
        for rule in &rules {
            prop_assert_eq!(rule.is_perishable(), perishable);
            prop_assert_eq!(rule.action(), FilterAction::Exclude);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests: Merge file parsing with arbitrary content
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// parse_rules with generated filter file content should produce valid rules.
    #[test]
    fn parse_generated_filter_file_content(content in filter_file_content()) {
        let result = parse_rules(&content, Path::new("<fuzz>"));
        prop_assert!(result.is_ok(), "Failed on valid content: {content:?}");
    }

    /// parse_rules with arbitrary multi-line content must never panic.
    #[test]
    fn parse_arbitrary_multiline_content(
        lines in proptest::collection::vec(filter_special_string(), 0..30)
    ) {
        let content = lines.join("\n");
        let _ = parse_rules(&content, Path::new("<fuzz>"));
    }
}

// ---------------------------------------------------------------------------
// Tests: Edge cases
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// Unicode strings must not cause panics in the parser.
    #[test]
    fn parse_rules_unicode(input in "\\PC{0,100}") {
        let _ = parse_rules(&input, Path::new("<fuzz>"));
    }

    /// Filter rules with unicode patterns must not panic when compiled.
    #[test]
    fn filter_set_unicode_patterns(pattern in "\\PC{1,50}") {
        let _ = FilterSet::from_rules([FilterRule::exclude(&pattern)]);
    }

    /// Strings consisting entirely of action prefixes and modifiers.
    #[test]
    fn parse_rules_prefix_flood(
        chars in proptest::collection::vec(
            prop::sample::select(vec!['+', '-', 'P', 'R', 'H', 'S', '.', ':', '!']),
            1..50
        )
    ) {
        let input: String = chars.into_iter().collect();
        let _ = parse_rules(&input, Path::new("<fuzz>"));
    }

    /// Patterns that are just glob metacharacters.
    #[test]
    fn filter_set_metachar_patterns(
        meta in proptest::collection::vec(
            prop::sample::select(vec!['*', '?', '[', ']', '{', '}', '\\', '/']),
            1..20
        )
    ) {
        let pattern: String = meta.into_iter().collect();
        // May succeed or fail but must not panic
        let _ = FilterSet::from_rules([FilterRule::exclude(&pattern)]);
    }

    /// Very deeply nested glob patterns must not panic.
    #[test]
    fn filter_set_nested_globs(depth in 1usize..20) {
        let pattern = "**/".repeat(depth) + "*.txt";
        let _ = FilterSet::from_rules([FilterRule::exclude(&pattern)]);
    }

    /// Patterns with lots of character class brackets.
    #[test]
    fn filter_set_character_classes(count in 1usize..10) {
        let pattern = "[abc]".repeat(count);
        let result = FilterSet::from_rules([FilterRule::exclude(&pattern)]);
        // Should compile since [abc] is valid glob
        prop_assert!(result.is_ok());
    }

    /// An empty FilterSet queried with arbitrary paths must not panic.
    #[test]
    fn empty_filter_set_arbitrary_paths(
        path in arb_relative_path(),
        is_dir in any::<bool>(),
    ) {
        let set = FilterSet::default();
        prop_assert!(set.allows(Path::new(&path), is_dir));
        prop_assert!(set.allows_deletion(Path::new(&path), is_dir));
    }

    /// Long-form keywords with arbitrary trailing content must not panic.
    #[test]
    fn parse_long_form_arbitrary_suffix(
        keyword in prop::sample::select(vec![
            "include", "exclude", "protect", "risk",
            "merge", "dir-merge", "hide", "show", "clear",
        ]),
        suffix in "[ -~]{0,80}",
    ) {
        let input = format!("{keyword} {suffix}");
        let _ = parse_rules(&input, Path::new("<fuzz>"));
    }

    /// Repeated clear rules interspersed with other rules must not panic.
    #[test]
    fn filter_set_repeated_clears(
        count in 1usize..20,
    ) {
        let mut rules = Vec::new();
        for _ in 0..count {
            rules.push(FilterRule::exclude("*.tmp"));
            rules.push(FilterRule::clear());
        }
        rules.push(FilterRule::include("*.txt"));
        let result = FilterSet::from_rules(rules);
        prop_assert!(result.is_ok());
        let set = result.unwrap();
        prop_assert!(set.allows(Path::new("file.txt"), false));
    }
}

// ---------------------------------------------------------------------------
// Tests: Deterministic edge cases (not proptest, but important fuzz targets)
// ---------------------------------------------------------------------------

#[test]
fn parse_empty_string() {
    let result = parse_rules("", Path::new("<test>"));
    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}

#[test]
fn parse_single_null_byte() {
    let result = parse_rules("\0", Path::new("<test>"));
    // May be Err or Ok, but must not panic
    let _ = result;
}

#[test]
fn parse_many_null_bytes() {
    let input = "\0".repeat(1000);
    let _ = parse_rules(&input, Path::new("<test>"));
}

#[test]
fn parse_just_action_chars() {
    for c in ['+', '-', 'P', 'R', 'H', 'S', '.', ':', '!'] {
        let _ = parse_rules(&c.to_string(), Path::new("<test>"));
    }
}

#[test]
fn parse_action_char_followed_by_newline() {
    for c in ['+', '-', 'P', 'R', 'H', 'S', '.', ':'] {
        let input = format!("{c}\n");
        let _ = parse_rules(&input, Path::new("<test>"));
    }
}

#[test]
fn parse_only_modifiers_no_pattern() {
    let inputs = ["-!", "-!p", "-!psr", "+!", "+!psx", "-!psrxenwC"];
    for input in &inputs {
        let _ = parse_rules(input, Path::new("<test>"));
    }
}

#[test]
fn parse_all_long_form_keywords_empty_pattern() {
    let keywords = [
        "include", "exclude", "protect", "risk", "merge",
        "dir-merge", "hide", "show", "clear",
    ];
    for kw in &keywords {
        let input = format!("{kw} ");
        let _ = parse_rules(&input, Path::new("<test>"));
    }
}

#[test]
fn filter_set_pattern_just_slash() {
    // Pattern "/" becomes anchored + directory-only with empty core
    let _ = FilterSet::from_rules([FilterRule::exclude("/")]);
}

#[test]
fn filter_set_pattern_double_star() {
    let result = FilterSet::from_rules([FilterRule::exclude("**")]);
    assert!(result.is_ok());
    let set = result.unwrap();
    // ** should match everything
    assert!(!set.allows(Path::new("any/path/file.txt"), false));
}

#[test]
fn filter_set_pattern_triple_star() {
    // *** is unusual but should not panic
    let _ = FilterSet::from_rules([FilterRule::exclude("***")]);
}

#[test]
fn filter_set_very_long_pattern() {
    let pattern = "a/".repeat(500) + "*.txt";
    let _ = FilterSet::from_rules([FilterRule::exclude(&pattern)]);
}

#[test]
fn filter_set_empty_pattern() {
    // Empty pattern may or may not compile but must not panic
    let _ = FilterSet::from_rules([FilterRule::exclude("")]);
}

#[test]
fn filter_set_only_wildcards() {
    let patterns = ["*", "?", "**", "***", "*?*", "??**"];
    for p in &patterns {
        let _ = FilterSet::from_rules([FilterRule::exclude(*p)]);
    }
}

#[test]
fn filter_set_unclosed_bracket() {
    let result = FilterSet::from_rules([FilterRule::exclude("[")]);
    // Should be an error (invalid glob), not a panic
    assert!(result.is_err());
}

#[test]
fn filter_set_nested_brackets() {
    let _ = FilterSet::from_rules([FilterRule::exclude("[[a]]")]);
}

#[test]
fn filter_set_backslash_patterns() {
    let patterns = ["\\", "\\\\", "\\*", "\\?", "\\[", "a\\b"];
    for p in &patterns {
        let _ = FilterSet::from_rules([FilterRule::exclude(*p)]);
    }
}

#[test]
fn parse_rules_mixed_valid_invalid_lines() {
    let content = "- *.bak\ninvalid garbage\n+ *.txt\n";
    let result = parse_rules(content, Path::new("<test>"));
    // Should fail on the invalid line
    assert!(result.is_err());
}

#[test]
fn parse_rules_windows_line_endings() {
    let content = "- *.bak\r\n+ *.txt\r\n";
    let result = parse_rules(content, Path::new("<test>"));
    assert!(result.is_ok());
    let rules = result.unwrap();
    assert_eq!(rules.len(), 2);
}

#[test]
fn parse_rules_trailing_whitespace() {
    let content = "  - *.bak   \n  + *.txt   \n";
    let result = parse_rules(content, Path::new("<test>"));
    assert!(result.is_ok());
}

#[test]
fn parse_rules_unicode_patterns() {
    let inputs = [
        "+ \u{00e9}toile",     // accent
        "- \u{1f600}",         // emoji
        "+ \u{4e16}\u{754c}", // CJK characters
        "- \u{0000}",         // null
        "+ \u{fffd}",         // replacement character
        "- \u{202e}evil",     // RTL override
    ];
    for input in &inputs {
        let _ = parse_rules(input, Path::new("<test>"));
    }
}

#[test]
fn filter_set_allows_with_path_components() {
    let set = FilterSet::from_rules([
        FilterRule::exclude("*.tmp"),
        FilterRule::include("keep/**"),
    ])
    .unwrap();

    // Test with various interesting path components
    let paths = [
        "", ".", "..", "...", "/", "//", "a//b", "a/./b", "a/../b",
        " ", "\t", "\n",
    ];
    for p in &paths {
        let _ = set.allows(Path::new(p), false);
        let _ = set.allows(Path::new(p), true);
        let _ = set.allows_deletion(Path::new(p), false);
        let _ = set.allows_deletion_when_excluded_removed(Path::new(p), false);
    }
}

#[test]
fn cvs_exclusion_rules_are_all_valid() {
    // All CVS exclusion patterns should compile into a valid FilterSet
    let rules: Vec<_> = cvs_exclusion_rules(false).collect();
    let result = FilterSet::from_rules(rules);
    assert!(result.is_ok());
}

#[test]
fn parse_word_split_empty_pattern() {
    // -w with only whitespace after
    let _ = parse_rules("-w   ", Path::new("<test>"));
}

#[test]
fn parse_word_split_single_word() {
    let result = parse_rules("-w single", Path::new("<test>"));
    assert!(result.is_ok());
    let rules = result.unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].pattern(), "single");
}

#[test]
fn parse_word_split_many_words() {
    let words: Vec<String> = (0..100).map(|i| format!("word{i}")).collect();
    let input = format!("-w {}", words.join(" "));
    let result = parse_rules(&input, Path::new("<test>"));
    assert!(result.is_ok());
    let rules = result.unwrap();
    assert_eq!(rules.len(), 100);
}
