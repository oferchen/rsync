//! Merge file reader for filter rules.
//!
//! This module reads filter rules from files in rsync's merge file format.
//! Merge files use the same syntax as `--filter` command-line rules:
//!
//! - `+ PATTERN` or `include PATTERN` - include matching files
//! - `- PATTERN` or `exclude PATTERN` - exclude matching files
//! - `P PATTERN` or `protect PATTERN` - protect from deletion
//! - `R PATTERN` or `risk PATTERN` - remove protection
//! - `. FILE` or `merge FILE` - read additional rules from FILE
//! - `, FILE` or `dir-merge FILE` - read rules per-directory
//! - `!` or `clear` - clear previously defined rules
//! - `H PATTERN` or `hide PATTERN` - sender-only exclude
//! - `S PATTERN` or `show PATTERN` - sender-only include
//!
//! # Modifiers
//!
//! Rules can have modifiers between the action and pattern:
//!
//! - `!` - Negate match (e.g., `-! *.txt` excludes files NOT matching `*.txt`)
//! - `p` - Perishable (ignored during delete-excluded processing)
//! - `s` - Sender-side only
//! - `r` - Receiver-side only
//! - `x` - Xattr filtering only
//!
//! Example: `-!p *.tmp` excludes files NOT matching `*.tmp`, marked perishable.
//!
//! Lines starting with `#` or `;` are comments. Empty lines are ignored.

use std::fs;
use std::io;
use std::path::Path;

use crate::{FilterAction, FilterRule};

/// Error when reading or parsing a merge file.
#[derive(Debug)]
pub struct MergeFileError {
    /// The file path that caused the error.
    pub path: String,
    /// The line number (1-indexed) if applicable.
    pub line: Option<usize>,
    /// Description of the error.
    pub message: String,
}

impl std::fmt::Display for MergeFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(line) => write!(f, "{}:{}: {}", self.path, line, self.message),
            None => write!(f, "{}: {}", self.path, self.message),
        }
    }
}

impl std::error::Error for MergeFileError {}

impl MergeFileError {
    fn io_error(path: &Path, error: io::Error) -> Self {
        Self {
            path: path.display().to_string(),
            line: None,
            message: error.to_string(),
        }
    }

    fn parse_error(path: &Path, line: usize, message: impl Into<String>) -> Self {
        Self {
            path: path.display().to_string(),
            line: Some(line),
            message: message.into(),
        }
    }
}

/// Reads filter rules from a merge file.
///
/// The file is read once and all rules are returned. Lines starting with
/// `#` or `;` are treated as comments. Empty lines are ignored.
///
/// # Recursion
///
/// If the file contains `. FILE` (merge) rules, those files are NOT automatically
/// read. The caller should handle Merge rules by calling this function recursively
/// if desired, or use [`read_rules_recursive`] for automatic expansion.
///
/// # Errors
///
/// Returns an error if the file cannot be read or contains invalid syntax.
pub fn read_rules(path: &Path) -> Result<Vec<FilterRule>, MergeFileError> {
    let content = fs::read_to_string(path).map_err(|e| MergeFileError::io_error(path, e))?;
    parse_rules(&content, path)
}

/// Reads filter rules from a merge file, recursively expanding nested merge rules.
///
/// Unlike [`read_rules`], this function automatically reads and inlines rules
/// from any `. FILE` (merge) directives encountered. DirMerge rules (`, FILE`)
/// are returned as-is since they're processed during directory traversal.
///
/// # Arguments
///
/// * `path` - The merge file to read
/// * `max_depth` - Maximum recursion depth to prevent infinite loops (typically 10)
///
/// # Errors
///
/// Returns an error if any file cannot be read or contains invalid syntax.
pub fn read_rules_recursive(path: &Path, max_depth: usize) -> Result<Vec<FilterRule>, MergeFileError> {
    read_rules_recursive_impl(path, max_depth, 0)
}

fn read_rules_recursive_impl(
    path: &Path,
    max_depth: usize,
    current_depth: usize,
) -> Result<Vec<FilterRule>, MergeFileError> {
    if current_depth > max_depth {
        return Err(MergeFileError {
            path: path.display().to_string(),
            line: None,
            message: format!("maximum merge depth ({max_depth}) exceeded"),
        });
    }

    let rules = read_rules(path)?;
    let base_dir = path.parent();

    let mut expanded = Vec::with_capacity(rules.len());
    for rule in rules {
        if rule.action() == FilterAction::Merge {
            // Resolve the merge file path relative to the current file's directory
            let merge_path = if rule.pattern().starts_with('/') {
                Path::new(rule.pattern()).to_path_buf()
            } else if let Some(base) = base_dir {
                base.join(rule.pattern())
            } else {
                Path::new(rule.pattern()).to_path_buf()
            };

            let nested = read_rules_recursive_impl(&merge_path, max_depth, current_depth + 1)?;
            expanded.extend(nested);
        } else {
            expanded.push(rule);
        }
    }

    Ok(expanded)
}

/// Parses filter rules from a string in merge file format.
pub fn parse_rules(content: &str, source_path: &Path) -> Result<Vec<FilterRule>, MergeFileError> {
    let mut rules = Vec::new();

    for (line_num, line) in content.lines().enumerate() {
        let line_num = line_num + 1; // 1-indexed for error messages
        let line = line.trim();

        // Skip empty lines and comments
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        let rule = parse_rule_line(line, source_path, line_num)?;
        rules.push(rule);
    }

    Ok(rules)
}

/// Modifiers parsed from a rule prefix.
///
/// These mirror upstream rsync's rule modifiers from `exclude.c`.
#[derive(Clone, Copy, Debug, Default)]
struct RuleModifiers {
    /// Negate the match result (`!` modifier).
    negate: bool,
    /// Mark as perishable (`p` modifier).
    perishable: bool,
    /// Apply to sender side only (`s` modifier).
    sender_only: bool,
    /// Apply to receiver side only (`r` modifier).
    receiver_only: bool,
    /// Apply to xattr names only (`x` modifier).
    xattr_only: bool,
}

impl RuleModifiers {
    /// Applies modifiers to a filter rule.
    fn apply(self, rule: FilterRule) -> FilterRule {
        let mut rule = rule
            .with_negate(self.negate)
            .with_perishable(self.perishable)
            .with_xattr_only(self.xattr_only);

        // Handle side modifiers - if specified, they override the defaults
        if self.sender_only && !self.receiver_only {
            rule = rule.with_sides(true, false);
        } else if self.receiver_only && !self.sender_only {
            rule = rule.with_sides(false, true);
        }
        // If both are set, keep both sides (effectively a no-op for most rules)

        rule
    }
}

/// Parses modifiers from a string following the action character.
///
/// Returns the parsed modifiers and the remaining string (pattern).
/// Modifiers are single characters that can appear in any order before the pattern.
///
/// Reference: Upstream rsync `exclude.c` lines 1220-1288 handles modifiers.
fn parse_modifiers(s: &str) -> (RuleModifiers, &str) {
    let mut mods = RuleModifiers::default();

    for (idx, ch) in s.char_indices() {
        match ch {
            '!' => mods.negate = true,
            'p' => mods.perishable = true,
            's' => mods.sender_only = true,
            'r' => mods.receiver_only = true,
            'x' => mods.xattr_only = true,
            ' ' | '_' => {
                // Space or underscore ends modifiers, rest is pattern
                // Skip past the separator and any additional whitespace
                let remainder = &s[idx + ch.len_utf8()..];
                return (mods, remainder.trim_start());
            }
            _ => {
                // Unknown character - treat as start of pattern
                return (mods, &s[idx..]);
            }
        }
    }

    // No pattern found (all modifiers, no content)
    (mods, "")
}

/// Parses a single filter rule line.
///
/// Supports both short form (`+ pattern`, `-! pattern`) and long form
/// (`include pattern`, `exclude pattern`). Modifiers like `!`, `p`, `s`, `r`
/// can appear between the action and pattern.
fn parse_rule_line(line: &str, source_path: &Path, line_num: usize) -> Result<FilterRule, MergeFileError> {
    // Handle clear rule (just `!` or `clear`)
    if line == "!" || line.eq_ignore_ascii_case("clear") {
        return Ok(FilterRule::clear());
    }

    // Try short form first: `+ pattern`, `- pattern`, `-! pattern`, etc.
    // The action character is followed by optional modifiers, then space/pattern
    if let Some(rest) = line.strip_prefix('+') {
        let (mods, pattern) = parse_modifiers(rest);
        if !pattern.is_empty() {
            return Ok(mods.apply(FilterRule::include(pattern)));
        }
    }
    if let Some(rest) = line.strip_prefix('-') {
        let (mods, pattern) = parse_modifiers(rest);
        if !pattern.is_empty() {
            return Ok(mods.apply(FilterRule::exclude(pattern)));
        }
    }
    if let Some(rest) = line.strip_prefix('P') {
        let (mods, pattern) = parse_modifiers(rest);
        if !pattern.is_empty() {
            return Ok(mods.apply(FilterRule::protect(pattern)));
        }
    }
    if let Some(rest) = line.strip_prefix('R') {
        let (mods, pattern) = parse_modifiers(rest);
        if !pattern.is_empty() {
            return Ok(mods.apply(FilterRule::risk(pattern)));
        }
    }
    if let Some(rest) = line.strip_prefix('.') {
        // Merge rules don't support negation modifier (upstream restriction)
        let (_, pattern) = parse_modifiers(rest);
        if !pattern.is_empty() {
            return Ok(FilterRule::merge(pattern));
        }
    }
    if let Some(rest) = line.strip_prefix(',') {
        // Dir-merge rules don't support negation modifier (upstream restriction)
        let (_, pattern) = parse_modifiers(rest);
        if !pattern.is_empty() {
            return Ok(FilterRule::dir_merge(pattern));
        }
    }
    if let Some(rest) = line.strip_prefix('H') {
        let (mods, pattern) = parse_modifiers(rest);
        if !pattern.is_empty() {
            return Ok(mods.apply(FilterRule::hide(pattern)));
        }
    }
    if let Some(rest) = line.strip_prefix('S') {
        let (mods, pattern) = parse_modifiers(rest);
        if !pattern.is_empty() {
            return Ok(mods.apply(FilterRule::show(pattern)));
        }
    }

    // Try long form: `include pattern`, `exclude pattern`, etc.
    // We check the lowercase version but extract the pattern from the original
    // line to preserve case.
    let lower = line.to_ascii_lowercase();
    if lower.starts_with("include ") {
        let pattern = &line[8..]; // Preserve original case
        return Ok(FilterRule::include(pattern.trim()));
    }
    if lower.starts_with("exclude ") {
        let pattern = &line[8..];
        return Ok(FilterRule::exclude(pattern.trim()));
    }
    if lower.starts_with("protect ") {
        let pattern = &line[8..];
        return Ok(FilterRule::protect(pattern.trim()));
    }
    if lower.starts_with("risk ") {
        let pattern = &line[5..];
        return Ok(FilterRule::risk(pattern.trim()));
    }
    if lower.starts_with("merge ") {
        let pattern = &line[6..];
        return Ok(FilterRule::merge(pattern.trim()));
    }
    if lower.starts_with("dir-merge ") {
        let pattern = &line[10..];
        return Ok(FilterRule::dir_merge(pattern.trim()));
    }
    if lower.starts_with("hide ") {
        let pattern = &line[5..];
        return Ok(FilterRule::hide(pattern.trim()));
    }
    if lower.starts_with("show ") {
        let pattern = &line[5..];
        return Ok(FilterRule::show(pattern.trim()));
    }

    // Unrecognized rule
    Err(MergeFileError::parse_error(
        source_path,
        line_num,
        format!("unrecognized filter rule: {line}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

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
        let rules = parse_rules(", .rsync-filter", Path::new("test")).unwrap();
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

        // Create a simple rules file
        let rules_path = dir.path().join("rules.txt");
        fs::write(&rules_path, "+ *.txt\n- *.bak\n").unwrap();

        let rules = read_rules_recursive(&rules_path, 10).unwrap();
        assert_eq!(rules.len(), 2);
    }

    #[test]
    fn read_rules_recursive_with_merge() {
        let dir = TempDir::new().unwrap();

        // Create nested rules file
        let nested_path = dir.path().join("nested.rules");
        fs::write(&nested_path, "- *.tmp\n").unwrap();

        // Create main rules file with merge directive
        let main_path = dir.path().join("main.rules");
        fs::write(&main_path, format!("+ *.txt\n. {}\n- *.bak\n", nested_path.display())).unwrap();

        let rules = read_rules_recursive(&main_path, 10).unwrap();
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].pattern(), "*.txt");
        assert_eq!(rules[1].pattern(), "*.tmp"); // From nested file
        assert_eq!(rules[2].pattern(), "*.bak");
    }

    #[test]
    fn read_rules_recursive_depth_limit() {
        let dir = TempDir::new().unwrap();

        // Create a self-referencing rules file
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
        fs::write(&rules_path, ", .rsync-filter\n+ *.txt\n").unwrap();

        let rules = read_rules_recursive(&rules_path, 10).unwrap();
        assert_eq!(rules.len(), 2);
        // DirMerge rules are preserved, not expanded
        assert_eq!(rules[0].action(), FilterAction::DirMerge);
        assert_eq!(rules[0].pattern(), ".rsync-filter");
    }

    #[test]
    fn parse_preserves_pattern_case() {
        // Pattern case should be preserved even when using long-form keywords
        let rules = parse_rules("include README.TXT", Path::new("test")).unwrap();
        assert_eq!(rules[0].pattern(), "README.TXT");
    }

    #[test]
    fn parse_trims_whitespace() {
        let rules = parse_rules("  + *.txt  ", Path::new("test")).unwrap();
        assert_eq!(rules[0].pattern(), "*.txt");
    }

    // Tests for modifier parsing

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
        // Negated and perishable
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
        // Order shouldn't matter for modifiers
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
        // Underscore can be used as separator between modifiers and pattern
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
        // Hide already sets sender-only
        assert!(rules[0].applies_to_sender());
        assert!(!rules[0].applies_to_receiver());
    }

    #[test]
    fn parse_show_with_negate() {
        let rules = parse_rules("S! *.public", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Include);
        assert!(rules[0].is_negated());
        // Show already sets sender-only
        assert!(rules[0].applies_to_sender());
        assert!(!rules[0].applies_to_receiver());
    }

    #[test]
    fn parse_modifier_with_no_space() {
        // Pattern can follow modifiers without space if it starts with non-modifier char
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
        let (mods, pattern) = parse_modifiers("!psrx pattern");
        assert!(mods.negate);
        assert!(mods.perishable);
        assert!(mods.sender_only);
        assert!(mods.receiver_only);
        assert!(mods.xattr_only);
        assert_eq!(pattern, "pattern");
    }
}
