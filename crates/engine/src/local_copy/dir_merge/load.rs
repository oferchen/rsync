use super::parse::{FilterParseError, ParsedFilterDirective, parse_filter_directive_line};
use crate::local_copy::LocalCopyError;
use crate::local_copy::filter_program::{
    DirMergeEnforcedKind, DirMergeOptions, DirMergeParser, ExcludeIfPresentRule, FilterProgramError,
};
use filters::FilterRule;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Wraps a `FilterProgramError` as a `LocalCopyError` with file path context,
/// reporting the failure through the standard `compile filter file` operation
/// label so callers see consistent error messages.
pub(crate) fn filter_program_local_error(
    path: &Path,
    error: &FilterProgramError,
) -> LocalCopyError {
    LocalCopyError::io(
        "compile filter file",
        path.to_path_buf(),
        io::Error::new(io::ErrorKind::InvalidData, error.to_string()),
    )
}

/// Resolves a per-directory merge file pattern against `base`.
///
/// Absolute patterns whose root component is `/` are stripped of that root and
/// joined onto `base`, mirroring upstream's treatment of root-anchored
/// `dir-merge` paths. Relative patterns are joined to `base` directly.
pub(crate) fn resolve_dir_merge_path(base: &Path, pattern: &Path) -> PathBuf {
    if pattern.is_absolute()
        && let Ok(stripped) = pattern.strip_prefix(Path::new("/"))
    {
        return base.join(stripped);
    }

    base.join(pattern)
}

/// Applies the per-directory merge `options` to `rule` as defaults.
///
/// Anchors the rule to the source root when configured, marks it perishable,
/// and overrides the sender/receiver flags when the merge directive specified
/// either side modifier. Returns the rule unchanged when no defaults apply.
///
/// When `delete_excluded` is set and neither side modifier was specified on
/// the merge directive, mirrors upstream's per-token implicit SENDER_SIDE
/// flag for rules expanded out of merge/dir-merge files.
///
/// # Upstream Reference
///
/// `exclude.c:1324-1332 parse_rule_tok`:
///
/// ```c
/// if (delete_excluded
///  && !(rule->rflags & (FILTRULES_SIDES|FILTRULE_MERGE_FILE|FILTRULE_PERDIR_MERGE)))
///     rule->rflags |= FILTRULE_SENDER_SIDE;
/// ```
///
/// The OR'ing fires for every rule produced by `parse_rule_tok`, including
/// tokens expanded from a `:C .cvsignore` per-directory merge or the
/// built-in `get_cvs_excludes()` patterns. Only the wrapper merge/dir-merge
/// directives themselves are skipped (FILTRULE_MERGE_FILE /
/// FILTRULE_PERDIR_MERGE bits), not the per-token rules they expand into.
pub(crate) fn apply_dir_merge_rule_defaults(
    mut rule: FilterRule,
    options: &DirMergeOptions,
    delete_excluded: bool,
) -> FilterRule {
    if options.anchor_root_enabled() {
        rule = rule.anchor_to_root();
    }

    if options.perishable() {
        rule = rule.with_perishable(true);
    }

    let sender_override = options.sender_side_override();
    let receiver_override = options.receiver_side_override();

    if let Some(sender) = sender_override {
        rule = rule.with_sender(sender);
    }

    if let Some(receiver) = receiver_override {
        rule = rule.with_receiver(receiver);
    }

    if delete_excluded
        && sender_override.is_none()
        && receiver_override.is_none()
        && rule.applies_to_sender()
        && rule.applies_to_receiver()
        && matches!(
            rule.action(),
            filters::FilterAction::Include | filters::FilterAction::Exclude
        )
    {
        rule = rule.with_receiver(false);
    }

    rule
}

/// Propagates the enclosing merge file's sender/receiver side onto a nested
/// `dir-merge` directive that did not declare its own side.
///
/// # Upstream Reference
///
/// `exclude.c:1293-1303 parse_rule_tok`:
///
/// ```c
/// if (template->rflags & FILTRULES_SIDES) {
///     if (rule->rflags & FILTRULES_SIDES) { ... reject ... }
///     rule->rflags |= template->rflags & FILTRULES_SIDES;
/// }
/// ```
///
/// Every rule read from a side-specified merge file (`:s`/`:r`) inherits that
/// side, including a nested `dir-merge` directive. The nested merge then
/// becomes the template for its own file, so the side propagates transitively.
/// Without this, rules expanded from a `:s`-inherited `dir-merge` keep
/// `applies_to_receiver = true` and wrongly protect destination extras from
/// the receiver's deletion pass.
fn inherit_enclosing_side(enclosing: &DirMergeOptions, child: DirMergeOptions) -> DirMergeOptions {
    if child.sender_side_override().is_some() || child.receiver_side_override().is_some() {
        return child;
    }
    match (
        enclosing.sender_side_override(),
        enclosing.receiver_side_override(),
    ) {
        (None, None) => child,
        (sender, receiver) => child.with_side_overrides(sender, receiver),
    }
}

/// Nested per-directory merge declaration encountered while loading another
/// filter file.
///
/// upstream: exclude.c:1419-1428 - a `dir-merge` directive inside a merge file
/// registers a new per-directory merge rule whose filename gets looked up in
/// every subdirectory subsequently entered. The rule is NOT expanded against
/// the enclosing file's directory.
#[derive(Clone, Debug)]
pub(crate) struct NestedDirMerge {
    /// Bare merge-file name to look up in each subdirectory entered beneath
    /// the scope where this directive appeared.
    pub(crate) pattern: PathBuf,
    /// Parser configuration for the registered per-directory merge rule.
    pub(crate) options: DirMergeOptions,
}

/// Accumulated rules and `exclude-if-present` markers loaded from a single
/// per-directory merge file (and any files it transitively merges).
#[derive(Default)]
pub(crate) struct DirMergeEntries {
    /// Filter rules parsed from the merge file, in source order.
    pub(crate) rules: Vec<FilterRule>,
    /// `exclude-if-present` marker rules parsed from the merge file.
    pub(crate) exclude_if_present: Vec<ExcludeIfPresentRule>,
    /// Nested `dir-merge`/`:` declarations to register as per-directory rules
    /// for subsequent subdirectory traversal.
    pub(crate) nested_dir_merges: Vec<NestedDirMerge>,
    /// Indicates a clear directive was encountered, meaning inherited rules
    /// from parent directories should also be cleared.
    pub(crate) clear_inherited: bool,
}

impl DirMergeEntries {
    fn push_rule(&mut self, rule: FilterRule) {
        self.rules.push(rule);
    }

    fn push_exclude_if_present(&mut self, rule: ExcludeIfPresentRule) {
        self.exclude_if_present.push(rule);
    }

    fn push_nested_dir_merge(&mut self, nested: NestedDirMerge) {
        self.nested_dir_merges.push(nested);
    }

    /// Merges another set of entries into this one.
    ///
    /// A `clear_inherited` flag in the nested entries propagates to this set,
    /// matching upstream's behaviour when a merged filter file contains `!`
    /// or `clear`: the directive wipes accumulated rules from this scope as
    /// well as parent scopes.
    fn extend(&mut self, mut other: DirMergeEntries) {
        if other.clear_inherited {
            self.rules.clear();
            self.exclude_if_present.clear();
            self.nested_dir_merges.clear();
            self.clear_inherited = true;
        }
        self.rules.append(&mut other.rules);
        self.exclude_if_present
            .append(&mut other.exclude_if_present);
        self.nested_dir_merges.append(&mut other.nested_dir_merges);
    }
}

/// Loads filter rules from `path`, recursing into nested `merge` directives.
///
/// `options` controls the parser (whitespace-vs-line, comment handling,
/// enforced include/exclude kind) used for this file. `visited` is a
/// canonical-path stack used to detect cycles: if `path` would re-enter a file
/// already on the stack the function returns an error rather than recursing
/// infinitely. On success the visited entry is popped before returning.
///
/// `delete_excluded` propagates upstream's per-token implicit SENDER_SIDE
/// flag (`exclude.c:1324-1332 parse_rule_tok`) onto every rule produced from
/// this merge file when neither side modifier was specified on the dir-merge
/// directive itself. This ensures the receiver's delete-pass treats expanded
/// per-token rules the same way `add_rule()` flags them when the user passed
/// `--delete-excluded`.
pub(crate) fn load_dir_merge_rules_recursive(
    path: &Path,
    options: &DirMergeOptions,
    delete_excluded: bool,
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
        .map_err(|error| LocalCopyError::io("read filter file", path, error))?;
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
        .map_err(|error| LocalCopyError::io("read filter file", path, error))?;

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
                        entries.clear_inherited = true;
                        continue;
                    }
                    let directive = if token == "!" { "!" } else { token };
                    return Err(map_error(FilterParseError::new(format!(
                        "list-clearing '{directive}' is not permitted in this filter file"
                    ))));
                }

                if let Some(kind) = enforce_kind {
                    let rule = match kind {
                        DirMergeEnforcedKind::Include => FilterRule::include(token.to_owned()),
                        DirMergeEnforcedKind::Exclude => FilterRule::exclude(token.to_owned()),
                    };
                    entries.push_rule(apply_dir_merge_rule_defaults(
                        rule,
                        options,
                        delete_excluded,
                    ));
                    continue;
                }

                let mut directive = token.to_owned();
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
                        entries.push_rule(apply_dir_merge_rule_defaults(
                            rule,
                            options,
                            delete_excluded,
                        ));
                    }
                    Ok(Some(ParsedFilterDirective::ExcludeIfPresent(rule))) => {
                        entries.push_exclude_if_present(rule);
                    }
                    Ok(Some(ParsedFilterDirective::Clear)) => {
                        entries.rules.clear();
                        entries.exclude_if_present.clear();
                        entries.nested_dir_merges.clear();
                        entries.clear_inherited = true;
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
                                delete_excluded,
                                visited,
                            )?;
                            entries.extend(nested_entries);
                        } else {
                            let nested_entries = load_dir_merge_rules_recursive(
                                &nested,
                                options,
                                delete_excluded,
                                visited,
                            )?;
                            entries.extend(nested_entries);
                        }
                    }
                    Ok(Some(ParsedFilterDirective::DirMerge {
                        pattern,
                        options: merge_options,
                    })) => {
                        // upstream: exclude.c:1419-1428 - register the merge
                        // filename for lookup in each subdirectory; do NOT
                        // load anything from the enclosing file's directory.
                        // exclude.c:1293-1303 - inherit the enclosing file's
                        // side so a `:s`-loaded `dir-merge` carries sender-side.
                        entries.push_nested_dir_merge(NestedDirMerge {
                            pattern,
                            options: inherit_enclosing_side(options, merge_options),
                        });
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
                        entries.clear_inherited = true;
                        continue;
                    }
                    return Err(map_error(FilterParseError::new(format!(
                        "list-clearing '{trimmed}' is not permitted in this filter file"
                    ))));
                }

                if let Some(kind) = enforce_kind {
                    let rule = match kind {
                        DirMergeEnforcedKind::Include => FilterRule::include(trimmed.to_owned()),
                        DirMergeEnforcedKind::Exclude => FilterRule::exclude(trimmed.to_owned()),
                    };
                    entries.push_rule(apply_dir_merge_rule_defaults(
                        rule,
                        options,
                        delete_excluded,
                    ));
                    continue;
                }

                match parse_filter_directive_line(trimmed) {
                    Ok(Some(ParsedFilterDirective::Rule(rule))) => {
                        entries.push_rule(apply_dir_merge_rule_defaults(
                            rule,
                            options,
                            delete_excluded,
                        ));
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
                                delete_excluded,
                                visited,
                            )?;
                            entries.extend(nested_entries);
                        } else {
                            let nested_entries = load_dir_merge_rules_recursive(
                                &nested,
                                options,
                                delete_excluded,
                                visited,
                            )?;
                            entries.extend(nested_entries);
                        }
                    }
                    Ok(Some(ParsedFilterDirective::DirMerge {
                        pattern,
                        options: merge_options,
                    })) => {
                        // upstream: exclude.c:1419-1428 - register the merge
                        // filename for lookup in each subdirectory; do NOT
                        // load anything from the enclosing file's directory.
                        // exclude.c:1293-1303 - inherit the enclosing file's
                        // side so a `:s`-loaded `dir-merge` carries sender-side.
                        entries.push_nested_dir_merge(NestedDirMerge {
                            pattern,
                            options: inherit_enclosing_side(options, merge_options),
                        });
                    }
                    Ok(Some(ParsedFilterDirective::Clear)) => {
                        entries.rules.clear();
                        entries.exclude_if_present.clear();
                        entries.nested_dir_merges.clear();
                        entries.clear_inherited = true;
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

    #[cfg(unix)]
    #[test]
    fn resolve_dir_merge_path_relative_pattern() {
        let base = Path::new("/home/user/project");
        let pattern = Path::new(".rsync-filter");
        let result = resolve_dir_merge_path(base, pattern);
        assert_eq!(result, PathBuf::from("/home/user/project/.rsync-filter"));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_dir_merge_path_absolute_pattern_strips_root() {
        let base = Path::new("/base");
        let pattern = Path::new("/subdir/filter");
        let result = resolve_dir_merge_path(base, pattern);
        assert_eq!(result, PathBuf::from("/base/subdir/filter"));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_dir_merge_path_pattern_with_subdirectory() {
        let base = Path::new("/project");
        let pattern = Path::new("config/.filter");
        let result = resolve_dir_merge_path(base, pattern);
        assert_eq!(result, PathBuf::from("/project/config/.filter"));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_dir_merge_path_empty_pattern() {
        let base = Path::new("/home");
        let pattern = Path::new("");
        let result = resolve_dir_merge_path(base, pattern);
        assert_eq!(result, PathBuf::from("/home"));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_dir_merge_path_dot_pattern() {
        let base = Path::new("/base");
        let pattern = Path::new(".");
        let result = resolve_dir_merge_path(base, pattern);
        assert_eq!(result, PathBuf::from("/base/."));
    }

    #[test]
    fn nested_dir_merge_inherits_sender_side_from_enclosing() {
        // upstream exclude.c:1293-1303 - a `dir-merge` directive read from a
        // `:s` merge file inherits sender-side, so its rules elide at the
        // receiver delete pass and the excluded dest files become extraneous.
        let enclosing = DirMergeOptions::default().sender_modifier();
        let child = DirMergeOptions::default();
        let inherited = inherit_enclosing_side(&enclosing, child);
        assert_eq!(inherited.sender_side_override(), Some(true));
        assert_eq!(inherited.receiver_side_override(), Some(false));
    }

    #[test]
    fn nested_dir_merge_keeps_its_own_side_over_enclosing() {
        // A nested directive that declares its own side is not overridden by
        // the enclosing merge file's side.
        let enclosing = DirMergeOptions::default().sender_modifier();
        let child = DirMergeOptions::default().receiver_modifier();
        let inherited = inherit_enclosing_side(&enclosing, child);
        assert_eq!(inherited.sender_side_override(), Some(false));
        assert_eq!(inherited.receiver_side_override(), Some(true));
    }

    #[test]
    fn nested_dir_merge_no_inheritance_when_enclosing_is_sideless() {
        let enclosing = DirMergeOptions::default();
        let child = DirMergeOptions::default();
        let inherited = inherit_enclosing_side(&enclosing, child);
        assert_eq!(inherited.sender_side_override(), None);
        assert_eq!(inherited.receiver_side_override(), None);
    }

    #[test]
    fn dir_merge_entries_default_is_empty() {
        let entries = DirMergeEntries::default();
        assert!(entries.rules.is_empty());
        assert!(entries.exclude_if_present.is_empty());
    }

    #[test]
    fn dir_merge_entries_push_rule() {
        let mut entries = DirMergeEntries::default();
        let rule = FilterRule::exclude("*.tmp".to_owned());
        entries.push_rule(rule);
        assert_eq!(entries.rules.len(), 1);
    }

    #[test]
    fn dir_merge_entries_push_multiple_rules() {
        let mut entries = DirMergeEntries::default();
        entries.push_rule(FilterRule::exclude("*.tmp".to_owned()));
        entries.push_rule(FilterRule::include("*.rs".to_owned()));
        entries.push_rule(FilterRule::exclude("target/".to_owned()));
        assert_eq!(entries.rules.len(), 3);
    }

    #[test]
    fn dir_merge_entries_push_exclude_if_present() {
        let mut entries = DirMergeEntries::default();
        let rule = ExcludeIfPresentRule::new(".nobackup".to_owned());
        entries.push_exclude_if_present(rule);
        assert_eq!(entries.exclude_if_present.len(), 1);
    }

    #[test]
    fn dir_merge_entries_extend_merges_both_vecs() {
        let mut entries1 = DirMergeEntries::default();
        entries1.push_rule(FilterRule::exclude("*.tmp".to_owned()));
        entries1.push_exclude_if_present(ExcludeIfPresentRule::new(".skip".to_owned()));

        let mut entries2 = DirMergeEntries::default();
        entries2.push_rule(FilterRule::include("*.rs".to_owned()));
        entries2.push_exclude_if_present(ExcludeIfPresentRule::new(".ignore".to_owned()));

        entries1.extend(entries2);

        assert_eq!(entries1.rules.len(), 2);
        assert_eq!(entries1.exclude_if_present.len(), 2);
    }

    #[test]
    fn dir_merge_entries_extend_empty_into_populated() {
        let mut entries = DirMergeEntries::default();
        entries.push_rule(FilterRule::exclude("*.log".to_owned()));

        let empty = DirMergeEntries::default();
        entries.extend(empty);

        assert_eq!(entries.rules.len(), 1);
    }

    #[test]
    fn dir_merge_entries_extend_populated_into_empty() {
        let mut entries = DirMergeEntries::default();

        let mut populated = DirMergeEntries::default();
        populated.push_rule(FilterRule::include("*.md".to_owned()));

        entries.extend(populated);

        assert_eq!(entries.rules.len(), 1);
    }

    #[test]
    fn dir_merge_entries_default_clear_inherited_is_false() {
        let entries = DirMergeEntries::default();
        assert!(!entries.clear_inherited);
    }

    #[test]
    fn dir_merge_entries_extend_with_clear_inherited_clears_parent() {
        let mut parent_entries = DirMergeEntries::default();
        parent_entries.push_rule(FilterRule::exclude("*.tmp".to_owned()));
        parent_entries.push_rule(FilterRule::exclude("*.bak".to_owned()));
        parent_entries.push_exclude_if_present(ExcludeIfPresentRule::new(".nobackup".to_owned()));

        let mut child_entries = DirMergeEntries {
            clear_inherited: true,
            ..Default::default()
        };
        child_entries.push_rule(FilterRule::include("important.tmp".to_owned()));

        parent_entries.extend(child_entries);

        assert_eq!(parent_entries.rules.len(), 1);
        assert_eq!(parent_entries.exclude_if_present.len(), 0);
        assert!(parent_entries.clear_inherited);
    }

    #[test]
    fn dir_merge_entries_extend_without_clear_preserves_parent() {
        let mut parent_entries = DirMergeEntries::default();
        parent_entries.push_rule(FilterRule::exclude("*.tmp".to_owned()));

        let mut child_entries = DirMergeEntries::default();
        child_entries.push_rule(FilterRule::include("*.rs".to_owned()));

        parent_entries.extend(child_entries);

        assert_eq!(parent_entries.rules.len(), 2);
        assert!(!parent_entries.clear_inherited);
    }
}
