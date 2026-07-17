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

pub(super) fn split_short_rule_modifiers(text: &str) -> (&str, &str) {
    if text.is_empty() {
        return ("", "");
    }

    if let Some(rest) = text.strip_prefix(',') {
        let (modifiers, remainder) = split_short_rule_modifiers(rest);
        if modifiers.is_empty() {
            return ("", remainder);
        }
        return (modifiers, remainder);
    }

    if matches!(text.chars().next(), Some(ch) if ch.is_ascii_whitespace() || ch == '_') {
        return ("", consume_rule_separator(text));
    }

    for (idx, ch) in text.char_indices() {
        if ch == ',' || ch == '_' || ch.is_ascii_whitespace() {
            let modifiers = &text[..idx];
            let remainder = consume_rule_separator(&text[idx..]);
            return (modifiers, remainder);
        }
    }

    // upstream: exclude.c:1214-1287 - after a `+`/`-` prefix, every byte up to the
    // first ` `/`_` separator is a modifier. With no separator at all the whole
    // remainder is modifiers (and the pattern is empty), so `+foo`/`+S` are
    // rejected as invalid modifiers rather than silently treated as a pattern.
    (text, "")
}

pub(super) fn split_short_merge_modifiers(text: &str, allow_extended: bool) -> (&str, &str) {
    if text.is_empty() {
        return ("", "");
    }

    if let Some(rest) = text.strip_prefix(',') {
        let (modifiers, remainder) = split_short_merge_modifiers(rest, allow_extended);
        if modifiers.is_empty() {
            return ("", remainder);
        }
        return (modifiers, remainder);
    }

    if matches!(text.chars().next(), Some(ch) if ch.is_ascii_whitespace() || ch == '_') {
        return ("", consume_rule_separator(text));
    }

    let mut end = 0usize;
    for (idx, ch) in text.char_indices() {
        if ch == ',' || ch == '_' || ch.is_ascii_whitespace() {
            let modifiers = &text[..end];
            let remainder = consume_rule_separator(&text[idx..]);
            return (modifiers, remainder);
        }

        let lower = ch.to_ascii_lowercase();
        let base_modifier = matches!(lower, '+' | '-' | 'c' | 'w' | 's' | 'r' | 'p' | '/');
        let extended_modifier = matches!(lower, 'e' | 'n');

        if base_modifier || (allow_extended && extended_modifier) {
            end = idx + ch.len_utf8();
            continue;
        }

        let modifiers = &text[..end];
        let remainder = consume_rule_separator(&text[idx..]);
        return (modifiers, remainder);
    }

    (&text[..end], "")
}

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

    #[test]
    fn split_short_rule_modifiers_empty() {
        assert_eq!(split_short_rule_modifiers(""), ("", ""));
    }

    #[test]
    fn split_short_rule_modifiers_no_separator_is_all_modifiers() {
        // upstream: exclude.c:1214-1287 - with no ` `/`_` separator after the
        // prefix, every byte is a modifier and the pattern is empty. The caller
        // then rejects the unknown modifier bytes (e.g. `+foo` -> invalid 'f').
        assert_eq!(split_short_rule_modifiers("pattern"), ("pattern", ""));
    }

    #[test]
    fn split_short_rule_modifiers_whitespace_start() {
        assert_eq!(split_short_rule_modifiers(" pattern"), ("", "pattern"));
    }

    #[test]
    fn split_short_rule_modifiers_underscore_start() {
        assert_eq!(split_short_rule_modifiers("_pattern"), ("", "pattern"));
    }

    #[test]
    fn split_short_rule_modifiers_comma_separated() {
        let (mods, rem) = split_short_rule_modifiers("sr,pattern");
        assert_eq!(mods, "sr");
        assert_eq!(rem, "pattern");
    }

    #[test]
    fn split_short_rule_modifiers_space_separated() {
        let (mods, rem) = split_short_rule_modifiers("sr pattern");
        assert_eq!(mods, "sr");
        assert_eq!(rem, "pattern");
    }

    #[test]
    fn split_short_merge_modifiers_empty() {
        assert_eq!(split_short_merge_modifiers("", false), ("", ""));
        assert_eq!(split_short_merge_modifiers("", true), ("", ""));
    }

    #[test]
    fn split_short_merge_modifiers_base() {
        let (mods, rem) = split_short_merge_modifiers("+-cs pattern", false);
        assert_eq!(mods, "+-cs");
        assert_eq!(rem, "pattern");
    }

    #[test]
    fn split_short_merge_modifiers_extended_disabled() {
        // 'e' and 'n' not recognized without extended
        let (mods, rem) = split_short_merge_modifiers("ce pattern", false);
        assert_eq!(mods, "c");
        assert_eq!(rem, "e pattern");
    }

    #[test]
    fn split_short_merge_modifiers_extended_enabled() {
        let (mods, rem) = split_short_merge_modifiers("cen pattern", true);
        assert_eq!(mods, "cen");
        assert_eq!(rem, "pattern");
    }

    #[test]
    fn split_short_merge_modifiers_whitespace_start() {
        assert_eq!(
            split_short_merge_modifiers(" pattern", false),
            ("", "pattern")
        );
    }

    #[test]
    fn split_short_merge_modifiers_comma_prefix() {
        // 'p' is a valid modifier, so it's extracted
        let (mods, rem) = split_short_merge_modifiers(",pattern", false);
        assert_eq!(mods, "p");
        assert_eq!(rem, "attern");
    }

    #[test]
    fn split_short_merge_modifiers_comma_only_non_modifier() {
        // 'x' is not a valid modifier
        let (mods, rem) = split_short_merge_modifiers(",xyz", false);
        assert_eq!(mods, "");
        assert_eq!(rem, "xyz");
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
