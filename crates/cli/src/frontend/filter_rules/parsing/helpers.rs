use core::message::{Message, Role};
use core::rsync_error;

/// Consumes exactly one rule separator (a single space, `_`, or `,`) that
/// terminates the rule character and its modifiers, returning the rest of the
/// line verbatim.
///
/// upstream: exclude.c:1290-1291 - after the modifier loop stops at the first
/// ` `/`_` separator, `if (*s) s++` consumes exactly ONE separator character.
/// The pattern length is then `strlen` (exclude.c:1313), so any further leading
/// whitespace or `_` stays part of the pattern verbatim and is never trimmed.
/// The `,` case mirrors the optional comma that may follow a rule character or
/// keyword (exclude.c:1075-1077, 1176-1177). A non-separator leading character
/// is returned unchanged.
pub(super) fn consume_rule_separator(remainder: &str) -> &str {
    let mut chars = remainder.chars();
    match chars.next() {
        Some(ch) if ch == '_' || ch == ',' || ch.is_ascii_whitespace() => chars.as_str(),
        _ => remainder,
    }
}

/// An unrecognised modifier byte, located by its offset within the scanned
/// slice. Callers add the offset of that slice within the whole rule to
/// reproduce upstream's absolute position.
#[derive(Debug)]
pub(super) struct InvalidModifier {
    /// The offending character.
    pub(super) ch: char,
    /// Byte offset of `ch` within the slice handed to the scanner.
    pub(super) offset: usize,
}

impl InvalidModifier {
    /// Renders upstream's diagnostic for this byte.
    ///
    /// upstream: exclude.c:1371-1379 - `invalid modifier '%c' at position %d in
    /// filter rule: %s`, where the position is
    /// `(int)(s - (const uchar *)*rulestr_ptr)`, i.e. a 0-based byte offset from
    /// the start of the whole rule. `rule_offset` is where the scanned slice
    /// begins within `rule`, so a one-character prefix passes 1.
    pub(super) fn into_message(self, rule_offset: usize, rule: &str) -> Message {
        let position = rule_offset + self.offset;
        let ch = self.ch;
        rsync_error!(
            1,
            format!("invalid modifier '{ch}' at position {position} in filter rule: {rule}")
        )
        .with_role(Role::Client)
    }
}

/// Scans the modifier run that follows a rule character.
///
/// `text` begins immediately after the rule character. Upstream consumes at
/// most ONE optional `,` there (exclude.c:1075-1077, :1176-1177), then reads
/// modifiers until the first ` `/`_` separator (exclude.c:1290-1291) and
/// consumes exactly that one separator. Every other byte reaches `default:`
/// and aborts with `RERR_SYNTAX` (exclude.c:1371-1379) - including a further
/// `,`, which is why `,` is not a run terminator here.
///
/// Measured against rsync 3.5.0: `-s,r a` and `:n,e f` both fail with
/// `invalid modifier ',' at position 2`, while `-,s a` succeeds; and
/// `:,pattern` fails on `a` at position 3 because the leading `,` is consumed
/// and `p` is a valid modifier.
fn scan_modifiers(
    text: &str,
    is_modifier: impl Fn(char) -> bool,
) -> Result<(&str, &str), InvalidModifier> {
    let (body, base) = match text.strip_prefix(',') {
        Some(rest) => (rest, 1usize),
        None => (text, 0usize),
    };

    for (idx, ch) in body.char_indices() {
        if ch == '_' || ch.is_ascii_whitespace() {
            return Ok((&body[..idx], consume_rule_separator(&body[idx..])));
        }
        if !is_modifier(ch) {
            return Err(InvalidModifier {
                ch,
                offset: base + idx,
            });
        }
    }

    // upstream: exclude.c:1214-1287 - with no separator at all the whole
    // remainder is modifiers and the pattern is empty, so `+foo` is rejected as
    // an invalid modifier rather than silently treated as a pattern.
    Ok((body, ""))
}

/// Splits a `+`/`-` short rule's text into `(modifiers, pattern)`.
///
/// Character validity for these rules is decided by the caller, so the scan
/// rejects only `,` - the one byte that can never be a modifier and that the
/// previous separator-based split silently swallowed.
pub(super) fn split_short_rule_modifiers(text: &str) -> Result<(&str, &str), InvalidModifier> {
    scan_modifiers(text, |ch| ch != ',')
}

/// Splits a `.`/`:` merge directive's text into `(modifiers, pattern)`.
///
/// upstream: exclude.c:1256-1264 - the `e` and `n` modifiers are guarded solely
/// by `FILTRULE_MERGE_FILE`, which `parse_rule_tok` sets for a plain merge (`.`,
/// exclude.c:1186) as well as a dir-merge (`:`), so both directives accept them.
pub(super) fn split_short_merge_modifiers(text: &str) -> Result<(&str, &str), InvalidModifier> {
    scan_modifiers(text, |ch| {
        matches!(
            ch.to_ascii_lowercase(),
            '+' | '-' | 'c' | 'w' | 's' | 'r' | 'p' | '/' | 'e' | 'n' | 'x'
        )
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
    use super::*;

    #[test]
    fn consume_rule_separator_basic() {
        // A non-separator leading character is returned unchanged.
        assert_eq!(consume_rule_separator("pattern"), "pattern");
    }

    #[test]
    fn consume_rule_separator_leading_whitespace() {
        // Exactly one separator is consumed; any further whitespace stays in the
        // pattern verbatim (upstream exclude.c:1290-1291 `if (*s) s++`).
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

    /// `(modifiers, pattern)` for a scan that must succeed.
    fn split_rule(text: &str) -> (&str, &str) {
        split_short_rule_modifiers(text).expect("modifier run must be accepted")
    }

    /// `(modifiers, pattern)` for a merge scan that must succeed.
    fn split_merge(text: &str) -> (&str, &str) {
        split_short_merge_modifiers(text).expect("modifier run must be accepted")
    }

    #[test]
    fn split_short_rule_modifiers_empty() {
        assert_eq!(split_rule(""), ("", ""));
    }

    #[test]
    fn split_short_rule_modifiers_no_separator_is_all_modifiers() {
        // upstream: exclude.c:1214-1287 - with no ` `/`_` separator after the
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
        // that may follow the rule character (exclude.c:1075-1077) is consumed
        // once and is not itself a modifier.
        assert_eq!(split_rule(",s a"), ("s", "a"));
    }

    #[test]
    fn split_short_rule_modifiers_rejects_a_comma_inside_the_run() {
        // MEASURED against rsync 3.5.0: `--filter='-s,r a'` exits 1 with
        // `invalid modifier ',' at position 2`. Only ONE comma is consumed, so a
        // second one reaches `default:` (exclude.c:1371-1379). The previous
        // separator-based split silently produced ("sr", "a") instead.
        let invalid = split_short_rule_modifiers("s,r a").expect_err("`,` is never a modifier");
        assert_eq!(invalid.ch, ',');
        assert_eq!(invalid.offset + '-'.len_utf8(), 2);
    }

    #[test]
    fn split_short_rule_modifiers_rejects_a_doubled_leading_comma() {
        // MEASURED against rsync 3.5.0: `--filter='-,, a'` exits 1 with
        // `invalid modifier ',' at position 2`.
        let invalid = split_short_rule_modifiers(",, a").expect_err("`,,` is not a modifier run");
        assert_eq!(invalid.ch, ',');
        assert_eq!(invalid.offset + '-'.len_utf8(), 2);
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
        // upstream exclude.c:1256-1264 gates `e`/`n` on FILTRULE_MERGE_FILE,
        // which a plain merge (`.`) sets too, so they are modifiers for both
        // merge forms. Previously `e` ended the run, so `.e FILE` mis-parsed as
        // the file name "e FILE".
        assert_eq!(split_merge("ce pattern"), ("ce", "pattern"));
        assert_eq!(split_merge("cen pattern"), ("cen", "pattern"));
    }

    #[test]
    fn split_short_merge_modifiers_whitespace_start() {
        assert_eq!(split_merge(" pattern"), ("", "pattern"));
    }

    #[test]
    fn split_short_merge_modifiers_rejects_an_unknown_byte() {
        // MEASURED against rsync 3.5.0: `--filter=':n.rsync-filter'` exits 1
        // with `invalid modifier '.' at position 2`. This is the headline case:
        // the run previously stopped at `.` and silently produced a dir-merge of
        // ".rsync-filter", exiting 0.
        let invalid =
            split_short_merge_modifiers("n.rsync-filter").expect_err("`.` is not a modifier");
        assert_eq!(invalid.ch, '.');
        assert_eq!(invalid.offset + ':'.len_utf8(), 2);
    }

    #[test]
    fn split_short_merge_modifiers_comma_prefix_scans_the_rest() {
        // MEASURED against rsync 3.5.0: `--filter=':,pattern'` exits 1 with
        // `invalid modifier 'a' at position 3` - the leading `,` is consumed and
        // `p` IS a valid modifier, so the failure lands on `a`, not on `p`. The
        // previous split returned ("p", "attern") and exited 0.
        let invalid = split_short_merge_modifiers(",pattern").expect_err("`a` is not a modifier");
        assert_eq!(invalid.ch, 'a');
        assert_eq!(invalid.offset + ':'.len_utf8(), 3);
    }

    #[test]
    fn split_short_merge_modifiers_rejects_a_doubled_leading_comma() {
        // MEASURED against rsync 3.5.0: `--filter=':,,f'` exits 1 with
        // `invalid modifier ',' at position 2`. The previous split swallowed the
        // whole run and produced a dir-merge of "f".
        let invalid = split_short_merge_modifiers(",,f").expect_err("`,,` is not a modifier run");
        assert_eq!(invalid.ch, ',');
        assert_eq!(invalid.offset + ':'.len_utf8(), 2);
    }

    #[test]
    fn invalid_modifier_message_matches_upstream_text() {
        // upstream: exclude.c:1376-1378 renders
        // `invalid modifier '%c' at position %d in filter rule: %s`.
        let invalid =
            split_short_merge_modifiers("n.rsync-filter").expect_err("`.` is not a modifier");
        let rendered = invalid.into_message(1, ":n.rsync-filter").to_string();
        assert!(
            rendered.contains("invalid modifier '.' at position 2 in filter rule: :n.rsync-filter"),
            "rendered message diverges from upstream: {rendered}"
        );
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
