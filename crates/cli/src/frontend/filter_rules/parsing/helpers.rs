pub(super) fn trim_short_rule_remainder(remainder: &str) -> &str {
    let remainder = remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    if let Some(rest) = remainder.strip_prefix(',') {
        return rest.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    }
    remainder
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
        return ("", trim_short_rule_remainder(text));
    }

    for (idx, ch) in text.char_indices() {
        if ch == ',' || ch == '_' || ch.is_ascii_whitespace() {
            let modifiers = &text[..idx];
            let remainder = trim_short_rule_remainder(&text[idx..]);
            return (modifiers, remainder);
        }
    }

    ("", text)
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
        return ("", trim_short_rule_remainder(text));
    }

    let mut end = 0usize;
    for (idx, ch) in text.char_indices() {
        if ch == ',' || ch == '_' || ch.is_ascii_whitespace() {
            let modifiers = &text[..end];
            let remainder = trim_short_rule_remainder(&text[idx..]);
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
        let remainder = trim_short_rule_remainder(&text[idx..]);
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
    fn trim_short_rule_remainder_basic() {
        assert_eq!(trim_short_rule_remainder("pattern"), "pattern");
    }

    #[test]
    fn trim_short_rule_remainder_leading_whitespace() {
        assert_eq!(trim_short_rule_remainder("  pattern"), "pattern");
        assert_eq!(trim_short_rule_remainder("\t\tpattern"), "pattern");
    }

    #[test]
    fn trim_short_rule_remainder_leading_underscore() {
        assert_eq!(trim_short_rule_remainder("__pattern"), "pattern");
        assert_eq!(trim_short_rule_remainder("_ pattern"), "pattern");
    }

    #[test]
    fn trim_short_rule_remainder_comma_prefix() {
        assert_eq!(trim_short_rule_remainder(",pattern"), "pattern");
        assert_eq!(trim_short_rule_remainder(", pattern"), "pattern");
        assert_eq!(trim_short_rule_remainder(",_pattern"), "pattern");
    }

    #[test]
    fn split_short_rule_modifiers_empty() {
        assert_eq!(split_short_rule_modifiers(""), ("", ""));
    }

    #[test]
    fn split_short_rule_modifiers_no_modifiers() {
        assert_eq!(split_short_rule_modifiers("pattern"), ("", "pattern"));
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
