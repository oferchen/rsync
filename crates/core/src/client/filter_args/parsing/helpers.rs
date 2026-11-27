use crate::message::Message;
use crate::rsync_error;

use super::rule_line::RuleLine;

/// Builds upstream's `Unknown filter rule` refusal for `line`.
///
/// upstream: exclude.c:1363 `filter_rule_err("Unknown filter rule", *rulestr_ptr)`,
/// which renders the rule through `rule_text` (exclude.c:103-124) and exits
/// `RERR_SYNTAX`. The rule text crosses that same chokepoint here via
/// `line.shown()`: a merged line is one the peer chose, so echoing it verbatim
/// would make the parser a read-any-line oracle.
pub(super) fn unknown_filter_rule(line: RuleLine<'_>) -> Message {
    rsync_error!(1, format!("Unknown filter rule: {}", line.shown()))
}

/// Builds upstream's `unexpected end of filter rule` refusal for `line`.
///
/// upstream: exclude.c:1475 `filter_rule_err("unexpected end of filter rule",
/// *rulestr_ptr)`, reached whenever a rule carries no pattern (`!len`) - a bare
/// prefix, an empty include/exclude, or a merge/dir-merge with no file name.
/// The rule text crosses `rule_text` (exclude.c:103-124) via `line.shown()`.
pub(super) fn unexpected_end_of_filter_rule(line: RuleLine<'_>) -> Message {
    rsync_error!(
        1,
        format!("unexpected end of filter rule: {}", line.shown())
    )
}

/// Splits the text that follows a long-form filter keyword into
/// `(modifiers, pattern)`, or returns `None` when the keyword is not terminated
/// by a separator and is therefore not this keyword at all.
///
/// upstream: exclude.c:1218-1227 `rule_strcmp` - the keyword matches only when
/// the byte after it is whitespace, `_`, `,`, or the end of the string. Any
/// other byte returns NULL, `ch` stays 0, and the rule falls to the inner
/// switch's `default:` and dies with `Unknown filter rule` (exclude.c:1363), so
/// `mergeX` is a syntax error rather than a merge of `X`.
///
/// A `,` returns `str + rule_len` (exclude.c:1224-1225), which puts the modifier
/// loop's `*++s` on the first modifier byte. Every other separator returns
/// `str + rule_len - 1` (exclude.c:1222-1223), so `*++s` lands ON the separator and
/// the loop stops at once: a long keyword only ever carries modifiers after a
/// comma. Either way exactly ONE separator is then consumed by `if (*s) s++`
/// (exclude.c:1444-1445) and the pattern is the remainder verbatim
/// (`len = strlen(s)`, exclude.c:1465) - never a whole run of whitespace.
///
/// MEASURED against rsync 3.5.0: `--filter='dir-merge  '` (two trailing spaces)
/// merges a per-directory file literally named ` `, `--filter='merge  X'`
/// tries to open ` X`, and `--filter='mergeX'` exits 1 with `Unknown filter rule`.
pub(super) fn split_long_keyword_tail(after: &str) -> Option<(&str, &str)> {
    let mut chars = after.chars();
    match chars.next() {
        None => Some(("", "")),
        Some(',') => Some(split_modifier_run(chars.as_str())),
        Some(ch) if is_rule_separator(ch) => Some(("", &after[ch.len_utf8()..])),
        // upstream: rule_strcmp accepts any isspace (exclude.c:1222), but the
        // modifier loop's `*++s` then lands ON it and only ` `/`_` stop the loop
        // (exclude.c:1365), so a tab is the first modifier byte and is refused.
        // MEASURED against 3.5.2: `--filter=$'exclude\tfoo'` exits 1 with
        // `invalid modifier '\t' at position 7 in filter rule: exclude\tfoo`.
        Some(ch) if filters::is_c_isspace(ch) => Some(split_modifier_run(after)),
        Some(_) => None,
    }
}

/// The bytes that end a modifier run: `' '` and `'_'` alone.
///
/// upstream: exclude.c:1365 - `while (ch != '!' && *++s && *s != ' ' && *s !=
/// '_')`. A tab or any other whitespace is not a separator; it reaches the
/// modifier `switch` default and is refused as an invalid modifier
/// (exclude.c:1371-1380). MEASURED against 3.5.2: `--filter=$'-\tfoo'` exits 1
/// with `invalid modifier '\t' at position 1 in filter rule: -\tfoo`.
fn is_rule_separator(ch: char) -> bool {
    ch == ' ' || ch == '_'
}

/// Splits `body` at its first separator into `(modifiers, pattern)`.
///
/// upstream: exclude.c:1444-1445 `if (*s) s++` consumes exactly ONE separator,
/// and the pattern is then the remainder verbatim (`len = strlen(s)`,
/// exclude.c:1465), so further spaces or `_` stay part of it. With no separator
/// the whole body is modifiers and the pattern is empty.
fn split_modifier_run(body: &str) -> (&str, &str) {
    match body.find(is_rule_separator) {
        // The separator is ASCII, so `idx + 1` stays on a char boundary.
        Some(idx) => (&body[..idx], &body[idx + 1..]),
        None => (body, ""),
    }
}

/// Splits the text after a single-character rule prefix into
/// `(modifiers, pattern)`.
///
/// Upstream consumes at most ONE optional `,` after the prefix
/// (exclude.c:1327-1328), then reads modifiers until the first ` `/`_`
/// separator and consumes exactly that one (exclude.c:1365, 1444-1445). Every
/// byte of the run is handed to the shared modifier table
/// ([`parse_modifier_run`]), which refuses a further `,` and any other
/// non-modifier the way upstream's single modifier `switch` does
/// (exclude.c:1365-1443). With no separator the whole text is modifiers and the
/// pattern is empty, so `+foo` is refused on its `f` rather than read as a
/// pattern.
pub(super) fn split_short_modifiers(text: &str) -> (&str, &str) {
    split_modifier_run(text.strip_prefix(',').unwrap_or(text))
}

/// Runs a modifier run through the shared table, rendering a refusal the way
/// upstream's `invalid:` label does (exclude.c:1371-1380):
/// `invalid modifier '%c' at position %d in filter rule: %s`.
///
/// The position is a 0-based byte offset from the start of the whole rule
/// (`s - *rulestr_ptr`, exclude.c:1374-1375), so `run` should be a slice of
/// `line.text()`.
///
/// BOTH halves cross a provenance chokepoint. The rule text goes through
/// `rule_text` (exclude.c:103-124) and the `'%c' at position %d` detail
/// through `rule_detail` (exclude.c:126-131), which upstream describes as
/// "the extra detail some messages add ABOUT the text - a character of it,
/// an offset into it. Dropped along with the text it describes." Reporting
/// the character while hiding the line would still leak it one byte at a
/// time.
pub(super) fn parse_modifier_run(
    run: &str,
    scope: filters::ModifierScope,
    line: RuleLine<'_>,
) -> Result<filters::RuleModifiers, Message> {
    filters::parse_rule_modifiers_in(line.text(), run, scope).map_err(|invalid| {
        let detail = format!(" '{}' at position {}", invalid.ch, invalid.offset);
        rsync_error!(
            1,
            format!(
                "invalid modifier{} in filter rule: {}",
                line.detail(&detail),
                line.shown()
            )
        )
    })
}

#[cfg(test)]
mod tests {

    use filters::RuleSource;

    fn arg(text: &str) -> super::RuleLine<'_> {
        super::RuleLine::new(text, RuleSource::Argument)
    }
    use super::*;

    fn split_merge(text: &str) -> (&str, &str) {
        split_short_modifiers(text)
    }

    fn split_rule(text: &str) -> (&str, &str) {
        split_short_modifiers(text)
    }

    /// The refusal `parse_modifier_run` renders for the run a merge rule's
    /// text splits into.
    fn merge_refusal(rule: &str) -> String {
        let (run, _) = split_merge(&rule[1..]);
        parse_modifier_run(run, filters::ModifierScope::MERGE, arg(rule))
            .expect_err("the run must be refused")
            .to_string()
    }

    #[test]
    fn split_short_rule_modifiers_empty() {
        assert_eq!(split_rule(""), ("", ""));
    }

    #[test]
    fn split_short_rule_modifiers_no_separator_is_all_modifiers() {
        // upstream: exclude.c:1365-1443 - with no ` `/`_` separator after the
        // prefix, every byte is a modifier and the pattern is empty. The caller
        // then rejects the unknown modifier bytes (e.g. `+foo` -> invalid 'f').
        assert_eq!(split_rule("pattern"), ("pattern", ""));
    }

    #[test]
    fn split_short_rule_modifiers_whitespace_start() {
        assert_eq!(split_rule(" pattern"), ("", "pattern"));
    }

    #[test]
    fn split_short_rule_modifiers_underscore_start() {
        assert_eq!(split_rule("_pattern"), ("", "pattern"));
    }

    #[test]
    fn split_short_rule_modifiers_space_separated() {
        assert_eq!(split_rule("sr pattern"), ("sr", "pattern"));
    }

    #[test]
    fn split_short_rule_modifiers_consumes_one_leading_comma() {
        // MEASURED against rsync 3.5.0: `--filter='-,s a'` exits 0. The comma
        // that may follow the rule character (exclude.c:1327-1328) is consumed
        // once and is not itself a modifier.
        assert_eq!(split_rule(",s a"), ("s", "a"));
    }

    #[test]
    fn rule_run_refuses_a_comma_inside_or_doubled() {
        // MEASURED against rsync 3.5.0: `--filter='-s,r a'` and `--filter='-,, a'`
        // exit 1 with `invalid modifier ',' at position 2`. Only ONE comma is
        // consumed, so a second one reaches `default:` (exclude.c:1371-1379).
        for rule in ["-s,r a", "-,, a"] {
            let (run, _) = split_rule(&rule[1..]);
            let refusal = parse_modifier_run(run, filters::ModifierScope::PLAIN, arg(rule))
                .expect_err("`,` is never a modifier")
                .to_string();
            assert!(
                refusal.contains("invalid modifier ',' at position 2"),
                "{rule}: {refusal}"
            );
        }
    }

    #[test]
    fn split_short_merge_modifiers_empty() {
        assert_eq!(split_merge(""), ("", ""));
    }

    #[test]
    fn split_short_merge_modifiers_base() {
        assert_eq!(split_merge("+-cs pattern"), ("+-cs", "pattern"));
    }

    #[test]
    fn split_short_merge_modifiers_accepts_e_and_n() {
        // upstream exclude.c:1410-1419 gates `e`/`n` on FILTRULE_MERGE_FILE,
        // which a plain merge (`.`) sets too, so they are modifiers for both
        // merge forms.
        assert_eq!(split_merge("ce pattern"), ("ce", "pattern"));
        assert_eq!(split_merge("cen pattern"), ("cen", "pattern"));
    }

    #[test]
    fn split_short_merge_modifiers_whitespace_start() {
        assert_eq!(split_merge(" pattern"), ("", "pattern"));
    }

    #[test]
    fn merge_run_refuses_an_unknown_byte_at_its_rule_position() {
        // MEASURED against rsync 3.5.0: `--filter=':n.rsync-filter'` exits 1
        // with `invalid modifier '.' at position 2`.
        assert!(
            merge_refusal(":n.rsync-filter")
                .contains("invalid modifier '.' at position 2 in filter rule: :n.rsync-filter")
        );
    }

    #[test]
    fn merge_run_after_a_comma_is_measured_from_the_rule_start() {
        // MEASURED against rsync 3.5.0: `--filter=':,pattern'` exits 1 with
        // `invalid modifier 'a' at position 3` - the leading `,` is consumed and
        // `p` IS a valid modifier, so the failure lands on `a`.
        assert!(merge_refusal(":,pattern").contains("invalid modifier 'a' at position 3"));
        // `--filter=':,,f'` exits 1 with `invalid modifier ',' at position 2`.
        assert!(merge_refusal(":,,f").contains("invalid modifier ',' at position 2"));
    }

    /// upstream: exclude.c:1381-1391, 1402-1404 - a second `-`/`+`, or a `C`
    /// after one (and vice versa), reaches `invalid`. MEASURED against rsync
    /// 3.5.1: each of these fails with `invalid modifier` at position 2.
    #[test]
    fn merge_run_refuses_a_second_prefix_modifier() {
        for (rule, ch) in [
            (":-C", 'C'),
            (":C-", '-'),
            (":--", '-'),
            (":++", '+'),
            (":+-", '-'),
        ] {
            assert!(
                merge_refusal(rule).contains(&format!(
                    "invalid modifier '{ch}' at position 2 in filter rule: {rule}"
                )),
                "{rule}"
            );
        }
    }

    /// The refusal a keyword rule's tail is given, or `None` when it parses.
    fn keyword_refusal(rule: &str, keyword: &str) -> Option<String> {
        let (run, _) = split_long_keyword_tail(&rule[keyword.len()..])?;
        parse_modifier_run(run, filters::ModifierScope::PLAIN, arg(rule))
            .err()
            .map(|refusal| refusal.to_string())
    }

    /// upstream: exclude.c:1365 - only ` ` and `_` end the modifier run, and
    /// exclude.c:1444-1445 consumes exactly one of them. MEASURED against 3.5.2:
    /// `-_foo` and `- foo` exclude `foo`, `-  foo` excludes ` foo`.
    #[test]
    fn split_modifier_run_ends_only_at_space_or_underscore() {
        for (body, expected) in [
            (" foo", ("", "foo")),
            ("_foo", ("", "foo")),
            ("  foo", ("", " foo")),
            ("__foo", ("", "_foo")),
            ("_ foo", ("", " foo")),
            ("p foo", ("p", "foo")),
            ("\tfoo", ("\tfoo", "")),
            ("p\tfoo bar", ("p\tfoo", "bar")),
            ("\x0bfoo", ("\x0bfoo", "")),
        ] {
            assert_eq!(split_modifier_run(body), expected, "{body:?}");
        }
    }

    /// upstream: exclude.c:1222 rule_strcmp accepts any isspace after a
    /// keyword, but the modifier loop then refuses every one but ` `
    /// (exclude.c:1365, 1376). MEASURED against 3.5.2: `exclude<TAB>foo` exits
    /// 1 with `invalid modifier '<TAB>' at position 7`, while `exclude foo`,
    /// `exclude_foo` and `exclude  foo` parse.
    #[test]
    fn long_keyword_refuses_non_space_whitespace_as_a_modifier() {
        for (rule, refused_at) in [
            ("exclude foo", None),
            ("exclude_foo", None),
            ("exclude  foo", None),
            ("exclude,p foo", None),
            ("exclude\tfoo", Some(('\t', 7))),
            ("exclude\x0bfoo", Some(('\x0b', 7))),
            ("exclude\x0cfoo", Some(('\x0c', 7))),
            ("exclude\rfoo", Some(('\r', 7))),
            ("exclude,p\tfoo", Some(('\t', 9))),
        ] {
            let refusal = keyword_refusal(rule, "exclude");
            match refused_at {
                None => assert_eq!(refusal, None, "{rule:?}"),
                Some((ch, position)) => {
                    let expected = format!(
                        "invalid modifier '{ch}' at position {position} in filter rule: {rule}"
                    );
                    let refusal = refusal.unwrap_or_else(|| panic!("{rule:?} must be refused"));
                    assert!(refusal.contains(&expected), "{rule:?}: {refusal}");
                }
            }
        }
    }

    #[test]
    fn long_keyword_needs_a_terminator() {
        // upstream: rule_strcmp returns NULL for any other byte (exclude.c:1226).
        assert_eq!(split_long_keyword_tail("X"), None);
        assert_eq!(split_long_keyword_tail(""), Some(("", "")));
    }
}
