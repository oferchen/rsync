use super::parse::{FilterParseError, ParsedFilterDirective, parse_filter_directive_line};
use crate::local_copy::LocalCopyError;
use crate::local_copy::filter_program::{
    DirMergeEnforcedKind, DirMergeOptions, DirMergeParser, ExcludeIfPresentRule, FilterProgramError,
};
use filters::FilterRule;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub(crate) fn filter_program_local_error(path: &Path, error: FilterProgramError) -> LocalCopyError {
    LocalCopyError::io(
        "compile filter file",
        path.to_path_buf(),
        io::Error::new(io::ErrorKind::InvalidData, error.to_string()),
    )
}

pub(crate) fn resolve_dir_merge_path(base: &Path, pattern: &Path) -> PathBuf {
    if pattern.is_absolute()
        && let Ok(stripped) = pattern.strip_prefix(Path::new("/"))
    {
        return base.join(stripped);
    }

    base.join(pattern)
}

pub(crate) fn apply_dir_merge_rule_defaults(
    mut rule: FilterRule,
    options: &DirMergeOptions,
) -> FilterRule {
    if options.anchor_root_enabled() {
        rule = rule.anchor_to_root();
    }

    if options.perishable() {
        rule = rule.with_perishable(true);
    }

    if let Some(sender) = options.sender_side_override() {
        rule = rule.with_sender(sender);
    }

    if let Some(receiver) = options.receiver_side_override() {
        rule = rule.with_receiver(receiver);
    }

    rule
}

#[derive(Default)]
pub(crate) struct DirMergeEntries {
    pub(crate) rules: Vec<FilterRule>,
    pub(crate) exclude_if_present: Vec<ExcludeIfPresentRule>,
}

impl DirMergeEntries {
    fn push_rule(&mut self, rule: FilterRule) {
        self.rules.push(rule);
    }

    fn push_exclude_if_present(&mut self, rule: ExcludeIfPresentRule) {
        self.exclude_if_present.push(rule);
    }

    fn extend(&mut self, mut other: DirMergeEntries) {
        self.rules.append(&mut other.rules);
        self.exclude_if_present
            .append(&mut other.exclude_if_present);
    }
}

pub(crate) fn load_dir_merge_rules_recursive(
    path: &Path,
    options: &DirMergeOptions,
    visited: &mut Vec<PathBuf>,
) -> Result<DirMergeEntries, LocalCopyError> {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if visited.contains(&canonical) {
        let path_display = path.display();
        let message = format!("recursive filter merge detected for {path_display}");
        return Err(LocalCopyError::io(
            "parse filter file",
            path.to_path_buf(),
            io::Error::new(io::ErrorKind::InvalidData, message),
        ));
    }

    visited.push(canonical);

    let file = fs::File::open(path)
        .map_err(|error| LocalCopyError::io("read filter file", path.to_path_buf(), error))?;
    let mut entries = DirMergeEntries::default();

    let map_error = |error: FilterParseError| {
        LocalCopyError::io(
            "parse filter file",
            path.to_path_buf(),
            io::Error::new(io::ErrorKind::InvalidData, error.to_string()),
        )
    };

    let mut contents = String::new();
    io::BufReader::new(file)
        .read_to_string(&mut contents)
        .map_err(|error| LocalCopyError::io("read filter file", path.to_path_buf(), error))?;

    match options.parser() {
        DirMergeParser::Whitespace { enforce_kind } => {
            let enforce_kind = *enforce_kind;
            let mut iter = contents.split_whitespace();
            while let Some(token) = iter.next() {
                if token.is_empty() {
                    continue;
                }

                let token_lower = token.to_ascii_lowercase();
                if token == "!" || token_lower == "clear" {
                    if options.list_clear_allowed() {
                        entries.rules.clear();
                        entries.exclude_if_present.clear();
                        continue;
                    }
                    let directive = if token == "!" { "!" } else { token };
                    return Err(map_error(FilterParseError::new(format!(
                        "list-clearing '{directive}' is not permitted in this filter file"
                    ))));
                }

                if let Some(kind) = enforce_kind {
                    let rule = match kind {
                        DirMergeEnforcedKind::Include => FilterRule::include(token.to_string()),
                        DirMergeEnforcedKind::Exclude => FilterRule::exclude(token.to_string()),
                    };
                    entries.push_rule(apply_dir_merge_rule_defaults(rule, options));
                    continue;
                }

                let mut directive = token.to_string();
                let lower = directive.to_ascii_lowercase();
                let needs_argument = matches!(
                    lower.as_str(),
                    "merge"
                        | "include"
                        | "exclude"
                        | "show"
                        | "hide"
                        | "protect"
                        | "exclude-if-present"
                ) || lower.starts_with("dir-merge");

                if needs_argument && let Some(next) = iter.next() {
                    directive.push(' ');
                    directive.push_str(next);
                }

                match parse_filter_directive_line(&directive) {
                    Ok(Some(ParsedFilterDirective::Rule(rule))) => {
                        entries.push_rule(apply_dir_merge_rule_defaults(rule, options));
                    }
                    Ok(Some(ParsedFilterDirective::ExcludeIfPresent(rule))) => {
                        entries.push_exclude_if_present(rule);
                    }
                    Ok(Some(ParsedFilterDirective::Clear)) => {
                        entries.rules.clear();
                        entries.exclude_if_present.clear();
                    }
                    Ok(Some(ParsedFilterDirective::Merge {
                        path: merge_path,
                        options: merge_options,
                    })) => {
                        let nested = if merge_path.is_absolute() {
                            merge_path
                        } else {
                            let parent = path.parent().unwrap_or_else(|| Path::new("."));
                            parent.join(merge_path)
                        };
                        if let Some(options_override) = merge_options {
                            let nested_entries = load_dir_merge_rules_recursive(
                                &nested,
                                &options_override,
                                visited,
                            )?;
                            entries.extend(nested_entries);
                        } else {
                            let nested_entries =
                                load_dir_merge_rules_recursive(&nested, options, visited)?;
                            entries.extend(nested_entries);
                        }
                    }
                    Ok(None) => {}
                    Err(error) => return Err(map_error(error)),
                }
            }
        }
        DirMergeParser::Lines {
            enforce_kind,
            allow_comments,
        } => {
            let enforce_kind = *enforce_kind;
            let allow_comments = *allow_comments;
            for line in contents.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if allow_comments && trimmed.starts_with('#') {
                    continue;
                }

                if trimmed == "!" || trimmed.eq_ignore_ascii_case("clear") {
                    if options.list_clear_allowed() {
                        entries.rules.clear();
                        entries.exclude_if_present.clear();
                        continue;
                    }
                    return Err(map_error(FilterParseError::new(format!(
                        "list-clearing '{trimmed}' is not permitted in this filter file"
                    ))));
                }

                if let Some(kind) = enforce_kind {
                    let rule = match kind {
                        DirMergeEnforcedKind::Include => FilterRule::include(trimmed.to_string()),
                        DirMergeEnforcedKind::Exclude => FilterRule::exclude(trimmed.to_string()),
                    };
                    entries.push_rule(apply_dir_merge_rule_defaults(rule, options));
                    continue;
                }

                match parse_filter_directive_line(trimmed) {
                    Ok(Some(ParsedFilterDirective::Rule(rule))) => {
                        entries.push_rule(apply_dir_merge_rule_defaults(rule, options));
                    }
                    Ok(Some(ParsedFilterDirective::ExcludeIfPresent(rule))) => {
                        entries.push_exclude_if_present(rule);
                    }
                    Ok(Some(ParsedFilterDirective::Merge {
                        path: merge_path,
                        options: merge_options,
                    })) => {
                        let nested = if merge_path.is_absolute() {
                            merge_path
                        } else {
                            let parent = path.parent().unwrap_or_else(|| Path::new("."));
                            parent.join(merge_path)
                        };
                        if let Some(options_override) = merge_options {
                            let nested_entries = load_dir_merge_rules_recursive(
                                &nested,
                                &options_override,
                                visited,
                            )?;
                            entries.extend(nested_entries);
                        } else {
                            let nested_entries =
                                load_dir_merge_rules_recursive(&nested, options, visited)?;
                            entries.extend(nested_entries);
                        }
                    }
                    Ok(Some(ParsedFilterDirective::Clear)) => {
                        entries.rules.clear();
                        entries.exclude_if_present.clear();
                    }
                    Ok(None) => {}
                    Err(error) => return Err(map_error(error)),
                }
            }
        }
    }

    visited.pop();
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== resolve_dir_merge_path tests ====================

    #[test]
    fn resolve_dir_merge_path_relative_pattern() {
        let base = Path::new("/home/user/project");
        let pattern = Path::new(".rsync-filter");
        let result = resolve_dir_merge_path(base, pattern);
        assert_eq!(result, PathBuf::from("/home/user/project/.rsync-filter"));
    }

    #[test]
    fn resolve_dir_merge_path_absolute_pattern_strips_root() {
        let base = Path::new("/base");
        let pattern = Path::new("/subdir/filter");
        let result = resolve_dir_merge_path(base, pattern);
        // Absolute pattern with "/" root should be stripped and joined to base
        assert_eq!(result, PathBuf::from("/base/subdir/filter"));
    }

    #[test]
    fn resolve_dir_merge_path_pattern_with_subdirectory() {
        let base = Path::new("/project");
        let pattern = Path::new("config/.filter");
        let result = resolve_dir_merge_path(base, pattern);
        assert_eq!(result, PathBuf::from("/project/config/.filter"));
    }

    #[test]
    fn resolve_dir_merge_path_empty_pattern() {
        let base = Path::new("/home");
        let pattern = Path::new("");
        let result = resolve_dir_merge_path(base, pattern);
        assert_eq!(result, PathBuf::from("/home"));
    }

    #[test]
    fn resolve_dir_merge_path_dot_pattern() {
        let base = Path::new("/base");
        let pattern = Path::new(".");
        let result = resolve_dir_merge_path(base, pattern);
        assert_eq!(result, PathBuf::from("/base/."));
    }

    // ==================== DirMergeEntries tests ====================

    #[test]
    fn dir_merge_entries_default_is_empty() {
        let entries = DirMergeEntries::default();
        assert!(entries.rules.is_empty());
        assert!(entries.exclude_if_present.is_empty());
    }

    #[test]
    fn dir_merge_entries_push_rule() {
        let mut entries = DirMergeEntries::default();
        let rule = FilterRule::exclude("*.tmp".to_string());
        entries.push_rule(rule);
        assert_eq!(entries.rules.len(), 1);
    }

    #[test]
    fn dir_merge_entries_push_multiple_rules() {
        let mut entries = DirMergeEntries::default();
        entries.push_rule(FilterRule::exclude("*.tmp".to_string()));
        entries.push_rule(FilterRule::include("*.rs".to_string()));
        entries.push_rule(FilterRule::exclude("target/".to_string()));
        assert_eq!(entries.rules.len(), 3);
    }

    #[test]
    fn dir_merge_entries_push_exclude_if_present() {
        let mut entries = DirMergeEntries::default();
        let rule = ExcludeIfPresentRule::new(".nobackup".to_string());
        entries.push_exclude_if_present(rule);
        assert_eq!(entries.exclude_if_present.len(), 1);
    }

    #[test]
    fn dir_merge_entries_extend_merges_both_vecs() {
        let mut entries1 = DirMergeEntries::default();
        entries1.push_rule(FilterRule::exclude("*.tmp".to_string()));
        entries1.push_exclude_if_present(ExcludeIfPresentRule::new(".skip".to_string()));

        let mut entries2 = DirMergeEntries::default();
        entries2.push_rule(FilterRule::include("*.rs".to_string()));
        entries2.push_exclude_if_present(ExcludeIfPresentRule::new(".ignore".to_string()));

        entries1.extend(entries2);

        assert_eq!(entries1.rules.len(), 2);
        assert_eq!(entries1.exclude_if_present.len(), 2);
    }

    #[test]
    fn dir_merge_entries_extend_empty_into_populated() {
        let mut entries = DirMergeEntries::default();
        entries.push_rule(FilterRule::exclude("*.log".to_string()));

        let empty = DirMergeEntries::default();
        entries.extend(empty);

        assert_eq!(entries.rules.len(), 1);
    }

    #[test]
    fn dir_merge_entries_extend_populated_into_empty() {
        let mut entries = DirMergeEntries::default();

        let mut populated = DirMergeEntries::default();
        populated.push_rule(FilterRule::include("*.md".to_string()));

        entries.extend(populated);

        assert_eq!(entries.rules.len(), 1);
    }

}
