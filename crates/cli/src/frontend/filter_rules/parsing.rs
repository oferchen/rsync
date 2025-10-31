use std::ffi::{OsStr, OsString};

use rsync_core::client::{DirMergeEnforcedKind, DirMergeOptions, FilterRuleKind, FilterRuleSpec};
use rsync_core::message::{Message, Role};
use rsync_core::rsync_error;

use super::directive::{FilterDirective, MergeDirective};

fn split_short_rule_modifiers(text: &str) -> (&str, &str) {
    if text.is_empty() {
        return ("", "");
    }

    if let Some(rest) = text.strip_prefix(',') {
        let mut parts = rest.splitn(2, |ch: char| ch.is_ascii_whitespace() || ch == '_');
        let modifiers = parts.next().unwrap_or("");
        let remainder = parts.next().unwrap_or("");
        let remainder =
            remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        return (modifiers, remainder);
    }

    let mut chars = text.chars();
    match chars.next() {
        None => ("", ""),
        Some(first) if first.is_ascii_whitespace() || first == '_' => {
            let remainder =
                text.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
            ("", remainder)
        }
        Some(_) => {
            let mut len = 0;
            for ch in text.chars() {
                if ch.is_ascii_whitespace() || ch == '_' {
                    break;
                }
                len += ch.len_utf8();
            }
            let modifiers = &text[..len];
            let remainder =
                text[len..].trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
            (modifiers, remainder)
        }
    }
}

fn parse_short_merge_directive(text: &str) -> Option<Result<FilterDirective, Message>> {
    let mut chars = text.chars();
    let first = chars.next()?;
    let (allow_extended, label) = match first {
        '.' => (false, "merge"),
        ':' => (true, "dir-merge"),
        _ => return None,
    };

    let remainder = chars.as_str();
    let (modifiers, rest) = split_short_rule_modifiers(remainder);
    let (options, assume_cvsignore) = match parse_merge_modifiers(modifiers, text, allow_extended) {
        Ok(result) => result,
        Err(error) => return Some(Err(error)),
    };

    let pattern = rest.trim();
    let pattern = if pattern.is_empty() {
        if assume_cvsignore {
            ".cvsignore"
        } else if allow_extended {
            let message = rsync_error!(
                1,
                format!("filter rule '{text}' is missing a file name after '{label}'")
            )
            .with_role(Role::Client);
            return Some(Err(message));
        } else {
            let message = rsync_error!(
                1,
                format!("filter merge directive '{text}' is missing a file path")
            )
            .with_role(Role::Client);
            return Some(Err(message));
        }
    } else {
        pattern
    };

    if allow_extended {
        let rule = FilterRuleSpec::dir_merge(pattern.to_string(), options.clone());
        return Some(Ok(FilterDirective::Rule(rule)));
    }

    let enforced_kind = match options.enforced_kind() {
        Some(DirMergeEnforcedKind::Include) => Some(FilterRuleKind::Include),
        Some(DirMergeEnforcedKind::Exclude) => Some(FilterRuleKind::Exclude),
        None => None,
    };

    let directive =
        MergeDirective::new(OsString::from(pattern), enforced_kind).with_options(options);
    Some(Ok(FilterDirective::Merge(directive)))
}

fn parse_filter_shorthand(
    trimmed: &str,
    short: char,
    label: &str,
    builder: fn(String) -> FilterRuleSpec,
) -> Option<Result<FilterDirective, Message>> {
    let mut chars = trimmed.chars();
    let first = chars.next()?;
    if !first.eq_ignore_ascii_case(&short) {
        return None;
    }

    let remainder = chars.as_str();
    if remainder.is_empty() {
        let text = format!("filter rule '{trimmed}' is missing a pattern after '{label}'");
        let message = rsync_error!(1, text).with_role(Role::Client);
        return Some(Err(message));
    }

    if !remainder
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_whitespace() || ch == '_')
    {
        return None;
    }

    let pattern = remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    if pattern.is_empty() {
        let text = format!("filter rule '{trimmed}' is missing a pattern after '{label}'");
        let message = rsync_error!(1, text).with_role(Role::Client);
        return Some(Err(message));
    }

    Some(Ok(FilterDirective::Rule(builder(pattern.to_string()))))
}

pub(crate) fn parse_merge_modifiers(
    modifiers: &str,
    directive: &str,
    allow_extended: bool,
) -> Result<(DirMergeOptions, bool), Message> {
    let mut options = if allow_extended {
        DirMergeOptions::default()
    } else {
        DirMergeOptions::default().allow_list_clearing(true)
    };
    let mut enforced: Option<DirMergeEnforcedKind> = None;
    let mut saw_include = false;
    let mut saw_exclude = false;
    let mut assume_cvsignore = false;

    for modifier in modifiers.chars() {
        let lower = modifier.to_ascii_lowercase();
        match lower {
            '-' => {
                if saw_include {
                    let message = rsync_error!(
                        1,
                        format!("filter rule '{directive}' cannot combine '+' and '-' modifiers")
                    )
                    .with_role(Role::Client);
                    return Err(message);
                }
                saw_exclude = true;
                enforced = Some(DirMergeEnforcedKind::Exclude);
            }
            '+' => {
                if saw_exclude {
                    let message = rsync_error!(
                        1,
                        format!("filter rule '{directive}' cannot combine '+' and '-' modifiers")
                    )
                    .with_role(Role::Client);
                    return Err(message);
                }
                saw_include = true;
                enforced = Some(DirMergeEnforcedKind::Include);
            }
            'c' => {
                if saw_include {
                    let message = rsync_error!(
                        1,
                        format!(
                            "filter merge directive '{directive}' cannot combine 'C' with '+' or '-'"
                        )
                    )
                    .with_role(Role::Client);
                    return Err(message);
                }
                saw_exclude = true;
                enforced = Some(DirMergeEnforcedKind::Exclude);
                options = options
                    .use_whitespace()
                    .allow_comments(false)
                    .allow_list_clearing(true)
                    .inherit(false);
                assume_cvsignore = true;
            }
            'e' => {
                if allow_extended {
                    options = options.exclude_filter_file(true);
                } else {
                    let message = rsync_error!(
                        1,
                        format!(
                            "filter merge directive '{directive}' uses unsupported modifier '{}'",
                            modifier
                        )
                    )
                    .with_role(Role::Client);
                    return Err(message);
                }
            }
            'n' => {
                if allow_extended {
                    options = options.inherit(false);
                } else {
                    let message = rsync_error!(
                        1,
                        format!(
                            "filter merge directive '{directive}' uses unsupported modifier '{}'",
                            modifier
                        )
                    )
                    .with_role(Role::Client);
                    return Err(message);
                }
            }
            'w' => {
                options = options.use_whitespace().allow_comments(false);
            }
            's' => {
                options = options.sender_modifier();
            }
            'r' => {
                options = options.receiver_modifier();
            }
            '/' => {
                options = options.anchor_root(true);
            }
            _ => {
                let message = rsync_error!(
                    1,
                    format!(
                        "filter merge directive '{directive}' uses unsupported modifier '{}'",
                        modifier
                    )
                )
                .with_role(Role::Client);
                return Err(message);
            }
        }
    }

    options = options.with_enforced_kind(enforced);
    if !allow_extended && !options.list_clear_allowed() {
        options = options.allow_list_clearing(true);
    }
    Ok((options, assume_cvsignore))
}

pub(crate) fn parse_filter_directive(argument: &OsStr) -> Result<FilterDirective, Message> {
    let text = argument.to_string_lossy();
    let trimmed_leading = text.trim_start();

    if let Some(result) = parse_short_merge_directive(trimmed_leading) {
        return result;
    }

    if let Some(rest) = trimmed_leading.strip_prefix("merge") {
        let mut remainder =
            rest.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        let mut modifiers = "";
        if let Some(next) = remainder.strip_prefix(',') {
            let mut split = next.splitn(2, |ch: char| ch.is_ascii_whitespace() || ch == '_');
            modifiers = split.next().unwrap_or("");
            remainder = split
                .next()
                .unwrap_or("")
                .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        }
        let (options, assume_cvsignore) = parse_merge_modifiers(modifiers, trimmed_leading, false)?;

        let mut path_text = remainder.trim_end();
        if path_text.is_empty() {
            if assume_cvsignore {
                path_text = ".cvsignore";
            } else {
                let message = rsync_error!(
                    1,
                    format!("filter merge directive '{trimmed_leading}' is missing a file path")
                )
                .with_role(Role::Client);
                return Err(message);
            }
        }

        let enforced_kind = match options.enforced_kind() {
            Some(DirMergeEnforcedKind::Include) => Some(FilterRuleKind::Include),
            Some(DirMergeEnforcedKind::Exclude) => Some(FilterRuleKind::Exclude),
            None => None,
        };

        let directive =
            MergeDirective::new(OsString::from(path_text), enforced_kind).with_options(options);
        return Ok(FilterDirective::Merge(directive));
    }

    let trimmed = trimmed_leading.trim_end();

    if trimmed.is_empty() {
        let message = rsync_error!(
            1,
            "filter rule is empty: supply '+', '-', '!', or 'merge FILE'"
        )
        .with_role(Role::Client);
        return Err(message);
    }

    if let Some(remainder) = trimmed.strip_prefix('!') {
        if remainder.trim().is_empty() {
            return Ok(FilterDirective::Clear);
        }

        let message = rsync_error!(1, "'!' rule has trailing characters: {}", trimmed)
            .with_role(Role::Client);
        return Err(message);
    }

    if trimmed.eq_ignore_ascii_case("clear") {
        return Ok(FilterDirective::Clear);
    }

    const EXCLUDE_IF_PRESENT_PREFIX: &str = "exclude-if-present";

    if let Some(result) = parse_filter_shorthand(trimmed, 'P', "P", FilterRuleSpec::protect) {
        return result;
    }

    if let Some(result) = parse_filter_shorthand(trimmed, 'H', "H", FilterRuleSpec::hide) {
        return result;
    }

    if let Some(result) = parse_filter_shorthand(trimmed, 'S', "S", FilterRuleSpec::show) {
        return result;
    }

    if let Some(result) = parse_filter_shorthand(trimmed, 'R', "R", FilterRuleSpec::risk) {
        return result;
    }

    if trimmed.len() >= EXCLUDE_IF_PRESENT_PREFIX.len()
        && trimmed[..EXCLUDE_IF_PRESENT_PREFIX.len()]
            .eq_ignore_ascii_case(EXCLUDE_IF_PRESENT_PREFIX)
    {
        let mut remainder = trimmed[EXCLUDE_IF_PRESENT_PREFIX.len()..]
            .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        if let Some(rest) = remainder.strip_prefix('=') {
            remainder = rest.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        }

        let pattern_text = remainder.trim();
        if pattern_text.is_empty() {
            let message = rsync_error!(
                1,
                format!(
                    "filter rule '{trimmed}' is missing a marker file after 'exclude-if-present'"
                )
            )
            .with_role(Role::Client);
            return Err(message);
        }

        return Ok(FilterDirective::Rule(FilterRuleSpec::exclude_if_present(
            pattern_text.to_string(),
        )));
    }

    if let Some(remainder) = trimmed.strip_prefix('+') {
        let pattern =
            remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        if pattern.is_empty() {
            let message = rsync_error!(
                1,
                "filter rule '{}' is missing a pattern after '+'",
                trimmed
            )
            .with_role(Role::Client);
            return Err(message);
        }
        return Ok(FilterDirective::Rule(FilterRuleSpec::include(
            pattern.to_string(),
        )));
    }

    if let Some(remainder) = trimmed.strip_prefix('-') {
        let pattern =
            remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        if pattern.is_empty() {
            let message = rsync_error!(
                1,
                "filter rule '{}' is missing a pattern after '-'",
                trimmed
            )
            .with_role(Role::Client);
            return Err(message);
        }
        return Ok(FilterDirective::Rule(FilterRuleSpec::exclude(
            pattern.to_string(),
        )));
    }

    const DIR_MERGE_PREFIX: &str = "dir-merge";

    if trimmed.len() >= DIR_MERGE_PREFIX.len()
        && trimmed[..DIR_MERGE_PREFIX.len()].eq_ignore_ascii_case(DIR_MERGE_PREFIX)
    {
        let mut remainder = trimmed[DIR_MERGE_PREFIX.len()..]
            .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        let mut modifiers = "";
        if let Some(rest) = remainder.strip_prefix(',') {
            let mut split = rest.splitn(2, |ch: char| ch.is_ascii_whitespace() || ch == '_');
            modifiers = split.next().unwrap_or("");
            remainder = split
                .next()
                .unwrap_or("")
                .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        }

        let (options, assume_cvsignore) = parse_merge_modifiers(modifiers, trimmed, true)?;

        let mut path_text = remainder.trim_end();
        if path_text.is_empty() {
            if assume_cvsignore {
                path_text = ".cvsignore";
            } else {
                let text =
                    format!("filter rule '{trimmed}' is missing a file name after 'dir-merge'");
                return Err(rsync_error!(1, text).with_role(Role::Client));
            }
        }

        return Ok(FilterDirective::Rule(FilterRuleSpec::dir_merge(
            path_text.to_string(),
            options,
        )));
    }

    let mut parts = trimmed.splitn(2, |ch: char| ch.is_ascii_whitespace());
    let keyword = parts.next().expect("split always yields at least one part");
    let remainder = parts.next().unwrap_or("");
    let pattern = remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());

    let handle_keyword = |action_label: &str, builder: fn(String) -> FilterRuleSpec| {
        if pattern.is_empty() {
            let text =
                format!("filter rule '{trimmed}' is missing a pattern after '{action_label}'");
            let message = rsync_error!(1, text).with_role(Role::Client);
            return Err(message);
        }

        Ok(FilterDirective::Rule(builder(pattern.to_string())))
    };

    if keyword.eq_ignore_ascii_case("include") {
        return handle_keyword("include", FilterRuleSpec::include);
    }

    if keyword.eq_ignore_ascii_case("exclude") {
        return handle_keyword("exclude", FilterRuleSpec::exclude);
    }

    if keyword.eq_ignore_ascii_case("show") {
        return handle_keyword("show", FilterRuleSpec::show);
    }

    if keyword.eq_ignore_ascii_case("hide") {
        return handle_keyword("hide", FilterRuleSpec::hide);
    }

    if keyword.eq_ignore_ascii_case("protect") {
        return handle_keyword("protect", FilterRuleSpec::protect);
    }

    if keyword.eq_ignore_ascii_case("risk") {
        return handle_keyword("risk", FilterRuleSpec::risk);
    }

    let message = rsync_error!(
        1,
        "unsupported filter rule '{}': this build currently supports only '+' (include), '-' (exclude), '!' (clear), 'include PATTERN', 'exclude PATTERN', 'show PATTERN', 'hide PATTERN', 'protect PATTERN', 'risk PATTERN', 'merge[,MODS] FILE' or '.[,MODS] FILE', and 'dir-merge[,MODS] FILE' or ':[,MODS] FILE' directives",
        trimmed
    )
    .with_role(Role::Client);
    Err(message)
}
