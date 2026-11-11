//! upstream's `parse_filter_str()`, and the steps of `add_rule()` its `xflags`
//! switch on.
//!
//! upstream: exclude.c:1516-1598 parse_filter_str(), exclude.c:256-317
//! add_rule(). Every caller parses through here - the client's argv options
//! with no flags, and a daemon module's directives with `XFLG_ABS_IF_SLASH |
//! XFLG_DIR2WILD3` (clientserver.c:933-952) - so the rule grammar has one owner.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use filters::{CvsSession, RuleSource};

use super::FilterFileSource;
use super::cvs::cvs_default_exclude_rules;
use super::directive::{FilterDirective, merge_directive_options, os_string_to_pattern};
use super::merge::apply_merge_directive;
use super::parsing::{parse_filter_directive, parse_old_prefix_rule};
use crate::client::{DirMergeOptions, FilterRuleKind, FilterRuleSpec};
use crate::message::Message;

/// The `xflags` bits a parse passes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Xflags {
    /// `XFLG_OLD_PREFIXES`: only `- `, `+ ` and `!` prefix a rule
    /// (exclude.c:1276-1284).
    pub(crate) old_prefixes: bool,
    /// `XFLG_ABS_IF_SLASH`: a pattern holding a slash is anchored, and a rule
    /// for the other side is dropped as it is added (exclude.c:279-306).
    pub(crate) abs_if_slash: bool,
    /// `XFLG_DIR2WILD3`: a directory exclude `dir/` becomes `dir/***`
    /// (exclude.c:308-314).
    pub(crate) dir2wild3: bool,
}

/// The template a string is parsed against, upstream's `rule_template()`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Template {
    /// The kind a pattern with no prefix takes under `XFLG_OLD_PREFIXES`:
    /// `FILTRULE_INCLUDE` set or not.
    pub(crate) kind: FilterRuleKind,
    /// `FILTRULE_WORD_SPLIT`: the string holds one rule per word.
    pub(crate) word_split: bool,
}

/// What a parse reads merge files with.
pub(crate) struct ParseContext<'a> {
    /// upstream's `dirbuf`: where a relative merge-file name resolves.
    pub(crate) base_dir: &'a Path,
    /// The enclosing merge files, bounded at `MAX_MERGE_DEPTH`.
    pub(crate) merge_stack: &'a mut Vec<PathBuf>,
    /// The CVS template a `C` rule establishes.
    pub(crate) cvs: &'a CvsSession,
    /// Where the files the rules name are read from.
    pub(crate) files: &'a dyn FilterFileSource,
}

/// Parses `text` against `template` and appends its rules to `list`.
///
/// upstream: exclude.c:1516-1598 parse_filter_str(). A word-split template
/// holds one rule per word, and the text is the operator's own, so a malformed
/// rule is shown verbatim (`rule_src_in_file == 0`, exclude.c:67-69).
///
/// # Errors
///
/// The first malformed rule or unreadable merge file, as upstream reports it
/// before exiting.
pub(crate) fn parse_filter_str(
    list: &mut Vec<FilterRuleSpec>,
    text: &OsStr,
    template: Template,
    xflags: Xflags,
    ctx: &mut ParseContext<'_>,
) -> Result<(), Message> {
    if !template.word_split {
        return parse_rule(list, text, template, xflags, ctx);
    }
    let text = text.to_string_lossy();
    let mut rest = text.as_ref();
    loop {
        // upstream: exclude.c:1250-1255 - a word-split parse skips leading
        // whitespace, and a string with nothing left ends it.
        rest = rest.trim_start_matches(|ch: char| ch.is_ascii_whitespace());
        if rest.is_empty() {
            return Ok(());
        }
        let len = if xflags.old_prefixes {
            old_prefix_rule_len(rest)
        } else {
            rule_len(rest)
        };
        let (rule, tail) = rest.split_at(len);
        parse_rule(list, OsStr::new(rule), template, xflags, ctx)
            .map_err(|error| reported_from(error, rule, rest))?;
        rest = tail;
    }
}

/// A word-split rule's refusal, naming the string from the rule onward.
///
/// upstream: filter_rule_err() prints `*rulestr_ptr` (exclude.c:1363-1475),
/// which still runs to the end of the string, so `filter = - foo clear,- keep`
/// reports `clear,- keep` rather than the one rule `clear,-`.
fn reported_from(error: Message, rule: &str, rest: &str) -> Message {
    match error.text().strip_suffix(rule) {
        Some(head) => {
            let text = format!("{head}{rest}");
            error.with_text(text)
        }
        None => error,
    }
}

/// Parses one rule and appends what it stands for.
fn parse_rule(
    list: &mut Vec<FilterRuleSpec>,
    text: &OsStr,
    template: Template,
    xflags: Xflags,
    ctx: &mut ParseContext<'_>,
) -> Result<(), Message> {
    let directive = if xflags.old_prefixes {
        let text = os_string_to_pattern(text.to_os_string());
        parse_old_prefix_rule(&text, template.kind, RuleSource::Argument)?
    } else {
        parse_filter_directive(text, RuleSource::Argument)?
    };
    match directive {
        FilterDirective::Rule(rule) => {
            if let Some(options) = rule.dir_merge_options().filter(|o| o.excludes_self()) {
                // upstream: exclude.c:1558-1572 - `e` adds an exclude of the
                // merge file's basename ahead of the rule, unflagged, and
                // clears FILTRULE_EXCLUDE_SELF from the rule itself.
                let options = options.clone().exclude_filter_file(false);
                list.push(FilterRuleSpec::exclude(
                    filters::merge_file_basename(rule.pattern()).to_owned(),
                ));
                add_rule(list, rule.with_dir_merge_options(options), xflags);
            } else {
                add_rule(list, rule, xflags);
            }
        }
        FilterDirective::Merge(directive) => {
            let options = merge_directive_options(&DirMergeOptions::default(), &directive);
            // The operator wrote this merge rule, so the file's name is theirs
            // to see: upstream's non-redacted open-failure arm
            // (exclude.c:1713-1717, errno included).
            apply_merge_directive(
                directive.with_options(options),
                ctx.base_dir,
                list,
                ctx.merge_stack,
                RuleSource::Argument,
                ctx.cvs,
                ctx.files,
            )?;
        }
        // upstream: exclude.c:1542-1551 - `pop_filter_list(listp);
        // listp->head = NULL`, so no clear rule survives to be sent.
        FilterDirective::Clear => list.clear(),
        // upstream: exclude.c:1595-1597 - a non-merge `C` rule stands for the
        // CVS ignore list at this point in the chain.
        FilterDirective::CvsDefaults(template) => list.extend(
            cvs_default_exclude_rules(template, ctx.cvs)
                .into_iter()
                .filter(|rule| !dropped_for_other_side(rule, xflags)),
        ),
        // upstream: parse_rule_tok() returns NULL for an empty rule.
        FilterDirective::Noop => {}
    }
    Ok(())
}

/// Appends `rule`, applying what `xflags` changes as it is added.
///
/// upstream: exclude.c:279-314 add_rule().
pub(crate) fn add_rule(list: &mut Vec<FilterRuleSpec>, rule: FilterRuleSpec, xflags: Xflags) {
    if dropped_for_other_side(&rule, xflags) {
        return;
    }
    if rule.kind() == FilterRuleKind::DirMerge {
        // upstream: exclude.c:297-298 - a merge rule's name is a path, never
        // a pattern, so neither rewrite below touches it.
        list.push(rule);
        return;
    }
    let pattern = rule.pattern();
    // upstream: exclude.c:287-290 - a trailing slash marks a directory rule
    // and is not counted as one of the pattern's slashes.
    let body = match pattern.strip_suffix('/') {
        Some(body) if !body.is_empty() => body,
        _ => pattern,
    };
    let directory = body.len() < pattern.len();
    // upstream: exclude.c:297-306 - XFLG_ABS_IF_SLASH anchors a pattern that
    // holds a slash. A leading `/` already anchors in oc's encoding, and a
    // leading `**` is left alone: upstream marks it FILTRULE_WILD2_PREFIX
    // (exclude.c:335-339), which matches at the root without the anchor, while
    // prepending `/` would turn `**/*.o` into `/**/*.o` and lose that prefix.
    let anchor = xflags.abs_if_slash
        && !rule.is_abs_path()
        && !pattern.starts_with('/')
        && !pattern.starts_with("**")
        && body.contains('/');
    // upstream: exclude.c:308-314 - XFLG_DIR2WILD3 turns a directory rule that
    // is not an include into `dir/***`, so it covers the directory's contents.
    let wild3 = xflags.dir2wild3
        && directory
        && matches!(
            rule.kind(),
            FilterRuleKind::Exclude | FilterRuleKind::Protect
        );
    if !anchor && !wild3 {
        list.push(rule);
        return;
    }
    let mut rewritten = String::with_capacity(pattern.len() + 4);
    if anchor {
        rewritten.push('/');
    }
    if wild3 {
        rewritten.push_str(body);
        rewritten.push_str("/***");
    } else {
        rewritten.push_str(pattern);
    }
    list.push(rule.with_pattern(rewritten));
}

/// Whether `add_rule` drops `rule` as one for the other side.
///
/// upstream: exclude.c:279-285 - under XFLG_ANCHORED2ABS|XFLG_ABS_IF_SLASH a
/// rule whose sides are exactly the other side's is dropped. The only parse
/// that passes the flag here is a daemon module's, which runs before
/// parse_arguments() sets `am_sender`, so the other side is always the sender:
/// `hide`, `show` and the `s` modifier go, `protect`, `risk` and `r` stay.
fn dropped_for_other_side(rule: &FilterRuleSpec, xflags: Xflags) -> bool {
    if !xflags.abs_if_slash {
        return false;
    }
    match rule.dir_merge_options() {
        Some(options) => {
            options.sender_side_override() == Some(true)
                && options.receiver_side_override() != Some(true)
        }
        None => rule.applies_to_sender() && !rule.applies_to_receiver(),
    }
}

/// The bytes that end a long rule keyword.
///
/// upstream: exclude.c:1218-1227 rule_strcmp() - whitespace, `_`, `,` or the
/// end of the string.
fn is_keyword_end(byte: u8) -> bool {
    byte == b'_' || byte == b',' || byte.is_ascii_whitespace()
}

/// The long keywords, each reachable from its own first byte.
///
/// upstream: exclude.c:1288-1330 dispatches on a rule's first byte and tries
/// exactly one keyword.
const KEYWORDS: &[&str] = &[
    "clear",
    "dir-merge",
    "exclude",
    "hide",
    "include",
    "merge",
    "protect",
    "risk",
    "show",
];

/// Bytes up to the first ASCII whitespace.
///
/// upstream: exclude.c:1459-1461 - `while (!isspace(*cp) && *cp != '\0') cp++`.
fn word_len(s: &str) -> usize {
    s.find(|ch: char| ch.is_ascii_whitespace())
        .unwrap_or(s.len())
}

/// Length of the one word-split rule at the front of `s`.
///
/// This is parse_rule_tok()'s positional walk (exclude.c:1241-1486): the rule
/// character or keyword, its modifier run, ONE separator byte, then a pattern
/// ending at whitespace. The walk never looks at the next word, so `- foo bar`
/// is two rules and `- !bait` is one.
pub(super) fn rule_len(s: &str) -> usize {
    let b = s.as_bytes();
    let n = b.len();
    // `i` tracks upstream's `s`: the LAST byte of the prefix, where
    // rule_strcmp() leaves it (exclude.c:1222-1225).
    let mut i;
    let is_clear;
    match KEYWORDS.iter().find(|kw| kw.as_bytes()[0] == b[0]) {
        Some(keyword) => {
            let k = keyword.len();
            let matched = s.starts_with(keyword) && (n == k || is_keyword_end(b[k]));
            if !matched {
                // upstream exits RERR_SYNTAX on an unknown rule
                // (exclude.c:1363); one word is what the parser refuses.
                return word_len(s);
            }
            // `,` is the one terminator rule_strcmp() does not step back over.
            i = if n > k && b[k] == b',' { k } else { k - 1 };
            is_clear = *keyword == "clear";
        }
        None => {
            // upstream: exclude.c:1325-1329 `ch = *s; if (s[1] == ',') s++;`
            i = usize::from(n > 1 && b[1] == b',');
            is_clear = b[0] == b'!';
        }
    }
    // upstream: exclude.c:1365-1370 - the modifier run, skipped for `!`. Under
    // FILTRULE_WORD_SPLIT whitespace other than a space ends the run one byte
    // early, so the separator step lands on it and the pattern comes out
    // empty: `exclude<TAB>foo` is a syntax error.
    if !is_clear {
        let mut k = i + 1;
        while k < n && b[k] != b' ' && b[k] != b'_' {
            if b[k].is_ascii_whitespace() {
                k -= 1;
                break;
            }
            k += 1;
        }
        i = k;
    }
    // upstream: exclude.c:1444-1445 `if (*s) s++` - exactly one separator.
    if i < n {
        i += 1;
    }
    i + word_len(&s[i..])
}

/// Length of the one word-split rule at the front of `s` under
/// `XFLG_OLD_PREFIXES`.
///
/// upstream: exclude.c:1276-1284 - a `- ` or `+ ` prefix takes two bytes and
/// the pattern runs to the next whitespace (exclude.c:1457-1462), so `- foo bar`
/// is an exclude of `foo` followed by a template rule for `bar`.
fn old_prefix_rule_len(s: &str) -> usize {
    let prefix = if s.starts_with("- ") || s.starts_with("+ ") {
        2
    } else {
        0
    };
    prefix + word_len(&s[prefix..])
}
