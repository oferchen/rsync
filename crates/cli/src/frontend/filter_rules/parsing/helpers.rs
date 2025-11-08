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
