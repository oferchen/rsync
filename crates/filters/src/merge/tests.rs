use std::fs;
use std::io::Write;
use std::path::Path;

use tempfile::{NamedTempFile, TempDir};

use crate::FilterAction;

use super::parse::{
    RuleModifiers, parse_modifiers, parse_rules, parse_rules_no_prefixes, parse_rules_word_split,
};
use super::read::{read_rules, read_rules_recursive};

#[test]
fn parse_include_short() {
    let rules = parse_rules("+ *.txt", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Include);
    assert_eq!(rules[0].pattern(), "*.txt");
}

#[test]
fn parse_exclude_short() {
    let rules = parse_rules("- *.bak", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Exclude);
    assert_eq!(rules[0].pattern(), "*.bak");
}

#[test]
fn parse_protect_short() {
    let rules = parse_rules("P /important", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Protect);
    assert_eq!(rules[0].pattern(), "/important");
}

#[test]
fn parse_risk_short() {
    let rules = parse_rules("R /temp", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Risk);
    assert_eq!(rules[0].pattern(), "/temp");
}

#[test]
fn parse_merge_short() {
    let rules = parse_rules(". /etc/rsync/rules", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Merge);
    assert_eq!(rules[0].pattern(), "/etc/rsync/rules");
}

#[test]
fn parse_dir_merge_short() {
    let rules = parse_rules(": .rsync-filter", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
    assert_eq!(rules[0].pattern(), ".rsync-filter");
}

#[test]
fn parse_hide_short() {
    let rules = parse_rules("H *.secret", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Exclude);
    assert!(rules[0].applies_to_sender());
    assert!(!rules[0].applies_to_receiver());
}

#[test]
fn parse_show_short() {
    let rules = parse_rules("S *.public", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Include);
    assert!(rules[0].applies_to_sender());
    assert!(!rules[0].applies_to_receiver());
}

#[test]
fn parse_clear_short() {
    let rules = parse_rules("!", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Clear);
}

#[test]
fn parse_include_long() {
    let rules = parse_rules("include *.txt", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Include);
    assert_eq!(rules[0].pattern(), "*.txt");
}

#[test]
fn parse_exclude_long() {
    let rules = parse_rules("exclude *.bak", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Exclude);
}

#[test]
fn parse_clear_long() {
    let rules = parse_rules("clear", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Clear);
}

#[test]
fn parse_comments_and_empty_lines() {
    let content = "# Comment\n\n; Another comment\n+ *.txt\n";
    let rules = parse_rules(content, Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].pattern(), "*.txt");
}

#[test]
fn parse_multiple_rules() {
    let content = "+ *.txt\n- *.bak\nP /important\n";
    let rules = parse_rules(content, Path::new("test")).unwrap();
    assert_eq!(rules.len(), 3);
    assert_eq!(rules[0].action(), FilterAction::Include);
    assert_eq!(rules[1].action(), FilterAction::Exclude);
    assert_eq!(rules[2].action(), FilterAction::Protect);
}

#[test]
fn parse_error_unrecognized() {
    let result = parse_rules("invalid rule", Path::new("test.rules"));
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.message.contains("Unknown filter rule"));
    assert_eq!(err.line, Some(1));
}

#[test]
fn read_rules_from_file() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "# My rules").unwrap();
    writeln!(file, "+ *.txt").unwrap();
    writeln!(file, "- *.bak").unwrap();

    let rules = read_rules(file.path()).unwrap();
    assert_eq!(rules.len(), 2);
}

#[test]
fn read_rules_file_not_found() {
    let result = read_rules(Path::new("/nonexistent/file.rules"));
    assert!(result.is_err());
}

#[test]
fn read_rules_recursive_simple() {
    let dir = TempDir::new().unwrap();

    let rules_path = dir.path().join("rules.txt");
    fs::write(&rules_path, "+ *.txt\n- *.bak\n").unwrap();

    let rules = read_rules_recursive(&rules_path, 10).unwrap();
    assert_eq!(rules.len(), 2);
}

#[test]
fn read_rules_recursive_with_merge() {
    let dir = TempDir::new().unwrap();

    let nested_path = dir.path().join("nested.rules");
    fs::write(&nested_path, "- *.tmp\n").unwrap();

    let main_path = dir.path().join("main.rules");
    fs::write(
        &main_path,
        format!("+ *.txt\n. {}\n- *.bak\n", nested_path.display()),
    )
    .unwrap();

    let rules = read_rules_recursive(&main_path, 10).unwrap();
    assert_eq!(rules.len(), 3);
    assert_eq!(rules[0].pattern(), "*.txt");
    assert_eq!(rules[1].pattern(), "*.tmp"); // From nested file
    assert_eq!(rules[2].pattern(), "*.bak");
}

#[test]
fn read_rules_recursive_depth_limit() {
    let dir = TempDir::new().unwrap();

    let rules_path = dir.path().join("loop.rules");
    fs::write(&rules_path, format!(". {}\n", rules_path.display())).unwrap();

    let result = read_rules_recursive(&rules_path, 3);
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("depth"));
}

#[test]
fn read_rules_recursive_preserves_dir_merge() {
    let dir = TempDir::new().unwrap();

    let rules_path = dir.path().join("rules.txt");
    fs::write(&rules_path, ": .rsync-filter\n+ *.txt\n").unwrap();

    let rules = read_rules_recursive(&rules_path, 10).unwrap();
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
    assert_eq!(rules[0].pattern(), ".rsync-filter");
}

#[test]
fn parse_preserves_pattern_case() {
    let rules = parse_rules("include README.TXT", Path::new("test")).unwrap();
    assert_eq!(rules[0].pattern(), "README.TXT");
}

#[test]
fn parse_leading_whitespace_is_rejected() {
    // upstream: exclude.c:1211-1213 parse_rule_tok - a leading space is not a
    // valid rule prefix, so it reaches the `switch` default and raises
    // "Unknown filter rule" (RERR_SYNTAX). It must not be silently trimmed.
    let result = parse_rules("  + *.txt", Path::new("test"));
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("Unknown filter rule"));
}

#[test]
fn parse_trailing_whitespace_is_kept_in_pattern() {
    // upstream: exclude.c:1313 - the pattern length is strlen, so a trailing
    // space stays part of the pattern verbatim. `x.o` therefore does not match
    // `*.o ` and stays included (differential-fuzz silent-data regression).
    let rules = parse_rules("- *.o ", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].pattern(), "*.o ");
}

#[test]
fn parse_whitespace_only_line_is_rejected() {
    // upstream: exclude.c:1514 parse_filter_file - a whitespace-only line is
    // neither empty nor a comment, so it falls through to parse_rule_tok and
    // errors rather than being skipped as blank.
    let result = parse_rules("   \n- *.bak\n", Path::new("test"));
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("Unknown filter rule"));
}

#[test]
fn parse_no_prefixes_keeps_literal_whitespace() {
    // upstream: exclude.c:1122-1124,1313 - under FILTRULE_NO_PREFIXES the whole
    // line is taken verbatim as the pattern (no prefix scan, strlen length), so
    // leading and trailing whitespace are preserved literally.
    let rules = parse_rules_no_prefixes("  *.o \n", Path::new("test"), false, false, false);
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].pattern(), "  *.o ");
}

#[test]
fn parse_negate_modifier_exclude() {
    let rules = parse_rules("-! *.txt", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Exclude);
    assert_eq!(rules[0].pattern(), "*.txt");
    assert!(rules[0].is_negated());
}

#[test]
fn parse_negate_modifier_include() {
    let rules = parse_rules("+! *.txt", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Include);
    assert_eq!(rules[0].pattern(), "*.txt");
    assert!(rules[0].is_negated());
}

#[test]
fn parse_perishable_modifier() {
    let rules = parse_rules("-p *.tmp", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Exclude);
    assert_eq!(rules[0].pattern(), "*.tmp");
    assert!(rules[0].is_perishable());
    assert!(!rules[0].is_negated());
}

#[test]
fn parse_combined_modifiers() {
    let rules = parse_rules("-!p *.tmp", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Exclude);
    assert_eq!(rules[0].pattern(), "*.tmp");
    assert!(rules[0].is_negated());
    assert!(rules[0].is_perishable());
}

#[test]
fn parse_sender_side_modifier() {
    let rules = parse_rules("-s *.bak", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Exclude);
    assert_eq!(rules[0].pattern(), "*.bak");
    assert!(rules[0].applies_to_sender());
    assert!(!rules[0].applies_to_receiver());
}

#[test]
fn parse_receiver_side_modifier() {
    let rules = parse_rules("-r *.bak", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Exclude);
    assert_eq!(rules[0].pattern(), "*.bak");
    assert!(!rules[0].applies_to_sender());
    assert!(rules[0].applies_to_receiver());
}

#[test]
fn parse_xattr_modifier() {
    let rules = parse_rules("-x user.*", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Exclude);
    assert_eq!(rules[0].pattern(), "user.*");
    assert!(rules[0].is_xattr_only());
}

#[test]
fn parse_multiple_modifiers_order_independent() {
    let rules1 = parse_rules("-!ps *.tmp", Path::new("test")).unwrap();
    let rules2 = parse_rules("-sp! *.tmp", Path::new("test")).unwrap();

    assert!(rules1[0].is_negated());
    assert!(rules1[0].is_perishable());
    assert!(rules1[0].applies_to_sender());
    assert!(!rules1[0].applies_to_receiver());

    assert!(rules2[0].is_negated());
    assert!(rules2[0].is_perishable());
    assert!(rules2[0].applies_to_sender());
    assert!(!rules2[0].applies_to_receiver());
}

#[test]
fn parse_underscore_separator() {
    // upstream: exclude.c:1290-1291,1313 - exactly one separator (here the `_`)
    // is consumed after the rule char and modifiers; the remaining ` *.txt` is
    // taken verbatim by strlen, so the leading space stays part of the pattern.
    let rules = parse_rules("-!_ *.txt", Path::new("test")).unwrap();
    assert_eq!(rules[0].pattern(), " *.txt");
    assert!(rules[0].is_negated());
}

#[test]
fn parse_protect_with_negate() {
    let rules = parse_rules("P! /important", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Protect);
    assert_eq!(rules[0].pattern(), "/important");
    assert!(rules[0].is_negated());
}

#[test]
fn parse_risk_with_negate() {
    let rules = parse_rules("R! /temp", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Risk);
    assert_eq!(rules[0].pattern(), "/temp");
    assert!(rules[0].is_negated());
}

#[test]
fn parse_hide_with_negate() {
    let rules = parse_rules("H! *.secret", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Exclude);
    assert!(rules[0].is_negated());
    assert!(rules[0].applies_to_sender());
    assert!(!rules[0].applies_to_receiver());
}

#[test]
fn parse_show_with_negate() {
    let rules = parse_rules("S! *.public", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Include);
    assert!(rules[0].is_negated());
    assert!(rules[0].applies_to_sender());
    assert!(!rules[0].applies_to_receiver());
}

#[test]
fn parse_modifier_negate_then_pattern() {
    // upstream requires a space (or `_`) between the modifiers and the pattern.
    // `-! /path/*.txt` is `-` (exclude) with the `!` (negate) modifier and the
    // pattern `/path/*.txt`.
    let rules = parse_rules("-! /path/*.txt", Path::new("test")).unwrap();
    assert_eq!(rules[0].pattern(), "/path/*.txt");
    assert!(rules[0].is_negated());
}

#[test]
fn parse_modifier_unknown_char_without_separator_errors() {
    // upstream: exclude.c:1180-1184 - without a separator the modifier loop
    // keeps consuming characters, so the `p` in `path` is read as the
    // perishable modifier and the following `a` hits the `invalid:` label
    // rather than being treated as the start of the pattern.
    let err = parse_rules("-!/path/*.txt", Path::new("test")).unwrap_err();
    assert!(
        err.to_string().contains("invalid modifier 'a'"),
        "unexpected error: {err}"
    );
}

#[test]
fn rule_modifiers_default() {
    let mods = RuleModifiers::default();
    assert!(!mods.negate);
    assert!(!mods.perishable);
    assert!(!mods.sender_only);
    assert!(!mods.receiver_only);
    assert!(!mods.xattr_only);
}

#[test]
fn parse_modifiers_empty_string() {
    let (mods, pattern) = parse_modifiers("", false, false, "", Path::new("test"), 1).unwrap();
    assert!(!mods.negate);
    assert_eq!(pattern, "");
}

#[test]
fn parse_modifiers_space_only() {
    let (mods, pattern) =
        parse_modifiers(" pattern", false, false, " pattern", Path::new("test"), 1).unwrap();
    assert!(!mods.negate);
    assert_eq!(pattern, "pattern");
}

/// The merge-only modifiers (`e`, `n`, `w`, plus `C`) parse together on a
/// merge-file rule. `!` is excluded because upstream rejects negation on a
/// merge rule (exclude.c:1241-1246); it is covered by the non-merge test.
#[test]
fn parse_modifiers_all_flags() {
    let (mods, pattern) = parse_modifiers(
        "psrxenwC pattern",
        true,
        false,
        ":psrxenwC pattern",
        Path::new("test"),
        1,
    )
    .unwrap();
    assert!(mods.perishable);
    assert!(mods.sender_only);
    assert!(mods.receiver_only);
    assert!(mods.xattr_only);
    assert!(mods.exclude_self);
    assert!(mods.no_inherit);
    assert!(mods.word_split);
    assert!(mods.cvs_mode);
    assert_eq!(pattern, "pattern");
}

/// The non-merge modifiers (`!`, `p`, `s`, `r`, `x`, `C`) parse together on
/// an ordinary rule; the merge-only `e`/`n`/`w` are rejected separately.
#[test]
fn parse_modifiers_non_merge_flags() {
    let (mods, pattern) = parse_modifiers(
        "!psrxC pattern",
        false,
        false,
        "-!psrxC pattern",
        Path::new("test"),
        1,
    )
    .unwrap();
    assert!(mods.negate);
    assert!(mods.perishable);
    assert!(mods.sender_only);
    assert!(mods.receiver_only);
    assert!(mods.xattr_only);
    assert!(mods.cvs_mode);
    assert_eq!(pattern, "pattern");
}

/// The `e` modifier is `FILTRULE_EXCLUDE_SELF`, valid only on a merge-file
/// rule. On an ordinary exclude/include/etc. rule upstream jumps to `invalid`
/// (exclude.c:1256-1259). oc-rsync previously accepted `-e *.bak` silently and
/// stored a flag no matching logic read, so a malformed rule became a no-op
/// instead of the parse error upstream reports. This test pins the rejection so
/// the two tools agree on what is a syntax error.
#[test]
fn parse_exclude_self_modifier_rejected_on_non_merge_rule() {
    let err = parse_rules("-e *.bak", Path::new("test")).unwrap_err();
    assert!(
        err.to_string().contains("invalid modifier 'e'"),
        "unexpected error: {err}"
    );
}

/// `e` word-split combinations on a non-merge rule are equally invalid: the
/// modifier is rejected before the pattern is expanded.
#[test]
fn parse_exclude_self_modifier_rejected_with_word_split() {
    let err = parse_rules("-ew foo bar", Path::new("test")).unwrap_err();
    assert!(
        err.to_string().contains("invalid modifier 'e'"),
        "unexpected error: {err}"
    );
}

/// `e` is accepted on a dir-merge rule (upstream sets `FILTRULE_EXCLUDE_SELF`
/// there). The rule parses successfully as a dir-merge; exclude-self plumbing
/// is applied at the chain layer via `DirMergeConfig::with_exclude_self`.
#[test]
fn parse_exclude_self_modifier_accepted_on_dir_merge_rule() {
    let rules = parse_rules(":e .rsync-filter", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
    assert_eq!(rules[0].pattern(), ".rsync-filter");
}

/// `e` is likewise accepted on a plain merge rule.
#[test]
fn parse_exclude_self_modifier_accepted_on_merge_rule() {
    let rules = parse_rules(".e rules.txt", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Merge);
    assert_eq!(rules[0].pattern(), "rules.txt");
}

/// `n` (FILTRULE_NO_INHERIT) is valid only on a merge-file rule. On an
/// ordinary exclude/include rule upstream jumps to `invalid`
/// (exclude.c:1261-1264: `case 'n': if (!(rule->rflags &
/// FILTRULE_MERGE_FILE)) goto invalid;`). oc-rsync previously accepted
/// `-n *.tmp` and stored a no-inherit flag no path honoured, so a malformed
/// rule silently diverged from upstream's syntax error.
#[test]
fn parse_no_inherit_modifier_rejected_on_non_merge_rule() {
    let err = parse_rules("-n *.tmp", Path::new("test")).unwrap_err();
    assert!(
        err.to_string().contains("invalid modifier 'n'"),
        "unexpected error: {err}"
    );
}

/// `n` parses on a dir-merge rule, where it maps to FILTRULE_NO_INHERIT.
#[test]
fn parse_no_inherit_modifier_accepted_on_dir_merge_rule() {
    let rules = parse_rules(":n .rsync-filter", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
    assert!(rules[0].is_no_inherit());
}

/// `w` (FILTRULE_WORD_SPLIT) is valid only on a merge-file rule
/// (exclude.c:1279-1283: `case 'w': if (!(rule->rflags &
/// FILTRULE_MERGE_FILE)) goto invalid;`). oc-rsync previously expanded
/// `-w foo bar baz` into three excludes, but upstream rejects `w` on an
/// exclude rule as a syntax error; word-split applies only to merge-file
/// bodies. Verified: `rsync -rn --filter='-w foo bar baz'` exits 1 with
/// "invalid modifier 'w'".
#[test]
fn parse_word_split_modifier_rejected_on_non_merge_rule() {
    let err = parse_rules("-w foo bar baz", Path::new("test")).unwrap_err();
    assert!(
        err.to_string().contains("invalid modifier 'w'"),
        "unexpected error: {err}"
    );
}

/// `w` on an include rule is equally invalid.
#[test]
fn parse_word_split_include_rejected_on_non_merge_rule() {
    let err = parse_rules("+w *.rs *.toml", Path::new("test")).unwrap_err();
    assert!(
        err.to_string().contains("invalid modifier 'w'"),
        "unexpected error: {err}"
    );
}

/// `w` parses on a dir-merge rule (FILTRULE_WORD_SPLIT), the only context
/// upstream permits it.
#[test]
fn parse_word_split_modifier_accepted_on_dir_merge_rule() {
    let rules = parse_rules(":w .rsync-filter", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
    assert_eq!(rules[0].pattern(), ".rsync-filter");
}

#[test]
fn parse_cvs_mode_modifier() {
    let (mods, pattern) = parse_modifiers(
        "C pattern",
        false,
        false,
        "-C pattern",
        Path::new("test"),
        1,
    )
    .unwrap();
    assert!(mods.cvs_mode);
    assert_eq!(pattern, "pattern");
}

#[test]
fn parse_dir_merge_no_prefixes_exclude_modifier() {
    // upstream: exclude.c:1197-1209 - `:- .filt` sets FILTRULE_NO_PREFIXES on a
    // dir-merge rule (exclude variant).
    let rules = parse_rules(":- .filt", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
    assert_eq!(rules[0].pattern(), ".filt");
    assert_eq!(rules[0].no_prefixes(), (true, false));
}

#[test]
fn parse_dir_merge_no_prefixes_include_modifier() {
    // upstream: exclude.c:1210-1213 - `:+ .filt` sets FILTRULE_NO_PREFIXES and
    // FILTRULE_INCLUDE (include variant).
    let rules = parse_rules(":+ .filt", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].no_prefixes(), (true, true));
}

#[test]
fn parse_dir_merge_abs_path_modifier() {
    // upstream: exclude.c:1215-1216 - `:/ .filt` sets FILTRULE_ABS_PATH.
    let rules = parse_rules(":/ .filt", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert!(rules[0].is_abs_path());
}

#[test]
fn parse_no_prefixes_modifier_on_non_merge_errors() {
    // upstream: exclude.c:1197-1199 - `-`/`+` require FILTRULE_MERGE_FILE, so a
    // plain exclude rule with `-` in its modifiers is invalid.
    let err = parse_rules("-- pattern", Path::new("test")).unwrap_err();
    assert!(
        err.to_string().contains("invalid modifier '-'"),
        "unexpected error: {err}"
    );
}

#[test]
fn parse_negate_modifier_on_merge_errors() {
    // upstream: exclude.c:1191-1196 - `!` is meaningless on a merge default and
    // is rejected on merge / dir-merge rules.
    let err = parse_rules(":! .filt", Path::new("test")).unwrap_err();
    assert!(
        err.to_string().contains("invalid modifier '!'"),
        "unexpected error: {err}"
    );
}

#[test]
fn read_rules_recursive_depth_zero_no_merge() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");
    fs::write(&rules_path, "+ *.txt\n- *.bak\n").unwrap();

    let rules = read_rules_recursive(&rules_path, 0).unwrap();
    assert_eq!(rules.len(), 2);
}

#[test]
fn read_rules_recursive_depth_zero_with_merge_fails() {
    let dir = TempDir::new().unwrap();

    let nested_path = dir.path().join("nested.rules");
    fs::write(&nested_path, "- *.tmp\n").unwrap();

    let main_path = dir.path().join("main.rules");
    fs::write(&main_path, format!(". {}\n", nested_path.display())).unwrap();

    let result = read_rules_recursive(&main_path, 0);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.message.contains("depth"));
    assert!(err.message.contains("0"));
}

#[test]
fn read_rules_recursive_depth_one_single_merge() {
    let dir = TempDir::new().unwrap();

    let nested_path = dir.path().join("nested.rules");
    fs::write(&nested_path, "- *.tmp\n").unwrap();

    let main_path = dir.path().join("main.rules");
    fs::write(
        &main_path,
        format!("+ *.txt\n. {}\n", nested_path.display()),
    )
    .unwrap();

    let rules = read_rules_recursive(&main_path, 1).unwrap();
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].pattern(), "*.txt");
    assert_eq!(rules[1].pattern(), "*.tmp");
}

#[test]
fn read_rules_recursive_depth_one_two_levels_fails() {
    let dir = TempDir::new().unwrap();

    let deep_path = dir.path().join("deep.rules");
    fs::write(&deep_path, "- *.deep\n").unwrap();

    let nested_path = dir.path().join("nested.rules");
    fs::write(&nested_path, format!(". {}\n", deep_path.display())).unwrap();

    let main_path = dir.path().join("main.rules");
    fs::write(&main_path, format!(". {}\n", nested_path.display())).unwrap();

    let result = read_rules_recursive(&main_path, 1);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.message.contains("depth"));
}

#[test]
fn read_rules_recursive_exact_depth_succeeds() {
    let dir = TempDir::new().unwrap();

    let level3_path = dir.path().join("level3.rules");
    fs::write(&level3_path, "- *.level3\n").unwrap();

    let level2_path = dir.path().join("level2.rules");
    fs::write(
        &level2_path,
        format!("- *.level2\n. {}\n", level3_path.display()),
    )
    .unwrap();

    let level1_path = dir.path().join("level1.rules");
    fs::write(
        &level1_path,
        format!("- *.level1\n. {}\n", level2_path.display()),
    )
    .unwrap();

    let main_path = dir.path().join("main.rules");
    fs::write(
        &main_path,
        format!("- *.main\n. {}\n", level1_path.display()),
    )
    .unwrap();

    let rules = read_rules_recursive(&main_path, 3).unwrap();
    assert_eq!(rules.len(), 4);
    assert_eq!(rules[0].pattern(), "*.main");
    assert_eq!(rules[1].pattern(), "*.level1");
    assert_eq!(rules[2].pattern(), "*.level2");
    assert_eq!(rules[3].pattern(), "*.level3");

    let result = read_rules_recursive(&main_path, 2);
    assert!(result.is_err());
}

#[test]
fn read_rules_recursive_error_includes_path() {
    let dir = TempDir::new().unwrap();

    let self_ref_path = dir.path().join("selfref.rules");
    fs::write(&self_ref_path, format!(". {}\n", self_ref_path.display())).unwrap();

    let result = read_rules_recursive(&self_ref_path, 5);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.path.contains("selfref.rules"));
}

#[test]
fn read_rules_recursive_multiple_merges_same_level() {
    let dir = TempDir::new().unwrap();

    let file_a = dir.path().join("a.rules");
    fs::write(&file_a, "- *.a\n").unwrap();

    let file_b = dir.path().join("b.rules");
    fs::write(&file_b, "- *.b\n").unwrap();

    let main_path = dir.path().join("main.rules");
    fs::write(
        &main_path,
        format!(". {}\n. {}\n", file_a.display(), file_b.display()),
    )
    .unwrap();

    let rules = read_rules_recursive(&main_path, 1).unwrap();
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].pattern(), "*.a");
    assert_eq!(rules[1].pattern(), "*.b");
}

#[test]
fn read_rules_recursive_diamond_pattern() {
    let dir = TempDir::new().unwrap();

    let file_d = dir.path().join("d.rules");
    fs::write(&file_d, "- *.d\n").unwrap();

    let file_b = dir.path().join("b.rules");
    fs::write(&file_b, format!("- *.b\n. {}\n", file_d.display())).unwrap();

    let file_c = dir.path().join("c.rules");
    fs::write(&file_c, format!("- *.c\n. {}\n", file_d.display())).unwrap();

    let main_path = dir.path().join("main.rules");
    fs::write(
        &main_path,
        format!(". {}\n. {}\n", file_b.display(), file_c.display()),
    )
    .unwrap();

    let rules = read_rules_recursive(&main_path, 3).unwrap();
    assert_eq!(rules.len(), 4);
}

#[test]
fn read_rules_recursive_relative_path_merge() {
    let dir = TempDir::new().unwrap();

    let nested_path = dir.path().join("nested.rules");
    fs::write(&nested_path, "- *.nested\n").unwrap();

    let main_path = dir.path().join("main.rules");
    fs::write(&main_path, ". nested.rules\n").unwrap();

    let rules = read_rules_recursive(&main_path, 2).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].pattern(), "*.nested");
}

#[test]
fn read_rules_recursive_mixed_with_dir_merge() {
    let dir = TempDir::new().unwrap();

    let rules_path = dir.path().join("rules.txt");
    fs::write(&rules_path, ": .rsync-filter\n+ *.txt\n").unwrap();

    let rules = read_rules_recursive(&rules_path, 0).unwrap();
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
    assert_eq!(rules[1].action(), FilterAction::Include);
}

// upstream: exclude.c parse_filter_str (rsync-3.4.2) lines 1269-1278 — the
// `s` and `r` modifiers gate which side a rule fires on. These tests pin
// down the three-state parsing surface (both, sender-only, receiver-only)
// and the `prefix_specifies_side` rejection.

#[test]
fn parse_include_default_both_sides() {
    let rules = parse_rules("+ foo", Path::new("test")).unwrap();
    assert!(rules[0].applies_to_sender());
    assert!(rules[0].applies_to_receiver());
}

#[test]
fn parse_include_receiver_only() {
    let rules = parse_rules("+r foo", Path::new("test")).unwrap();
    assert!(!rules[0].applies_to_sender());
    assert!(rules[0].applies_to_receiver());
}

#[test]
fn parse_include_sender_only() {
    let rules = parse_rules("+s foo", Path::new("test")).unwrap();
    assert!(rules[0].applies_to_sender());
    assert!(!rules[0].applies_to_receiver());
}

#[test]
fn parse_exclude_receiver_only() {
    let rules = parse_rules("-r *.tmp", Path::new("test")).unwrap();
    assert_eq!(rules[0].action(), FilterAction::Exclude);
    assert!(!rules[0].applies_to_sender());
    assert!(rules[0].applies_to_receiver());
}

#[test]
fn parse_both_side_modifiers_collapses_to_both() {
    // Upstream treats setting both FILTRULE_SENDER_SIDE and FILTRULE_RECEIVER_SIDE
    // as equivalent to setting neither (see exclude.c send_rules elide computation
    // at lines 1605-1612). Our parser matches that semantics.
    let rules = parse_rules("+sr foo", Path::new("test")).unwrap();
    assert!(rules[0].applies_to_sender());
    assert!(rules[0].applies_to_receiver());
}

#[test]
fn parse_rejects_s_modifier_on_side_specific_prefix() {
    // upstream: exclude.c parse_filter_str sets `prefix_specifies_side` for
    // H/S/P/R prefixes and rejects 's' or 'r' modifiers on them.
    for line in ["Hs foo", "Ss foo", "Ps foo", "Rs foo"] {
        let err = parse_rules(line, Path::new("test")).unwrap_err();
        assert!(
            err.message.contains("invalid modifier 's'"),
            "expected rejection for `{line}`, got `{}`",
            err.message
        );
    }
}

#[test]
fn parse_rejects_r_modifier_on_side_specific_prefix() {
    for line in ["Hr foo", "Sr foo", "Pr foo", "Rr foo"] {
        let err = parse_rules(line, Path::new("test")).unwrap_err();
        assert!(
            err.message.contains("invalid modifier 'r'"),
            "expected rejection for `{line}`, got `{}`",
            err.message
        );
    }
}

#[test]
fn parse_rejects_side_modifier_with_word_split_on_side_prefix() {
    let err = parse_rules("Hsw foo bar", Path::new("test")).unwrap_err();
    assert!(err.message.contains("invalid modifier 's'"));
}

// upstream: exclude.c:1404-1408 - merge / dir-merge with the `C`
// (CVS-ignore) modifier and an empty pattern defaults to `.cvsignore`.
#[test]
fn parse_colon_c_empty_pattern_defaults_to_cvsignore() {
    let rules = parse_rules(":C\n", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
    assert_eq!(rules[0].pattern(), ".cvsignore");
    assert!(rules[0].is_cvs_mode());
}

#[test]
fn parse_colon_c_with_explicit_pattern_preserves_cvs_mode() {
    let rules = parse_rules(":C my.ignore\n", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
    assert_eq!(rules[0].pattern(), "my.ignore");
    assert!(rules[0].is_cvs_mode());
}

#[test]
fn parse_dot_c_empty_pattern_defaults_to_cvsignore() {
    let rules = parse_rules(".C\n", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Merge);
    assert_eq!(rules[0].pattern(), ".cvsignore");
    assert!(rules[0].is_cvs_mode());
}

// upstream: exclude.c:1279-1283 - a `:w` dir-merge sets FILTRULE_WORD_SPLIT on
// its FilterRule so the chain later tokenises the merge file on whitespace.
#[test]
fn parse_dir_merge_w_modifier_sets_word_split_on_rule() {
    let rules = parse_rules(":w .filt\n", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
    assert_eq!(rules[0].pattern(), ".filt");
    assert!(rules[0].is_word_split());
}

// upstream: exclude.c:1499 - word-split tokenises on any whitespace and parses
// each token as a rule; `_` separates a token's prefix from its pattern.
#[test]
fn parse_rules_word_split_splits_on_any_whitespace() {
    let rules = parse_rules_word_split("-_*.log\t-_*.tmp -_*.bak\n", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 3);
    for rule in &rules {
        assert_eq!(rule.action(), FilterAction::Exclude);
    }
    assert_eq!(rules[0].pattern(), "*.log");
    assert_eq!(rules[1].pattern(), "*.tmp");
    assert_eq!(rules[2].pattern(), "*.bak");
}

// upstream: exclude.c:1211-1213 - a bare token with no valid prefix is an
// "Unknown filter rule" error, same as the non-word-split parser.
#[test]
fn parse_rules_word_split_rejects_bare_token() {
    assert!(parse_rules_word_split("*.log", Path::new("test")).is_err());
}

// upstream: exclude.c:1122-1133, 1499 - `:w-` tokenises on whitespace with each
// token a literal exclude (no prefix dispatch).
#[test]
fn parse_rules_no_prefixes_word_split_literal_tokens() {
    let rules = parse_rules_no_prefixes(
        "*.log\t*.tmp *.bak\n",
        Path::new("test"),
        false,
        false,
        true,
    );
    assert_eq!(rules.len(), 3);
    assert_eq!(rules[0].pattern(), "*.log");
    assert_eq!(rules[1].pattern(), "*.tmp");
    assert_eq!(rules[2].pattern(), "*.bak");
    assert!(rules.iter().all(|r| r.action() == FilterAction::Exclude));
}

// Without word-split, the no-prefixes parser keeps its one-pattern-per-line
// behaviour so a whitespace-separated line is a single literal pattern.
#[test]
fn parse_rules_no_prefixes_line_mode_keeps_whole_line() {
    let rules = parse_rules_no_prefixes(
        "*.log *.tmp *.bak\n",
        Path::new("test"),
        false,
        false,
        false,
    );
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].pattern(), "*.log *.tmp *.bak");
}

// upstream: exclude.c:1069-1078 rule_strcmp accepts `_` as a keyword separator
// exactly like a space, so `exclude_*.bak` must parse identically to
// `exclude *.bak`. Long-form filter files written with `_` separators are a
// documented rsync convenience and must interoperate.
#[test]
fn long_form_keyword_underscore_separator_matches_space_form() {
    let under = parse_rules("exclude_*.bak", Path::new("test")).unwrap();
    let space = parse_rules("exclude *.bak", Path::new("test")).unwrap();
    assert_eq!(under.len(), 1);
    assert_eq!(under[0].action(), FilterAction::Exclude);
    assert_eq!(under[0].pattern(), space[0].pattern());
    assert_eq!(under[0].pattern(), "*.bak");
}

// upstream: rule_strcmp uses isspace(), which accepts a tab as a separator, so
// `include<TAB>*.txt` is the same rule as `include *.txt`.
#[test]
fn long_form_keyword_tab_separator() {
    let rules = parse_rules("include\t*.txt", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Include);
    assert_eq!(rules[0].pattern(), "*.txt");
}

// upstream: rule_strcmp accepts end-of-string (`!str[rule_len]`), so a bare
// keyword with no pattern is a valid rule whose pattern is empty.
#[test]
fn long_form_keyword_alone_is_empty_pattern() {
    let rules = parse_rules("hide", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Exclude);
    assert!(rules[0].applies_to_sender());
    assert!(!rules[0].applies_to_receiver());
    assert_eq!(rules[0].pattern(), "");
}

// upstream: rule_strcmp returns `str + rule_len` for a comma, so the modifier
// loop runs after it. `dir-merge,n .filt` must therefore carry the same
// no-inherit modifier as the short-form `:n .filt`.
#[test]
fn long_form_comma_modifier_matches_short_form() {
    let long = parse_rules("dir-merge,n .filt", Path::new("test")).unwrap();
    let short = parse_rules(":n .filt", Path::new("test")).unwrap();
    assert_eq!(long.len(), 1);
    assert_eq!(long[0].action(), FilterAction::DirMerge);
    assert_eq!(long[0].pattern(), ".filt");
    assert!(long[0].is_no_inherit());
    assert_eq!(long[0].pattern(), short[0].pattern());
    assert_eq!(long[0].is_no_inherit(), short[0].is_no_inherit());
}

// upstream: a comma after a long-form keyword introduces side modifiers just as
// on a short-form prefix, so `exclude,s foo` is a sender-only exclude of `foo`.
#[test]
fn long_form_comma_sender_side_exclude() {
    let rules = parse_rules("exclude,s foo", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Exclude);
    assert_eq!(rules[0].pattern(), "foo");
    assert!(rules[0].applies_to_sender());
    assert!(!rules[0].applies_to_receiver());
}

// upstream: exclude.c:1176-1179 consumes a comma immediately after a
// single-character prefix, so `+,p foo` is the same perishable include as the
// directly-adjacent `+p foo`.
#[test]
fn short_form_prefix_comma_separator_matches_plain() {
    let comma = parse_rules("+,p foo", Path::new("test")).unwrap();
    let plain = parse_rules("+p foo", Path::new("test")).unwrap();
    assert_eq!(comma.len(), 1);
    assert_eq!(comma[0].action(), FilterAction::Include);
    assert_eq!(comma[0].pattern(), "foo");
    assert!(comma[0].is_perishable());
    assert_eq!(comma[0].is_perishable(), plain[0].is_perishable());
}

// upstream: only the comma directly after the prefix is a separator; a comma
// later in the modifier run is an invalid modifier and must be rejected, so the
// `,` in `-p,x foo` is a syntax error just like in upstream rsync.
#[test]
fn short_form_comma_not_after_prefix_is_invalid() {
    assert!(parse_rules("-p,x foo", Path::new("test")).is_err());
}

// upstream: a keyword followed by a non-separator byte is not that keyword, so
// `excludes foo` is not an exclude rule and falls through to "Unknown filter
// rule" rather than silently matching `exclude`.
#[test]
fn long_form_keyword_without_separator_is_unknown() {
    assert!(parse_rules("excludes foo", Path::new("test")).is_err());
}
