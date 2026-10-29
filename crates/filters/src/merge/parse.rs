//! Rule parsing for rsync merge-file format.
//!
//! Handles both short-form prefixes (`+`, `-`, `P`, `R`, `.`, `:`, `H`, `S`, `!`)
//! and long-form keywords (`include`, `exclude`, etc.). Lines starting with `#`
//! or `;` are comments, and blank lines are skipped.

use std::path::Path;

use crate::RuleSource;
use crate::clear_token::{ClearToken, classify_clear_token};
use crate::cvs::CvsTemplate;
use crate::rule_source::trace_add_rule;
use crate::{FilterRule, ModifierScope, RuleModifiers, parse_rule_modifiers_in};

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
    parse_rules_in(content, source_path, false)
}

/// [`parse_rules`] for a merge file whose own rule names a side (`:s`, `:r`),
/// which refuses any rule in it that names one too.
pub(crate) fn parse_rules_in(
    content: &str,
    source_path: &Path,
    template_sided: bool,
) -> Result<Vec<FilterRule>, MergeFileError> {
    let mut rules = Vec::new();

    // `filter_file_records` is the single owner of the record split; see
    // `merge::records` for why `str::lines` is the wrong boundary.
    for (line_num, line) in crate::filter_file_records(content).enumerate() {
        let line_num = line_num + 1; // 1-indexed for error messages

        // upstream: exclude.c:1806 parse_filter_file. `filter_file_line_is_rule`
        // is the single owner of that test; see `merge::skip` for why it must
        // not trim first.
        if !crate::filter_file_line_is_rule(line, true) {
            continue;
        }

        let name = source_path.display().to_string();
        let source = RuleSource::File {
            name: &name,
            line: line_num,
        };
        for rule in parse_rule_line(line, source_path, line_num)?.into_rules() {
            refuse_side_in_sided_file(&rule, template_sided, line, &source, source_path, line_num)?;
            trace_add_rule(
                rule.action,
                &String::from_utf8_lossy(&rule.pattern),
                &source,
            );
            rules.push(rule);
        }
    }

    Ok(rules)
}

/// Parses a per-dir merge file under FILTRULE_NO_PREFIXES semantics.
///
/// When a dir-merge rule carries the `-` or `+` modifier, upstream
/// `exclude.c:1273-1275 parse_rule_tok` skips the entire short-prefix and
/// long-form dispatch and consumes each non-empty, non-comment line as a
/// literal pattern. Lines become exclude rules by default; when `force_include`
/// is set (the `+` variant), they become include rules instead.
///
/// When `cvs_ignore` is true (FILTRULE_CVS_IGNORE inherited from the
/// template), a bare `!` line tentatively clears the list, matching upstream's
/// FILTRULE_CLEAR_LIST escape hatch at `exclude.c:1274-1275`. Without
/// CVS_IGNORE, `!` is just another literal pattern.
///
/// When `word_split` is true (the `w` modifier, e.g. `:w-`), the whole file is
/// tokenised on any whitespace instead of one pattern per line, mirroring
/// upstream's `parse_filter_file()` token loop (`exclude.c:1772-1773`). Comments are
/// not stripped in word-split mode (`exclude.c:1806`), so every token becomes a
/// literal pattern.
pub(crate) fn parse_rules_no_prefixes(
    content: &str,
    source_path: &Path,
    force_include: bool,
    cvs_ignore: bool,
    word_split: bool,
) -> Vec<FilterRule> {
    let mut rules = Vec::new();

    // Every rule here comes out of a merged file's *contents*, so its text is
    // never the operator's own and must be redacted in the FILTER2 trace.
    let name = source_path.display().to_string();
    let source = RuleSource::FileWordSplit { name: &name };

    let mut push_token = |token: &str| {
        // upstream: exclude.c:1274-1275 - when FILTRULE_CVS_IGNORE is set on
        // the template, a bare `!` becomes FILTRULE_CLEAR_LIST. Without
        // CVS_IGNORE the `!` is taken literally per the no-prefixes contract.
        let rule = if cvs_ignore && token == "!" {
            FilterRule::clear()
        } else if force_include {
            FilterRule::include(token)
        } else {
            FilterRule::exclude(token)
        };
        trace_add_rule(
            rule.action,
            &String::from_utf8_lossy(&rule.pattern),
            &source,
        );
        rules.push(rule);
    };

    if word_split {
        // upstream: exclude.c:1772-1773 - word_split splits on any whitespace and
        // parses every non-empty token; comments are not skipped (line 1806).
        for token in content.split_whitespace() {
            push_token(token);
        }
    } else {
        // Records split via the single owner `filter_file_records`
        // (exclude.c:1774-1793), so a lone `\r` ends a rule here too.
        for line in crate::filter_file_records(content) {
            // upstream: exclude.c:1806 parse_filter_file, via the single owner
            // `filter_file_line_is_rule`. The surviving line becomes a literal
            // pattern verbatim (FILTRULE_NO_PREFIXES takes strlen with no
            // trimming, exclude.c:1465), so leading/trailing whitespace is kept.
            if !crate::filter_file_line_is_rule(line, true) {
                continue;
            }
            push_token(line);
        }
    }

    rules
}

/// Parses a per-dir merge file whose template carries FILTRULE_WORD_SPLIT (the
/// `w` modifier) but not FILTRULE_NO_PREFIXES.
///
/// Upstream `parse_filter_file()` tokenises the file on any whitespace when
/// word-split is active (`exclude.c:1772-1773`) and runs each non-empty token
/// through the normal rule parser; comments are not stripped in this mode
/// (`exclude.c:1806`). A token that is not a valid rule (for example a bare
/// pattern with no `-`/`+` prefix) is an error, matching upstream's
/// `parse_rule_tok()` "Unknown filter rule" / "unexpected end of filter rule".
pub(crate) fn parse_rules_word_split(
    content: &str,
    source_path: &Path,
    template_sided: bool,
) -> Result<Vec<FilterRule>, MergeFileError> {
    let mut rules = Vec::new();
    let name = source_path.display().to_string();
    let source = RuleSource::FileWordSplit { name: &name };
    for token in content.split_whitespace() {
        for rule in parse_rule_line(token, source_path, 0)?.into_rules() {
            refuse_side_in_sided_file(&rule, template_sided, token, &source, source_path, 0)?;
            trace_add_rule(
                rule.action,
                &String::from_utf8_lossy(&rule.pattern),
                &source,
            );
            rules.push(rule);
        }
    }
    Ok(rules)
}

/// Where a rule of a merge file came from: its line, or the file alone when
/// the file is word-split and so counts no lines.
///
/// upstream: exclude.c:1754 sets `rule_src_line = word_split ? -1 : 0`, and
/// rule_src_where() names the file alone for a negative line (exclude.c:82-83).
/// Word-split tokens reach this module with line 0.
fn rule_file_source(name: &str, line_num: usize) -> RuleSource<'_> {
    if line_num == 0 {
        RuleSource::FileWordSplit { name }
    } else {
        RuleSource::File {
            name,
            line: line_num,
        }
    }
}

/// Refuses a rule that names a side inside a merge file whose rule named one.
///
/// upstream: exclude.c:1447-1456 parse_rule_tok() - "The filter and template
/// both specify side(s). This is dodgy (and won't work correctly if the
/// template is a one-sided per-dir merge rule), so reject it." The refusal is a
/// `filter_rule_err`, so the text goes through `rule_text` (exclude.c:133-137),
/// and it precedes the inherit, so the template never overrides the rule's own
/// side. oc spells "no side bit" as both sides applying.
fn refuse_side_in_sided_file(
    rule: &FilterRule,
    template_sided: bool,
    text: &str,
    source: &RuleSource<'_>,
    source_path: &Path,
    line_num: usize,
) -> Result<(), MergeFileError> {
    if !template_sided || (rule.applies_to_sender && rule.applies_to_receiver) {
        return Ok(());
    }
    Err(MergeFileError::parse_error(
        source_path,
        line_num,
        format!(
            "specified-side merge file contains specified-side filter: {}",
            source.rule_text(text)
        ),
    ))
}

/// Applies a rule's modifier flags to the rule its prefix built.
///
/// upstream: exclude.c:1402-1409 - `C` already set FILTRULE_NO_INHERIT in the
/// table, so `:C .cvsignore` rules do not propagate into descendants.
pub(crate) fn apply_modifiers(mods: RuleModifiers, rule: FilterRule) -> FilterRule {
    let mut rule = rule
        .with_negate(mods.negate)
        .with_perishable(mods.perishable)
        .with_xattr_only(mods.xattr)
        .with_no_inherit(mods.no_inherit)
        .with_cvs_mode(mods.cvs_ignore)
        .with_abs_path(mods.abs_path)
        .with_word_split(mods.word_split)
        .with_no_prefixes(mods.no_prefixes, mods.no_prefixes_include);
    if mods.sender_side != mods.receiver_side {
        rule = rule.with_sides(mods.sender_side, mods.receiver_side);
    }
    rule
}

/// Splits the text after a rule prefix into its modifier run and the pattern,
/// validating the run against the shared modifier table.
///
/// The run ends at the first ` ` or `_`, and exactly one separator is consumed
/// (exclude.c:1365, 1444-1445); the pattern is then the rest verbatim, as its
/// length is `strlen` (:1465). With no separator the whole text is modifiers,
/// so `+foo` is refused on its `f` rather than read as a pattern.
///
/// `full_line` and `line_num`/`source_path` are used only for error messages.
///
/// # Errors
///
/// Returns the refusal upstream's `invalid:` label prints (exclude.c:1371-1380)
/// for the first byte [`parse_rule_modifiers`] rejects.
pub(crate) fn parse_modifiers<'a>(
    s: &'a str,
    is_merge: bool,
    prefix_specifies_side: bool,
    full_line: &str,
    source_path: &Path,
    line_num: usize,
) -> Result<(RuleModifiers, &'a str), MergeFileError> {
    let (run, pattern) = match s.find([' ', '_']) {
        Some(idx) => (&s[..idx], &s[idx + 1..]),
        None => (s, ""),
    };
    let scope = ModifierScope {
        merge_file: is_merge,
        prefix_specifies_side,
    };
    let mods = parse_rule_modifiers_in(full_line, run, scope).map_err(|invalid| {
        invalid_modifier(invalid.ch, invalid.offset, full_line, source_path, line_num)
    })?;
    Ok((mods, pattern))
}

/// Builds the upstream `invalid modifier` parse error for the byte at
/// `position` within `full_line`.
///
/// upstream: exclude.c:1372-1380 - `rprintf(FERROR, "invalid modifier '%c' at
/// position %d in filter rule: %s\n", *s, (int)(s - *rulestr_ptr), *rulestr_ptr)`.
fn invalid_modifier(
    ch: char,
    position: usize,
    full_line: &str,
    source_path: &Path,
    line_num: usize,
) -> MergeFileError {
    // upstream: exclude.c:1370-1379 builds the detail into a scratch buffer and
    // passes it through rule_detail, which returns "" whenever the text itself
    // is replaced - "a character of it, an offset into it. Dropped along with
    // the text it describes" (exclude.c:127-131). Keeping the character or the
    // offset while redacting the line would leak the file one byte at a time,
    // which is a finer-grained oracle than echoing the whole rule.
    let name = source_path.display().to_string();
    let source = rule_file_source(&name, line_num);
    let detail = format!(" '{ch}' at position {position}");
    MergeFileError::parse_error(
        source_path,
        line_num,
        format!(
            "invalid modifier{} in filter rule: {}",
            source.rule_detail(&detail),
            source.rule_text(full_line)
        ),
    )
}

/// What one rule of a merge file parses to.
pub(crate) enum ParsedLine {
    /// An ordinary rule.
    Rule(FilterRule),
    /// A merge rule with the `e` modifier, preceded by the exclude of its own
    /// basename.
    SelfExcludingMerge {
        /// The `- <basename>` exclude.
        exclude: FilterRule,
        /// The merge or dir-merge rule itself.
        merge: FilterRule,
    },
    /// A non-merge `C` rule, which stands for the CVS ignore list.
    CvsIgnore(CvsTemplate),
}

impl ParsedLine {
    /// The rules this line contributes, in order.
    ///
    /// upstream: exclude.c:1201-1206 check_filter() evaluates `cvs_filter_list`
    /// in a `C` rule's place and never matches its own pattern, so expanding
    /// the list here is the same first-match order. The built-in defaults are
    /// marked perishable, as every protocol oc speaks at >= 30 does
    /// (exclude.c:1498-1500).
    pub(crate) fn into_rules(self) -> Vec<FilterRule> {
        match self {
            Self::Rule(rule) => vec![rule],
            Self::SelfExcludingMerge { exclude, merge } => vec![exclude, merge],
            Self::CvsIgnore(template) => crate::cvs::cvs_ignore_rules(template, true),
        }
    }
}

/// Builds what a rule with `action`, modifiers `mods` and `pattern` parses to.
///
/// upstream: exclude.c:1474 lets a `C` rule have no pattern and
/// exclude.c:1595-1597 hands its flags to get_cvs_excludes(); a merge `C`
/// instead names the per-directory `.cvsignore` and stays a merge rule.
fn parsed_rule(action: ShortFormAction, mods: RuleModifiers, pattern: &str) -> ParsedLine {
    if mods.cvs_ignore && !action.is_merge() {
        return ParsedLine::CvsIgnore(CvsTemplate::new(action.is_include(), &mods));
    }
    let rule = apply_modifiers(mods, action.to_rule(pattern));
    // upstream: exclude.c:1558-1572 - `e` adds an exclude of the merge file's
    // basename, carrying none of the merge rule's modifiers, ahead of the
    // merge rule itself.
    if mods.exclude_self {
        return ParsedLine::SelfExcludingMerge {
            exclude: FilterRule::exclude(crate::merge_file_basename(pattern)),
            merge: rule,
        };
    }
    ParsedLine::Rule(rule)
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

    /// Whether this action is a merge-file rule (`.`/`:`/`merge`/`dir-merge`).
    ///
    /// upstream: `FILTRULE_MERGE_FILE` covers both plain and per-directory
    /// merges; the `e` modifier is permitted only on these.
    const fn is_merge(self) -> bool {
        matches!(self, Self::Merge | Self::DirMerge)
    }

    /// Whether the prefix sets FILTRULE_INCLUDE (`+` and its long name).
    const fn is_include(self) -> bool {
        matches!(self, Self::Include)
    }
}

/// Tries to parse a short-form rule (single character prefix like `+`, `-`, `P`).
///
/// Returns `Ok(Some(rule))` if the line matches a short-form pattern,
/// `Ok(None)` if no prefix matched, or `Err` if a recognised prefix was paired
/// with an invalid modifier (e.g. `Hr`, `Sr`, `Ps`, `Rs`).
///
/// upstream: exclude.c:1325-1364 parse_rule_tok() - short-form prefix handling
fn try_parse_short_form(
    line: &str,
    source_path: &Path,
    line_num: usize,
) -> Result<Option<ParsedLine>, MergeFileError> {
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

    // upstream: exclude.c:1325-1329 - in the default (single-character prefix)
    // case, a comma immediately after the prefix is consumed as a separator
    // (`if (s[1] == ',') s++;`) so the modifier loop begins after it. This lets
    // `+,x pattern` be written as an equivalent of `+x pattern`. Only the comma
    // directly after the prefix is a separator; a later comma remains an
    // invalid modifier, exactly as upstream reports.
    let rest = rest.strip_prefix(',').unwrap_or(rest);

    // upstream: exclude.c:1345-1358 - `prefix_specifies_side` is set for the H/S
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
    // upstream: exclude.c:1365-1443 `parse_rule_tok` applies the parsed modifiers
    // to every rule prefix, including a plain merge (`.`): the merge-file
    // modifiers are gated on `FILTRULE_MERGE_FILE`, which `.` sets (exclude.c:1337-1338)
    // just as `:` does. Dropping them for `.` silently ignored `C`, `w`, `n`, `e`,
    // side restrictions and anchoring on plain merges.
    complete_rule(action, mods, pattern, line, source_path, line_num).map(Some)
}

/// Finishes a rule once its prefix, modifiers and pattern are split, the same
/// way for every spelling of the prefix.
///
/// upstream: exclude.c:1474-1476 parse_rule_tok() - an empty pattern is
/// "unexpected end of filter rule" unless the rule has `C`, so a bare `-` or
/// `exclude` is refused; exclude.c:1553-1557 parse_filter_str() then names a
/// `C` merge rule's missing file `.cvsignore`, for `:C` and `dir-merge,C` alike.
fn complete_rule(
    action: ShortFormAction,
    mods: RuleModifiers,
    pattern: &str,
    line: &str,
    source_path: &Path,
    line_num: usize,
) -> Result<ParsedLine, MergeFileError> {
    if pattern.is_empty() && !mods.cvs_ignore {
        let name = source_path.display().to_string();
        let source = rule_file_source(&name, line_num);
        return Err(MergeFileError::parse_error(
            source_path,
            line_num,
            format!("unexpected end of filter rule: {}", source.rule_text(line)),
        ));
    }
    let pattern = if pattern.is_empty() && action.is_merge() {
        ".cvsignore"
    } else {
        pattern
    };
    Ok(parsed_rule(action, mods, pattern))
}

/// Returns true for the characters upstream `isspace()` treats as whitespace in
/// the C locale (space, tab, newline, vertical tab, form feed, carriage return).
///
/// Rust's [`char::is_ascii_whitespace`] omits the vertical tab, so this mirrors
/// the C set exactly for `rule_strcmp` separator detection.
fn is_c_isspace(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r')
}

/// Tries to parse a long-form rule (keyword prefix like `include`, `exclude`).
///
/// Returns `Ok(Some(rule))` if the line matches a long-form keyword, `Ok(None)`
/// if no keyword matched, or `Err` if a keyword was followed by a comma and an
/// invalid modifier.
///
/// upstream: exclude.c:1218-1227 rule_strcmp - a keyword matches only when it is
/// followed by one of the accepted separators: whitespace, `_`, `,`, or the end
/// of the string. Whitespace/`_`/end terminate the keyword and the remaining
/// text is the pattern verbatim; a `,` instead introduces modifiers, which are
/// parsed by the same modifier loop as short-form prefixes (exclude.c:1241-1487).
fn try_parse_long_form(
    line: &str,
    source_path: &Path,
    line_num: usize,
) -> Result<Option<ParsedLine>, MergeFileError> {
    const KEYWORDS: &[(&str, ShortFormAction)] = &[
        ("include", ShortFormAction::Include),
        ("exclude", ShortFormAction::Exclude),
        ("protect", ShortFormAction::Protect),
        ("risk", ShortFormAction::Risk),
        ("merge", ShortFormAction::Merge),
        ("dir-merge", ShortFormAction::DirMerge),
        ("hide", ShortFormAction::Hide),
        ("show", ShortFormAction::Show),
    ];

    let bytes = line.as_bytes();
    for &(keyword, action) in KEYWORDS {
        let klen = keyword.len();
        // upstream: rule_strcmp uses strncmp, a case-sensitive byte compare, so
        // only the exact lowercase keyword matches (`EXCLUDE`/`Merge` do not).
        if bytes.len() < klen || &bytes[..klen] != keyword.as_bytes() {
            continue;
        }
        // The matched prefix is all ASCII, so `klen` is a valid char boundary.
        let after = &line[klen..];
        match after.chars().next() {
            // upstream: rule_strcmp returns `str + rule_len` for `,`, so the
            // modifier loop begins after the comma (e.g. `dir-merge,n .filt`).
            Some(',') => {
                let prefix_specifies_side = matches!(
                    action,
                    ShortFormAction::Hide
                        | ShortFormAction::Show
                        | ShortFormAction::Protect
                        | ShortFormAction::Risk
                );
                let (mods, pattern) = parse_modifiers(
                    &after[','.len_utf8()..],
                    action.is_merge(),
                    prefix_specifies_side,
                    line,
                    source_path,
                    line_num,
                )?;
                return complete_rule(action, mods, pattern, line, source_path, line_num).map(Some);
            }
            // upstream: rule_strcmp accepts isspace/`_` as terminating
            // separators; exactly one is consumed and the remainder is the
            // pattern verbatim (strlen, no trimming), so trailing and embedded
            // whitespace stays part of the pattern.
            Some(ch) if ch == '_' || is_c_isspace(ch) => {
                let pattern = &after[ch.len_utf8()..];
                return complete_rule(
                    action,
                    RuleModifiers::default(),
                    pattern,
                    line,
                    source_path,
                    line_num,
                )
                .map(Some);
            }
            // upstream: rule_strcmp accepts end-of-string (`!str[rule_len]`); the
            // keyword stands alone and the pattern is empty.
            None => {
                return complete_rule(
                    action,
                    RuleModifiers::default(),
                    "",
                    line,
                    source_path,
                    line_num,
                )
                .map(Some);
            }
            // Any other trailing byte means this is not the keyword (e.g.
            // `excludes`); fall through to the next candidate.
            Some(_) => continue,
        }
    }

    Ok(None)
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
) -> Result<ParsedLine, MergeFileError> {
    // upstream: exclude.c parse_rule_tok() - the bare `!` and the `clear` long
    // name are the two spellings of FILTRULE_CLEAR_LIST, and each leaves one
    // character for `if (*s) s++`, so anything left over (a lone trailing space
    // included) is a trailing-characters error. `RULE_STRCMP` is a
    // case-sensitive strncmp, so `CLEAR`/`Clear` are not the directive.
    match classify_clear_token(line.as_bytes()) {
        ClearToken::Clear => return Ok(ParsedLine::Rule(FilterRule::clear())),
        ClearToken::TrailingCharacters => {
            // upstream: exclude.c:1470 filter_rule_err, which renders the rule
            // through rule_text (exclude.c:135).
            let name = source_path.display().to_string();
            let source = rule_file_source(&name, line_num);
            return Err(MergeFileError::parse_error(
                source_path,
                line_num,
                format!(
                    "'!' rule has trailing characters: {}",
                    source.rule_text(line)
                ),
            ));
        }
        ClearToken::NotClearToken => {}
    }

    if let Some(rule) = try_parse_short_form(line, source_path, line_num)? {
        return Ok(rule);
    }

    if let Some(rule) = try_parse_long_form(line, source_path, line_num)? {
        return Ok(rule);
    }

    // upstream: exclude.c:1363 filter_rule_err("Unknown filter rule", *rulestr_ptr),
    // rendered through rule_text (exclude.c:135). Upstream does not quote the
    // text, and quoting a `<rule from ...>` replacement would be meaningless, so
    // the backticks go with the raw text they used to wrap.
    let name = source_path.display().to_string();
    let source = rule_file_source(&name, line_num);
    Err(MergeFileError::parse_error(
        source_path,
        line_num,
        format!("Unknown filter rule: {}", source.rule_text(line)),
    ))
}
