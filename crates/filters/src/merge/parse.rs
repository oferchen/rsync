//! Rule parsing for rsync merge-file format.
//!
//! Handles both short-form prefixes (`+`, `-`, `P`, `R`, `.`, `:`, `H`, `S`, `!`)
//! and long-form keywords (`include`, `exclude`, etc.). Lines starting with `#`
//! or `;` are comments, and blank lines are skipped.

use std::path::Path;

use crate::FilterRule;

use super::error::MergeFileError;

/// Parses filter rules from a string in rsync's merge-file format.
///
/// Accepts the same syntax as merge files on disk: short-form prefixes
/// (`+`, `-`, `P`, `R`, `.`, `:`, `H`, `S`, `!`) and long-form keywords
/// (`include`, `exclude`, etc.). Lines starting with `#` or `;` are
/// comments, and blank lines are skipped.
///
/// `source_path` is used only for error messages; no I/O is performed.
///
/// # Examples
///
/// ```
/// use filters::parse_rules;
/// use std::path::Path;
///
/// let rules = parse_rules(
///     "# Ignore backups\n- *.bak\n+ important/\n",
///     Path::new("<inline>"),
/// ).unwrap();
/// assert_eq!(rules.len(), 2);
/// ```
///
/// # Errors
///
/// Returns [`MergeFileError`] if any line contains unrecognised syntax.
pub fn parse_rules(content: &str, source_path: &Path) -> Result<Vec<FilterRule>, MergeFileError> {
    let mut rules = Vec::new();

    for (line_num, line) in content.lines().enumerate() {
        let line_num = line_num + 1; // 1-indexed for error messages
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        let line_rules = parse_rule_line_expanded(line, source_path, line_num)?;
        rules.extend(line_rules);
    }

    Ok(rules)
}

/// Parses a single filter rule line, potentially expanding into multiple rules.
///
/// The `w` (word-split) modifier causes the pattern to be split on whitespace,
/// creating multiple rules with the same action and modifiers.
fn parse_rule_line_expanded(
    line: &str,
    source_path: &Path,
    line_num: usize,
) -> Result<Vec<FilterRule>, MergeFileError> {
    let (action_char, rest) = if let Some(rest) = line.strip_prefix('+') {
        ('+', rest)
    } else if let Some(rest) = line.strip_prefix('-') {
        ('-', rest)
    } else if let Some(rest) = line.strip_prefix('P') {
        ('P', rest)
    } else if let Some(rest) = line.strip_prefix('R') {
        ('R', rest)
    } else if let Some(rest) = line.strip_prefix('H') {
        ('H', rest)
    } else if let Some(rest) = line.strip_prefix('S') {
        ('S', rest)
    } else {
        return Ok(vec![parse_rule_line(line, source_path, line_num)?]);
    };

    let (mods, pattern) = parse_modifiers(rest);

    if mods.word_split && !pattern.is_empty() {
        let patterns: Vec<&str> = pattern.split_whitespace().collect();
        if patterns.is_empty() {
            return Err(MergeFileError::parse_error(
                source_path,
                line_num,
                "word-split pattern is empty",
            ));
        }

        let mods_for_expanded = RuleModifiers {
            word_split: false,
            ..mods
        };

        let mut rules = Vec::with_capacity(patterns.len());
        for pat in patterns {
            let base_rule = match action_char {
                '+' => FilterRule::include(pat),
                '-' => FilterRule::exclude(pat),
                'P' => FilterRule::protect(pat),
                'R' => FilterRule::risk(pat),
                'H' => FilterRule::hide(pat),
                'S' => FilterRule::show(pat),
                _ => unreachable!(),
            };
            rules.push(mods_for_expanded.apply(base_rule));
        }
        return Ok(rules);
    }

    Ok(vec![parse_rule_line(line, source_path, line_num)?])
}

/// Modifiers parsed from a rule prefix.
///
/// These mirror upstream rsync's rule modifiers from `exclude.c` (lines 1220-1288).
/// Modifiers appear between the action character and the pattern, e.g., `-!ps pattern`.
///
/// # Modifier Characters
///
/// | Char | Field | Description |
/// |------|-------|-------------|
/// | `!` | `negate` | Invert match result |
/// | `p` | `perishable` | Can be overridden by include rules |
/// | `s` | `sender_only` | Apply on sender side only |
/// | `r` | `receiver_only` | Apply on receiver side only |
/// | `x` | `xattr_only` | Match xattr names only |
/// | `e` | `exclude_only` | Force rule to exclude |
/// | `n` | `no_inherit` | Don't inherit parent rules (merge) |
/// | `w` | `word_split` | Split pattern on whitespace |
/// | `C` | `cvs_mode` | Add CVS exclusion patterns |
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RuleModifiers {
    pub(crate) negate: bool,
    pub(crate) perishable: bool,
    pub(crate) sender_only: bool,
    pub(crate) receiver_only: bool,
    pub(crate) xattr_only: bool,
    pub(crate) exclude_only: bool,
    pub(crate) no_inherit: bool,
    pub(crate) word_split: bool,
    pub(crate) cvs_mode: bool,
}

impl RuleModifiers {
    /// Applies modifiers to a filter rule.
    pub(crate) fn apply(self, rule: FilterRule) -> FilterRule {
        let mut rule = rule
            .with_negate(self.negate)
            .with_perishable(self.perishable)
            .with_xattr_only(self.xattr_only)
            .with_exclude_only(self.exclude_only)
            .with_no_inherit(self.no_inherit);

        if self.sender_only && !self.receiver_only {
            rule = rule.with_sides(true, false);
        } else if self.receiver_only && !self.sender_only {
            rule = rule.with_sides(false, true);
        }

        rule
    }
}

/// Parses modifiers from a string following the action character.
///
/// Returns the parsed modifiers and the remaining string (pattern).
/// Modifiers are single characters that can appear in any order before the pattern.
///
/// Reference: upstream rsync `exclude.c` lines 1220-1288 handles modifiers.
pub(crate) fn parse_modifiers(s: &str) -> (RuleModifiers, &str) {
    let mut mods = RuleModifiers::default();

    for (idx, ch) in s.char_indices() {
        match ch {
            '!' => mods.negate = true,
            'p' => mods.perishable = true,
            's' => mods.sender_only = true,
            'r' => mods.receiver_only = true,
            'x' => mods.xattr_only = true,
            'e' => mods.exclude_only = true,
            'n' => mods.no_inherit = true,
            'w' => mods.word_split = true,
            'C' => mods.cvs_mode = true,
            ' ' | '_' => {
                let remainder = &s[idx + ch.len_utf8()..];
                return (mods, remainder.trim_start());
            }
            _ => {
                return (mods, &s[idx..]);
            }
        }
    }

    (mods, "")
}

/// Short-form rule action types.
#[derive(Clone, Copy)]
enum ShortFormAction {
    Include,
    Exclude,
    Protect,
    Risk,
    Merge,
    DirMerge,
    Hide,
    Show,
}

impl ShortFormAction {
    /// Creates a `FilterRule` from the action and pattern.
    fn to_rule(self, pattern: &str) -> FilterRule {
        match self {
            Self::Include => FilterRule::include(pattern),
            Self::Exclude => FilterRule::exclude(pattern),
            Self::Protect => FilterRule::protect(pattern),
            Self::Risk => FilterRule::risk(pattern),
            Self::Merge => FilterRule::merge(pattern),
            Self::DirMerge => FilterRule::dir_merge(pattern),
            Self::Hide => FilterRule::hide(pattern),
            Self::Show => FilterRule::show(pattern),
        }
    }

    /// Whether this action supports modifiers.
    const fn supports_mods(self) -> bool {
        !matches!(self, Self::Merge)
    }
}

/// Tries to parse a short-form rule (single character prefix like `+`, `-`, `P`).
///
/// Returns `Some(rule)` if the line matches a short-form pattern, `None` otherwise.
///
/// upstream: exclude.c:parse_filter_str() - short-form prefix handling
fn try_parse_short_form(line: &str) -> Option<FilterRule> {
    let (rest, action) = if let Some(r) = line.strip_prefix('+') {
        (r, ShortFormAction::Include)
    } else if let Some(r) = line.strip_prefix('-') {
        (r, ShortFormAction::Exclude)
    } else if let Some(r) = line.strip_prefix('P') {
        (r, ShortFormAction::Protect)
    } else if let Some(r) = line.strip_prefix('R') {
        (r, ShortFormAction::Risk)
    } else if let Some(r) = line.strip_prefix('.') {
        (r, ShortFormAction::Merge)
    } else if let Some(r) = line.strip_prefix(':') {
        (r, ShortFormAction::DirMerge)
    } else if let Some(r) = line.strip_prefix('H') {
        (r, ShortFormAction::Hide)
    } else if let Some(r) = line.strip_prefix('S') {
        (r, ShortFormAction::Show)
    } else {
        return None;
    };

    let (mods, pattern) = parse_modifiers(rest);
    if pattern.is_empty() {
        return None;
    }

    let rule = action.to_rule(pattern);
    Some(if action.supports_mods() {
        mods.apply(rule)
    } else {
        rule
    })
}

/// Tries to parse a long-form rule (keyword prefix like `include`, `exclude`).
///
/// Returns `Some(rule)` if the line matches a long-form pattern, `None` otherwise.
///
/// upstream: exclude.c:parse_filter_str() - long-form keyword handling
fn try_parse_long_form(line: &str) -> Option<FilterRule> {
    let lower = line.to_ascii_lowercase();

    let keywords: &[(&str, usize, ShortFormAction)] = &[
        ("include ", 8, ShortFormAction::Include),
        ("exclude ", 8, ShortFormAction::Exclude),
        ("protect ", 8, ShortFormAction::Protect),
        ("risk ", 5, ShortFormAction::Risk),
        ("merge ", 6, ShortFormAction::Merge),
        ("dir-merge ", 10, ShortFormAction::DirMerge),
        ("hide ", 5, ShortFormAction::Hide),
        ("show ", 5, ShortFormAction::Show),
    ];

    for &(keyword, len, action) in keywords {
        if lower.starts_with(keyword) {
            let pattern = line[len..].trim();
            return Some(action.to_rule(pattern));
        }
    }

    None
}

/// Parses a single filter rule line.
///
/// Supports both short form (`+ pattern`, `-! pattern`) and long form
/// (`include pattern`, `exclude pattern`). Modifiers like `!`, `p`, `s`, `r`
/// can appear between the action and pattern in short form.
fn parse_rule_line(
    line: &str,
    source_path: &Path,
    line_num: usize,
) -> Result<FilterRule, MergeFileError> {
    if line == "!" || line.eq_ignore_ascii_case("clear") {
        return Ok(FilterRule::clear());
    }

    if let Some(rule) = try_parse_short_form(line) {
        return Ok(rule);
    }

    if let Some(rule) = try_parse_long_form(line) {
        return Ok(rule);
    }

    Err(MergeFileError::parse_error(
        source_path,
        line_num,
        format!("unrecognized filter rule: {line}"),
    ))
}
