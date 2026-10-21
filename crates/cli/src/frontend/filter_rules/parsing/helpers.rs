use core::message::{Message, Role};
use core::rsync_error;

use super::rule_line::RuleLine;

/// Builds upstream's `Unknown filter rule` refusal for `line`.
///
/// upstream: exclude.c:1363 `filter_rule_err("Unknown filter rule", *rulestr_ptr)`,
/// which renders the rule through `rule_text` (exclude.c:103-124) and exits
/// `RERR_SYNTAX`. The rule text crosses that same chokepoint here via
/// `line.shown()`: a merged line is one the peer chose, so echoing it verbatim
/// would make the parser a read-any-line oracle.
pub(super) fn unknown_filter_rule(line: RuleLine<'_>) -> Message {
    rsync_error!(1, format!("Unknown filter rule: {}", line.shown())).with_role(Role::Client)
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
    .with_role(Role::Client)
}

/// Consumes exactly one rule separator (a single space, `_`, or `,`) that
/// terminates the rule character and its modifiers, returning the rest of the
/// line verbatim.
///
/// upstream: exclude.c:1365,1444-1445 - after the modifier loop stops at the
/// first ` `/`_` separator, `if (*s) s++` consumes exactly ONE separator
/// character. The pattern length is then `strlen` (exclude.c:1465), so any
/// further leading whitespace or `_` stays part of the pattern verbatim and is
/// never trimmed. The `,` case mirrors the optional comma that may follow a rule
/// character or keyword (exclude.c:1224-1225, 1327-1328). A non-separator leading character
/// is returned unchanged.
pub(super) fn consume_rule_separator(remainder: &str) -> &str {
    let mut chars = remainder.chars();
    match chars.next() {
        Some(ch) if ch == '_' || ch == ',' || ch.is_ascii_whitespace() => chars.as_str(),
        _ => remainder,
    }
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
        Some(',') => {
            let body = chars.as_str();
            Some(match body.find(is_rule_separator) {
                // The separator is ASCII, so `idx + 1` stays on a char boundary.
                Some(idx) => (&body[..idx], &body[idx + 1..]),
                None => (body, ""),
            })
        }
        Some(ch) if is_rule_separator(ch) => Some(("", &after[ch.len_utf8()..])),
        Some(_) => None,
    }
}

/// The bytes that end a rule keyword or modifier run in this parser.
///
/// upstream's modifier loop terminates on `' '`/`'_'` alone (exclude.c:1365),
/// while `rule_strcmp` accepts any `isspace` (exclude.c:1222); a tab therefore
/// reaches upstream's `default:` arm as an invalid modifier. oc has treated the
/// whole ASCII whitespace class as a separator throughout this module since
/// before these parsers existed, and this helper keeps that one convention
/// rather than introducing a second.
fn is_rule_separator(ch: char) -> bool {
    ch == '_' || ch.is_ascii_whitespace()
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
    let body = text.strip_prefix(',').unwrap_or(text);
    match body.find(is_rule_separator) {
        Some(idx) => (&body[..idx], consume_rule_separator(&body[idx..])),
        None => (body, ""),
    }
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
        .with_role(Role::Client)
    })
}

/// Splits a long-form keyword token into `(name, modifiers)` at the first
/// comma, returning empty modifiers when no comma is present.
pub(super) fn split_keyword_modifiers(keyword: &str) -> (&str, &str) {
    if let Some((name, modifiers)) = keyword.split_once(',') {
        (name, modifiers)
    } else {
        (keyword, "")
    }
}

#[cfg(test)]
mod tests {

    use filters::RuleSource;

    fn arg(text: &str) -> super::RuleLine<'_> {
        super::RuleLine::new(text, RuleSource::Argument)
    }
    use super::*;

    #[test]
    fn consume_rule_separator_basic() {
        // A non-separator leading character is returned unchanged.
        assert_eq!(consume_rule_separator("pattern"), "pattern");
    }

    #[test]
    fn consume_rule_separator_leading_whitespace() {
        // Exactly one separator is consumed; any further whitespace stays in the
        // pattern verbatim (upstream exclude.c:1444-1445 `if (*s) s++`).
        assert_eq!(consume_rule_separator("  pattern"), " pattern");
        assert_eq!(consume_rule_separator("\t\tpattern"), "\tpattern");
        assert_eq!(consume_rule_separator(" pattern"), "pattern");
    }

    #[test]
    fn consume_rule_separator_leading_underscore() {
        assert_eq!(consume_rule_separator("__pattern"), "_pattern");
        assert_eq!(consume_rule_separator("_ pattern"), " pattern");
        assert_eq!(consume_rule_separator("_pattern"), "pattern");
    }

    #[test]
    fn consume_rule_separator_comma_prefix() {
        assert_eq!(consume_rule_separator(",pattern"), "pattern");
        assert_eq!(consume_rule_separator(", pattern"), " pattern");
        assert_eq!(consume_rule_separator(",_pattern"), "_pattern");
    }

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

    #[test]
    fn split_keyword_modifiers_no_comma() {
        assert_eq!(split_keyword_modifiers("include"), ("include", ""));
    }

    #[test]
    fn split_keyword_modifiers_with_comma() {
        assert_eq!(split_keyword_modifiers("include,sr"), ("include", "sr"));
    }

    #[test]
    fn split_keyword_modifiers_multiple_commas() {
        assert_eq!(split_keyword_modifiers("a,b,c"), ("a", "b,c"));
    }

    #[test]
    fn split_keyword_modifiers_empty() {
        assert_eq!(split_keyword_modifiers(""), ("", ""));
    }
}
