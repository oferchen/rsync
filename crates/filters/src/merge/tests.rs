use std::fs;
use std::io::Write;
use std::path::Path;

use tempfile::{NamedTempFile, TempDir};

use crate::FilterAction;

use super::parse::{RuleModifiers, parse_modifiers, parse_rules};
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
    assert!(err.message.contains("unrecognized"));
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
fn parse_trims_whitespace() {
    let rules = parse_rules("  + *.txt  ", Path::new("test")).unwrap();
    assert_eq!(rules[0].pattern(), "*.txt");
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
    let rules = parse_rules("-!_ *.txt", Path::new("test")).unwrap();
    assert_eq!(rules[0].pattern(), "*.txt");
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
fn parse_modifier_with_no_space() {
    let rules = parse_rules("-!/path/*.txt", Path::new("test")).unwrap();
    assert_eq!(rules[0].pattern(), "/path/*.txt");
    assert!(rules[0].is_negated());
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
    let (mods, pattern) = parse_modifiers("");
    assert!(!mods.negate);
    assert_eq!(pattern, "");
}

#[test]
fn parse_modifiers_space_only() {
    let (mods, pattern) = parse_modifiers(" pattern");
    assert!(!mods.negate);
    assert_eq!(pattern, "pattern");
}

#[test]
fn parse_modifiers_all_flags() {
    let (mods, pattern) = parse_modifiers("!psrxenC pattern");
    assert!(mods.negate);
    assert!(mods.perishable);
    assert!(mods.sender_only);
    assert!(mods.receiver_only);
    assert!(mods.xattr_only);
    assert!(mods.exclude_only);
    assert!(mods.no_inherit);
    assert!(mods.cvs_mode);
    assert_eq!(pattern, "pattern");
}

#[test]
fn parse_exclude_only_modifier() {
    let rules = parse_rules("-e *.bak", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Exclude);
    assert!(rules[0].is_exclude_only());
}

#[test]
fn parse_no_inherit_modifier() {
    let rules = parse_rules("-n *.tmp", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 1);
    assert!(rules[0].is_no_inherit());
}

#[test]
fn parse_word_split_modifier() {
    let rules = parse_rules("-w foo bar baz", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 3);
    assert_eq!(rules[0].pattern(), "foo");
    assert_eq!(rules[1].pattern(), "bar");
    assert_eq!(rules[2].pattern(), "baz");
    for rule in &rules {
        assert_eq!(rule.action(), FilterAction::Exclude);
    }
}

#[test]
fn parse_word_split_with_other_modifiers() {
    let rules = parse_rules("-!pw one two", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].pattern(), "one");
    assert_eq!(rules[1].pattern(), "two");
    for rule in &rules {
        assert!(rule.is_negated());
        assert!(rule.is_perishable());
    }
}

#[test]
fn parse_word_split_include() {
    let rules = parse_rules("+w *.rs *.toml", Path::new("test")).unwrap();
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].action(), FilterAction::Include);
    assert_eq!(rules[1].action(), FilterAction::Include);
    assert_eq!(rules[0].pattern(), "*.rs");
    assert_eq!(rules[1].pattern(), "*.toml");
}

#[test]
fn parse_cvs_mode_modifier() {
    let (mods, pattern) = parse_modifiers("C pattern");
    assert!(mods.cvs_mode);
    assert_eq!(pattern, "pattern");
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
