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

        rules.push(parse_rule_line(line, source_path, line_num)?);
    }

    Ok(rules)
}

/// Parses a per-dir merge file under FILTRULE_NO_PREFIXES semantics.
///
/// When a dir-merge rule carries the `-` or `+` modifier, upstream
/// `exclude.c:1116-1133 parse_rule_tok` skips the entire short-prefix and
/// long-form dispatch and consumes each non-empty, non-comment line as a
/// literal pattern. Lines become exclude rules by default; when `force_include`
/// is set (the `+` variant), they become include rules instead.
///
/// When `cvs_ignore` is true (FILTRULE_CVS_IGNORE inherited from the
/// template), a bare `!` line tentatively clears the list, matching upstream's
/// FILTRULE_CLEAR_LIST escape hatch at `exclude.c:1123-1124`. Without
/// CVS_IGNORE, `!` is just another literal pattern.
///
/// When `word_split` is true (the `w` modifier, e.g. `:w-`), the whole file is
/// tokenised on any whitespace instead of one pattern per line, mirroring
/// upstream's `parse_filter_file()` token loop (`exclude.c:1499`). Comments are
/// not stripped in word-split mode (`exclude.c:1514`), so every token becomes a
/// literal pattern.
pub(crate) fn parse_rules_no_prefixes(
    content: &str,
    _source_path: &Path,
    force_include: bool,
    cvs_ignore: bool,
    word_split: bool,
) -> Vec<FilterRule> {
    let mut rules = Vec::new();

    let mut push_token = |token: &str| {
        // upstream: exclude.c:1123-1124 - when FILTRULE_CVS_IGNORE is set on
        // the template, a bare `!` becomes FILTRULE_CLEAR_LIST. Without
        // CVS_IGNORE the `!` is taken literally per the no-prefixes contract.
        if cvs_ignore && token == "!" {
            rules.push(FilterRule::clear());
        } else if force_include {
            rules.push(FilterRule::include(token));
        } else {
            rules.push(FilterRule::exclude(token));
        }
    };

    if word_split {
        // upstream: exclude.c:1499 - word_split splits on any whitespace and
        // parses every non-empty token; comments are not skipped (line 1514).
        for token in content.split_whitespace() {
            push_token(token);
        }
    } else {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }
            push_token(trimmed);
        }
    }

    rules
}

/// Parses a per-dir merge file whose template carries FILTRULE_WORD_SPLIT (the
/// `w` modifier) but not FILTRULE_NO_PREFIXES.
///
/// Upstream `parse_filter_file()` tokenises the file on any whitespace when
/// word-split is active (`exclude.c:1499`) and runs each non-empty token
/// through the normal rule parser; comments are not stripped in this mode
/// (`exclude.c:1514`). A token that is not a valid rule (for example a bare
/// pattern with no `-`/`+` prefix) is an error, matching upstream's
/// `parse_rule_tok()` "Unknown filter rule" / "unexpected end of filter rule".
pub(crate) fn parse_rules_word_split(
    content: &str,
    source_path: &Path,
) -> Result<Vec<FilterRule>, MergeFileError> {
    let mut rules = Vec::new();
    for token in content.split_whitespace() {
        rules.push(parse_rule_line(token, source_path, 0)?);
    }
    Ok(rules)
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
/// | `e` | `exclude_self` | Exclude the merge file's own name (merge rules only) |
/// | `n` | `no_inherit` | Don't inherit rules into subdirectories (merge rules only) |
/// | `w` | `word_split` | Split the merge file at whitespace (merge rules only) |
/// | `C` | `cvs_mode` | Add CVS exclusion patterns |
/// | `/` | `abs_path` | Anchor merged rules to the transfer root (merge rules only) |
/// | `-` | `no_prefixes` | Merged lines are literal excludes (merge rules only) |
/// | `+` | `no_prefixes`+`no_prefixes_include` | Merged lines are literal includes (merge rules only) |
///
/// upstream: exclude.c:1256-1259 - the `e` modifier maps to
/// `FILTRULE_EXCLUDE_SELF` and is valid only on a merge-file rule
/// (`FILTRULE_MERGE_FILE`); on any other rule upstream jumps to `invalid`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RuleModifiers {
    pub(crate) negate: bool,
    pub(crate) perishable: bool,
    pub(crate) sender_only: bool,
    pub(crate) receiver_only: bool,
    pub(crate) xattr_only: bool,
    pub(crate) exclude_self: bool,
    pub(crate) no_inherit: bool,
    pub(crate) word_split: bool,
    pub(crate) cvs_mode: bool,
    /// `/` modifier: FILTRULE_ABS_PATH (merge / dir-merge rules).
    pub(crate) abs_path: bool,
    /// `-` / `+` modifier: FILTRULE_NO_PREFIXES (merge / dir-merge rules).
    pub(crate) no_prefixes: bool,
    /// Set alongside [`Self::no_prefixes`] when the `+` variant is used.
    pub(crate) no_prefixes_include: bool,
}

impl RuleModifiers {
    /// Applies modifiers to a filter rule.
    pub(crate) fn apply(self, rule: FilterRule) -> FilterRule {
        // upstream: exclude.c:1248-1254 - the `C` modifier on a merge/dir-merge
        // rule implicitly sets FILTRULE_NO_INHERIT (alongside NO_PREFIXES,
        // WORD_SPLIT, CVS_IGNORE). Mirror that here so `:C .cvsignore` rules
        // do not propagate into descendant directories.
        let no_inherit = self.no_inherit || self.cvs_mode;
        let mut rule = rule
            .with_negate(self.negate)
            .with_perishable(self.perishable)
            .with_xattr_only(self.xattr_only)
            .with_no_inherit(no_inherit)
            .with_cvs_mode(self.cvs_mode)
            .with_abs_path(self.abs_path)
            .with_word_split(self.word_split)
            .with_no_prefixes(self.no_prefixes, self.no_prefixes_include);

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
/// Returns the parsed modifiers and the remaining string (pattern). Modifiers
/// are single characters that can appear in any order before the pattern.
///
/// `is_merge` indicates whether the rule is a merge / dir-merge rule
/// (`FILTRULE_MERGE_FILE`), which gates the `e`, `n`, `w`, `/`, `-`, and `+`
/// modifiers. `prefix_specifies_side` indicates the rule prefix already binds a
/// side (H/S/P/R), which gates the `s` and `r` modifiers. `full_line` and
/// `line_num`/`source_path` are used only for error messages.
///
/// Modifiers are validated left-to-right in a single pass, so the first invalid
/// modifier is the one reported - matching upstream's single `switch` loop.
///
/// Any character that is not a recognised modifier is a syntax error, matching
/// upstream's `invalid:` label rather than silently treating it as the start of
/// the pattern.
///
/// upstream: exclude.c:1215-1289 - the modifier loop and its `invalid:` label.
pub(crate) fn parse_modifiers<'a>(
    s: &'a str,
    is_merge: bool,
    prefix_specifies_side: bool,
    full_line: &str,
    source_path: &Path,
    line_num: usize,
) -> Result<(RuleModifiers, &'a str), MergeFileError> {
    let mut mods = RuleModifiers::default();

    for (idx, ch) in s.char_indices() {
        match ch {
            '!' => {
                // upstream: exclude.c:1191-1196 - negation is meaningless as a
                // merge-file default, so `!` on a merge rule is invalid.
                if is_merge {
                    return Err(invalid_modifier(ch, idx, full_line, source_path, line_num));
                }
                mods.negate = true;
            }
            'p' => mods.perishable = true,
            // upstream: exclude.c:1269-1277 - `s`/`r` are invalid when the rule
            // prefix already binds a side (H/S sender, P/R receiver), i.e.
            // `prefix_specifies_side`.
            's' => {
                if prefix_specifies_side {
                    return Err(invalid_modifier(ch, idx, full_line, source_path, line_num));
                }
                mods.sender_only = true;
            }
            'r' => {
                if prefix_specifies_side {
                    return Err(invalid_modifier(ch, idx, full_line, source_path, line_num));
                }
                mods.receiver_only = true;
            }
            'x' => mods.xattr_only = true,
            // upstream: exclude.c:1256-1260 - `e` (FILTRULE_EXCLUDE_SELF) is
            // valid only on a merge-file rule; on any other rule upstream
            // jumps to `invalid`.
            'e' => {
                if !is_merge {
                    return Err(invalid_modifier(ch, idx, full_line, source_path, line_num));
                }
                mods.exclude_self = true;
            }
            // upstream: exclude.c:1261-1264 - `n` (FILTRULE_NO_INHERIT) is
            // valid only on a merge-file rule; on any other rule upstream
            // jumps to `invalid`.
            'n' => {
                if !is_merge {
                    return Err(invalid_modifier(ch, idx, full_line, source_path, line_num));
                }
                mods.no_inherit = true;
            }
            // upstream: exclude.c:1279-1283 - `w` (FILTRULE_WORD_SPLIT) is
            // valid only on a merge-file rule; on any other rule upstream
            // jumps to `invalid`.
            'w' => {
                if !is_merge {
                    return Err(invalid_modifier(ch, idx, full_line, source_path, line_num));
                }
                mods.word_split = true;
            }
            'C' => mods.cvs_mode = true,
            // upstream: exclude.c:1215-1216 - `/` sets FILTRULE_ABS_PATH.
            '/' => mods.abs_path = true,
            // upstream: exclude.c:1197-1213 - `-`/`+` set FILTRULE_NO_PREFIXES
            // and are valid only on a merge-file rule that has not already set
            // the flag; `+` additionally sets FILTRULE_INCLUDE.
            '-' => {
                if !is_merge || mods.no_prefixes {
                    return Err(invalid_modifier(ch, idx, full_line, source_path, line_num));
                }
                mods.no_prefixes = true;
            }
            '+' => {
                if !is_merge || mods.no_prefixes {
                    return Err(invalid_modifier(ch, idx, full_line, source_path, line_num));
                }
                mods.no_prefixes = true;
                mods.no_prefixes_include = true;
            }
            ' ' | '_' => {
                let remainder = &s[idx + ch.len_utf8()..];
                return Ok((mods, remainder.trim_start()));
            }
            _ => {
                return Err(invalid_modifier(ch, idx, full_line, source_path, line_num));
            }
        }
    }

    Ok((mods, ""))
}

/// Builds the upstream `invalid modifier` parse error.
///
/// `idx` is the byte offset of the offending character within the modifier
/// string (the text after the action character). Upstream reports the position
/// relative to the whole rule string, where the action character is position 0,
/// so the reported position is `idx + 1`.
///
/// upstream: exclude.c:1180-1184 - `rprintf(FERROR, "invalid modifier '%c' at
/// position %d in filter rule: %s\n", *s, (int)(s - *rulestr_ptr), *rulestr_ptr)`.
fn invalid_modifier(
    ch: char,
    idx: usize,
    full_line: &str,
    source_path: &Path,
    line_num: usize,
) -> MergeFileError {
    MergeFileError::parse_error(
        source_path,
        line_num,
        format!(
            "invalid modifier '{ch}' at position {} in filter rule: {full_line}",
            idx + 1
        ),
    )
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

    /// Whether this action is a merge-file rule (`.`/`:`/`merge`/`dir-merge`).
    ///
    /// upstream: `FILTRULE_MERGE_FILE` covers both plain and per-directory
    /// merges; the `e` modifier is permitted only on these.
    const fn is_merge(self) -> bool {
        matches!(self, Self::Merge | Self::DirMerge)
    }
}

/// Tries to parse a short-form rule (single character prefix like `+`, `-`, `P`).
///
/// Returns `Ok(Some(rule))` if the line matches a short-form pattern,
/// `Ok(None)` if no prefix matched, or `Err` if a recognised prefix was paired
/// with an invalid modifier (e.g. `Hr`, `Sr`, `Ps`, `Rs`).
///
/// upstream: exclude.c:parse_filter_str() - short-form prefix handling
fn try_parse_short_form(
    line: &str,
    source_path: &Path,
    line_num: usize,
) -> Result<Option<FilterRule>, MergeFileError> {
    let (rest, action, prefix_char) = if let Some(r) = line.strip_prefix('+') {
        (r, ShortFormAction::Include, '+')
    } else if let Some(r) = line.strip_prefix('-') {
        (r, ShortFormAction::Exclude, '-')
    } else if let Some(r) = line.strip_prefix('P') {
        (r, ShortFormAction::Protect, 'P')
    } else if let Some(r) = line.strip_prefix('R') {
        (r, ShortFormAction::Risk, 'R')
    } else if let Some(r) = line.strip_prefix('.') {
        (r, ShortFormAction::Merge, '.')
    } else if let Some(r) = line.strip_prefix(':') {
        (r, ShortFormAction::DirMerge, ':')
    } else if let Some(r) = line.strip_prefix('H') {
        (r, ShortFormAction::Hide, 'H')
    } else if let Some(r) = line.strip_prefix('S') {
        (r, ShortFormAction::Show, 'S')
    } else {
        return Ok(None);
    };

    // upstream: exclude.c:1136 - `prefix_specifies_side` is set for the H/S
    // (sender) and P/R (receiver) prefixes, gating the `s`/`r` modifiers.
    let prefix_specifies_side = matches!(prefix_char, 'H' | 'S' | 'P' | 'R');
    let (mods, pattern) = parse_modifiers(
        rest,
        action.is_merge(),
        prefix_specifies_side,
        line,
        source_path,
        line_num,
    )?;
    if pattern.is_empty() {
        // upstream: exclude.c:1404-1408 - a merge / dir-merge rule with the
        // `C` (CVS-ignore) modifier and an empty pattern defaults to the
        // filename `.cvsignore`. Without `C`, an empty pattern remains
        // unrecognised here and falls through to long-form parsing.
        if mods.cvs_mode && matches!(action, ShortFormAction::Merge | ShortFormAction::DirMerge) {
            let rule = action.to_rule(".cvsignore");
            return Ok(Some(mods.apply(rule)));
        }
        return Ok(None);
    }

    let rule = action.to_rule(pattern);
    Ok(Some(if action.supports_mods() {
        mods.apply(rule)
    } else {
        rule
    }))
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

    if let Some(rule) = try_parse_short_form(line, source_path, line_num)? {
        return Ok(rule);
    }

    if let Some(rule) = try_parse_long_form(line) {
        return Ok(rule);
    }

    Err(MergeFileError::parse_error(
        source_path,
        line_num,
        format!("Unknown filter rule: `{line}'"),
    ))
}
