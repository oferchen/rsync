//! Utilities for parsing and loading filter rules supplied via the CLI.

use std::collections::{HashSet, VecDeque};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, ErrorKind, Read};
use std::path::{Path, PathBuf};

use rsync_core::client::{DirMergeEnforcedKind, DirMergeOptions, FilterRuleKind, FilterRuleSpec};
use rsync_core::message::{Message, Role};
use rsync_core::rsync_error;

use super::defaults::CVS_EXCLUDE_PATTERNS;

pub(crate) fn os_string_to_pattern(value: OsString) -> String {
    match value.into_string() {
        Ok(text) => text,
        Err(value) => value.to_string_lossy().into_owned(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FilterDirective {
    Rule(FilterRuleSpec),
    Merge(MergeDirective),
    Clear,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MergeDirective {
    source: OsString,
    options: DirMergeOptions,
}

impl MergeDirective {
    pub(crate) fn new(source: OsString, enforced_kind: Option<FilterRuleKind>) -> Self {
        let mut options = DirMergeOptions::default();
        options = match enforced_kind {
            Some(FilterRuleKind::Include) => {
                options.with_enforced_kind(Some(DirMergeEnforcedKind::Include))
            }
            Some(FilterRuleKind::Exclude) => {
                options.with_enforced_kind(Some(DirMergeEnforcedKind::Exclude))
            }
            _ => options,
        };

        Self { source, options }
    }

    pub(crate) fn with_options(mut self, options: DirMergeOptions) -> Self {
        self.options = options;
        self
    }

    pub(crate) fn source(&self) -> &OsStr {
        self.source.as_os_str()
    }

    pub(crate) fn options(&self) -> &DirMergeOptions {
        &self.options
    }
}

pub(crate) fn merge_directive_options(
    base: &DirMergeOptions,
    directive: &MergeDirective,
) -> DirMergeOptions {
    let defaults = DirMergeOptions::default();
    let current = directive.options();

    let inherit = if current.inherit_rules() != defaults.inherit_rules() {
        current.inherit_rules()
    } else {
        base.inherit_rules()
    };

    let exclude_self = if current.excludes_self() != defaults.excludes_self() {
        current.excludes_self()
    } else {
        base.excludes_self()
    };

    let allow_list_clear = if current.list_clear_allowed() != defaults.list_clear_allowed() {
        current.list_clear_allowed()
    } else {
        base.list_clear_allowed()
    };

    let uses_whitespace = if current.uses_whitespace() != defaults.uses_whitespace() {
        current.uses_whitespace()
    } else {
        base.uses_whitespace()
    };

    let allows_comments = if current.allows_comments() != defaults.allows_comments() {
        current.allows_comments()
    } else {
        base.allows_comments()
    };

    let enforced_kind = if current.enforced_kind() != defaults.enforced_kind() {
        current.enforced_kind()
    } else {
        base.enforced_kind()
    };

    let sender_override = current
        .sender_side_override()
        .or_else(|| base.sender_side_override());
    let receiver_override = current
        .receiver_side_override()
        .or_else(|| base.receiver_side_override());

    let anchor_root = if current.anchor_root_enabled() != defaults.anchor_root_enabled() {
        current.anchor_root_enabled()
    } else {
        base.anchor_root_enabled()
    };

    let mut merged = DirMergeOptions::default()
        .inherit(inherit)
        .exclude_filter_file(exclude_self)
        .allow_list_clearing(allow_list_clear)
        .anchor_root(anchor_root)
        .with_side_overrides(sender_override, receiver_override)
        .with_enforced_kind(enforced_kind);

    if uses_whitespace {
        merged = merged.use_whitespace();
    }

    if !allows_comments {
        merged = merged.allow_comments(false);
    }

    merged
}

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

pub(crate) fn append_filter_rules_from_files(
    destination: &mut Vec<FilterRuleSpec>,
    files: &[OsString],
    kind: FilterRuleKind,
) -> Result<(), Message> {
    if matches!(kind, FilterRuleKind::DirMerge) {
        let message = rsync_error!(
            1,
            "dir-merge directives cannot be loaded via --include-from/--exclude-from in this build"
        )
        .with_role(Role::Client);
        return Err(message);
    }

    for path in files {
        let patterns = load_filter_file_patterns(Path::new(path.as_os_str()))?;
        destination.extend(patterns.into_iter().map(|pattern| match kind {
            FilterRuleKind::Include => FilterRuleSpec::include(pattern),
            FilterRuleKind::Exclude => FilterRuleSpec::exclude(pattern),
            FilterRuleKind::Clear => FilterRuleSpec::clear(),
            FilterRuleKind::ExcludeIfPresent => FilterRuleSpec::exclude_if_present(pattern),
            FilterRuleKind::Protect => FilterRuleSpec::protect(pattern),
            FilterRuleKind::Risk => FilterRuleSpec::risk(pattern),
            FilterRuleKind::DirMerge => unreachable!("dir-merge handled above"),
        }));
    }
    Ok(())
}

pub(crate) fn locate_filter_arguments(args: &[OsString]) -> (Vec<usize>, Vec<usize>) {
    let mut filter_indices = Vec::new();
    let mut rsync_filter_indices = Vec::new();
    let mut after_double_dash = false;
    let mut expect_filter_value = false;

    for (index, arg) in args.iter().enumerate().skip(1) {
        if after_double_dash {
            continue;
        }

        if expect_filter_value {
            expect_filter_value = false;
            continue;
        }

        if arg == "--" {
            after_double_dash = true;
            continue;
        }

        if arg == "--filter" {
            filter_indices.push(index);
            expect_filter_value = true;
            continue;
        }

        let value = arg.to_string_lossy();

        if value.starts_with("--filter=") {
            filter_indices.push(index);
            continue;
        }

        if value.starts_with('-') && !value.starts_with("--") && value.len() > 1 {
            for ch in value[1..].chars() {
                if ch == 'F' {
                    rsync_filter_indices.push(index);
                }
            }
        }
    }

    (filter_indices, rsync_filter_indices)
}

pub(crate) fn collect_filter_arguments(
    filters: &[OsString],
    filter_indices: &[usize],
    rsync_filter_indices: &[usize],
) -> Vec<OsString> {
    if rsync_filter_indices.is_empty() {
        return filters.to_vec();
    }

    let mut raw_queue: VecDeque<(usize, &OsString)> =
        filter_indices.iter().copied().zip(filters.iter()).collect();
    let mut alias_queue: VecDeque<(usize, usize)> = rsync_filter_indices
        .iter()
        .copied()
        .enumerate()
        .map(|(occurrence, position)| (position, occurrence))
        .collect();
    let mut merged = Vec::with_capacity(raw_queue.len() + alias_queue.len() * 2);

    while !raw_queue.is_empty() || !alias_queue.is_empty() {
        match (raw_queue.front(), alias_queue.front()) {
            (Some((raw_index, _)), Some((alias_index, _))) => {
                if alias_index <= raw_index {
                    let (_, occurrence) = alias_queue.pop_front().unwrap();
                    push_rsync_filter_shortcut(&mut merged, occurrence);
                } else {
                    let (_, value) = raw_queue.pop_front().unwrap();
                    merged.push(value.clone());
                }
            }
            (Some(_), None) => {
                let (_, value) = raw_queue.pop_front().unwrap();
                merged.push(value.clone());
            }
            (None, Some(_)) => {
                let (_, occurrence) = alias_queue.pop_front().unwrap();
                push_rsync_filter_shortcut(&mut merged, occurrence);
            }
            (None, None) => break,
        }
    }

    merged
}

fn push_rsync_filter_shortcut(target: &mut Vec<OsString>, occurrence: usize) {
    if occurrence == 0 {
        target.push(OsString::from("dir-merge /.rsync-filter"));
        target.push(OsString::from("exclude .rsync-filter"));
    } else {
        target.push(OsString::from("dir-merge .rsync-filter"));
    }
}

pub(crate) fn append_cvs_exclude_rules(
    destination: &mut Vec<FilterRuleSpec>,
) -> Result<(), Message> {
    let mut cvs_rules: Vec<FilterRuleSpec> = CVS_EXCLUDE_PATTERNS
        .iter()
        .map(|pattern| FilterRuleSpec::exclude((*pattern).to_string()))
        .collect();

    if let Some(home) = env::var_os("HOME").filter(|value| !value.is_empty()) {
        let path = Path::new(&home).join(".cvsignore");
        match fs::read(&path) {
            Ok(contents) => {
                let owned = String::from_utf8_lossy(&contents).into_owned();
                append_cvsignore_tokens(&mut cvs_rules, owned.split_whitespace());
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                let text = format!(
                    "failed to read '{}' for --cvs-exclude: {error}",
                    path.display()
                );
                return Err(rsync_error!(1, text).with_role(Role::Client));
            }
        }
    }

    if let Some(value) = env::var_os("CVSIGNORE").filter(|value| !value.is_empty()) {
        let owned = value.to_string_lossy().into_owned();
        append_cvsignore_tokens(&mut cvs_rules, owned.split_whitespace());
    }

    let options = DirMergeOptions::default()
        .with_enforced_kind(Some(DirMergeEnforcedKind::Exclude))
        .use_whitespace()
        .allow_comments(false)
        .inherit(false)
        .allow_list_clearing(true);
    cvs_rules.push(FilterRuleSpec::dir_merge(".cvsignore".to_string(), options));

    destination.extend(cvs_rules);
    Ok(())
}

fn append_cvsignore_tokens<'a, I>(destination: &mut Vec<FilterRuleSpec>, tokens: I)
where
    I: IntoIterator<Item = &'a str>,
{
    for token in tokens {
        let trimmed = token.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if trimmed == "!" {
            destination.clear();
            continue;
        }

        if let Some(remainder) = trimmed.strip_prefix('!') {
            if remainder.is_empty() {
                continue;
            }
            remove_cvs_pattern(destination, remainder);
            continue;
        }

        destination.push(FilterRuleSpec::exclude(trimmed.to_string()));
    }
}

fn remove_cvs_pattern(rules: &mut Vec<FilterRuleSpec>, pattern: &str) {
    rules.retain(|rule| {
        !(matches!(rule.kind(), FilterRuleKind::Exclude) && rule.pattern() == pattern)
    });
}

pub(crate) fn load_filter_file_patterns(path: &Path) -> Result<Vec<String>, Message> {
    if path == Path::new("-") {
        return read_filter_patterns_from_standard_input();
    }

    let path_display = path.display().to_string();
    let file = File::open(path).map_err(|error| {
        let text = format!("failed to read filter file '{}': {}", path_display, error);
        rsync_error!(1, text).with_role(Role::Client)
    })?;

    let mut reader = BufReader::new(file);
    read_filter_patterns(&mut reader).map_err(|error| {
        let text = format!("failed to read filter file '{}': {}", path_display, error);
        rsync_error!(1, text).with_role(Role::Client)
    })
}

pub(crate) fn apply_merge_directive(
    directive: MergeDirective,
    base_dir: &Path,
    destination: &mut Vec<FilterRuleSpec>,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), Message> {
    let options = directive.options().clone();
    let original_source_text = os_string_to_pattern(directive.source().to_os_string());
    let is_stdin = directive.source() == OsStr::new("-");
    let (resolved_path, display, canonical_path) = if is_stdin {
        (PathBuf::from("-"), String::from("-"), None)
    } else {
        let raw_path = PathBuf::from(directive.source());
        let resolved = if raw_path.is_absolute() {
            raw_path
        } else {
            base_dir.join(raw_path)
        };
        let display = resolved.display().to_string();
        let canonical = fs::canonicalize(&resolved).ok();
        (resolved, display, canonical)
    };

    let guard_key = if is_stdin {
        PathBuf::from("-")
    } else if let Some(canonical) = &canonical_path {
        canonical.clone()
    } else {
        resolved_path.clone()
    };

    if !visited.insert(guard_key.clone()) {
        let text = format!("recursive filter merge detected for '{display}'");
        return Err(rsync_error!(1, text).with_role(Role::Client));
    }

    let next_base_storage = if is_stdin {
        None
    } else {
        let resolved_for_base = canonical_path.as_ref().unwrap_or(&resolved_path);
        Some(
            resolved_for_base
                .parent()
                .map(|parent| parent.to_path_buf())
                .unwrap_or_else(|| base_dir.to_path_buf()),
        )
    };
    let next_base = next_base_storage.as_deref().unwrap_or(base_dir);
    let result = (|| -> Result<(), Message> {
        let contents = if is_stdin {
            read_merge_from_standard_input()?
        } else {
            read_merge_file(&resolved_path)?
        };

        parse_merge_contents(
            &contents,
            &options,
            next_base,
            &display,
            destination,
            visited,
        )
    })();
    visited.remove(&guard_key);
    if result.is_ok() && options.excludes_self() && !is_stdin {
        let mut rule = FilterRuleSpec::exclude(original_source_text);
        rule.apply_dir_merge_overrides(&options);
        destination.push(rule);
    }
    result
}

fn read_merge_file(path: &Path) -> Result<String, Message> {
    fs::read_to_string(path).map_err(|error| {
        let text = format!("failed to read filter file '{}': {}", path.display(), error);
        rsync_error!(1, text).with_role(Role::Client)
    })
}

fn read_merge_from_standard_input() -> Result<String, Message> {
    #[cfg(test)]
    if let Some(data) = take_filter_stdin_input() {
        return String::from_utf8(data).map_err(|error| {
            let text = format!(
                "failed to read filter patterns from standard input: {}",
                error
            );
            rsync_error!(1, text).with_role(Role::Client)
        });
    }

    let mut buffer = String::new();
    io::stdin().read_to_string(&mut buffer).map_err(|error| {
        let text = format!(
            "failed to read filter patterns from standard input: {}",
            error
        );
        rsync_error!(1, text).with_role(Role::Client)
    })?;
    Ok(buffer)
}

fn parse_merge_contents(
    contents: &str,
    options: &DirMergeOptions,
    base_dir: &Path,
    display: &str,
    destination: &mut Vec<FilterRuleSpec>,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), Message> {
    if options.uses_whitespace() {
        let mut tokens = contents.split_whitespace();
        while let Some(token) = tokens.next() {
            if token.is_empty() {
                continue;
            }

            if token == "!" {
                if options.list_clear_allowed() {
                    destination.clear();
                    continue;
                }
                let message = rsync_error!(
                    1,
                    format!("list-clearing '!' is not permitted in merge file '{display}'")
                )
                .with_role(Role::Client);
                return Err(message);
            }

            if let Some(kind) = options.enforced_kind() {
                let mut rule = match kind {
                    DirMergeEnforcedKind::Include => FilterRuleSpec::include(token.to_string()),
                    DirMergeEnforcedKind::Exclude => FilterRuleSpec::exclude(token.to_string()),
                };
                rule.apply_dir_merge_overrides(options);
                destination.push(rule);
                continue;
            }

            let lower = token.to_ascii_lowercase();
            let directive = if merge_directive_requires_argument(&lower) {
                let Some(arg) = tokens.next() else {
                    let message = rsync_error!(
                        1,
                        format!(
                            "filter merge directive '{}' in '{}' is missing a pattern",
                            token, display
                        )
                    )
                    .with_role(Role::Client);
                    return Err(message);
                };
                format!("{token} {arg}")
            } else {
                token.to_string()
            };

            process_merge_directive(&directive, options, base_dir, display, destination, visited)?;
        }
        return Ok(());
    }

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if options.allows_comments() && trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with(';') && options.allows_comments() {
            continue;
        }

        if trimmed == "!" {
            if options.list_clear_allowed() {
                destination.clear();
                continue;
            }
            let message = rsync_error!(
                1,
                format!("list-clearing '!' is not permitted in merge file '{display}'")
            )
            .with_role(Role::Client);
            return Err(message);
        }

        if let Some(kind) = options.enforced_kind() {
            let mut rule = match kind {
                DirMergeEnforcedKind::Include => FilterRuleSpec::include(trimmed.to_string()),
                DirMergeEnforcedKind::Exclude => FilterRuleSpec::exclude(trimmed.to_string()),
            };
            rule.apply_dir_merge_overrides(options);
            destination.push(rule);
            continue;
        }

        process_merge_directive(trimmed, options, base_dir, display, destination, visited)?;
    }

    Ok(())
}

pub(crate) fn process_merge_directive(
    directive: &str,
    options: &DirMergeOptions,
    base_dir: &Path,
    display: &str,
    destination: &mut Vec<FilterRuleSpec>,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), Message> {
    match parse_filter_directive(OsStr::new(directive)) {
        Ok(FilterDirective::Rule(mut rule)) => {
            rule.apply_dir_merge_overrides(options);
            destination.push(rule);
        }
        Ok(FilterDirective::Merge(nested)) => {
            let effective_options = merge_directive_options(options, &nested);
            let nested = nested.with_options(effective_options);
            apply_merge_directive(nested, base_dir, destination, visited).map_err(|error| {
                let detail = error.to_string();
                rsync_error!(
                    1,
                    format!("failed to process merge file '{display}': {detail}")
                )
                .with_role(Role::Client)
            })?;
        }
        Ok(FilterDirective::Clear) => destination.clear(),
        Err(error) => {
            let detail = error.to_string();
            let message = rsync_error!(
                1,
                format!(
                    "failed to parse filter rule '{}' from merge file '{}': {}",
                    directive, display, detail
                )
            )
            .with_role(Role::Client);
            return Err(message);
        }
    }

    Ok(())
}

fn merge_directive_requires_argument(keyword: &str) -> bool {
    matches!(
        keyword,
        "merge" | "include" | "exclude" | "show" | "hide" | "protect"
    ) || keyword.starts_with("dir-merge")
}

fn read_filter_patterns_from_standard_input() -> Result<Vec<String>, Message> {
    #[cfg(test)]
    if let Some(data) = take_filter_stdin_input() {
        let mut cursor = io::Cursor::new(data);
        return read_filter_patterns(&mut cursor).map_err(|error| {
            let text = format!(
                "failed to read filter patterns from standard input: {}",
                error
            );
            rsync_error!(1, text).with_role(Role::Client)
        });
    }

    let stdin = io::stdin();
    let mut reader = stdin.lock();
    read_filter_patterns(&mut reader).map_err(|error| {
        let text = format!(
            "failed to read filter patterns from standard input: {}",
            error
        );
        rsync_error!(1, text).with_role(Role::Client)
    })
}

fn read_filter_patterns<R: BufRead>(reader: &mut R) -> io::Result<Vec<String>> {
    let mut buffer = Vec::new();
    let mut patterns = Vec::new();

    loop {
        buffer.clear();
        let bytes_read = reader.read_until(b'\n', &mut buffer)?;

        if bytes_read == 0 {
            break;
        }

        if buffer.last() == Some(&b'\n') {
            buffer.pop();
        }
        if buffer.last() == Some(&b'\r') {
            buffer.pop();
        }

        let line = String::from_utf8_lossy(&buffer);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }

        patterns.push(line.into_owned());
    }

    Ok(patterns)
}

#[cfg(test)]
thread_local! {
    static FILTER_STDIN_INPUT: std::cell::RefCell<Option<Vec<u8>>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(test)]
fn take_filter_stdin_input() -> Option<Vec<u8>> {
    FILTER_STDIN_INPUT.with(|slot| slot.borrow_mut().take())
}

#[cfg(test)]
pub(crate) fn set_filter_stdin_input(data: Vec<u8>) {
    FILTER_STDIN_INPUT.with(|slot| *slot.borrow_mut() = Some(data));
}
