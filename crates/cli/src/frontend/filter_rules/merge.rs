use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use core::client::{DirMergeEnforcedKind, DirMergeOptions, FilterRuleSpec};
use core::message::{Message, Role};
use core::rsync_error;

use super::cvs::cvs_default_exclude_rules;
use super::directive::{
    FilterDirective, MergeDirective, merge_directive_options, os_string_to_pattern,
};
use super::parsing::parse_filter_directive;
use super::sources::{read_merge_file, read_merge_from_standard_input};

/// Loads a merge file referenced by `directive`, parsing each of its lines and
/// appending the resulting rules to `destination`. `visited` guards against
/// recursive merge cycles; stdin (`-`) is read once.
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
        let canonical = std::fs::canonicalize(&resolved).ok();
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
                .map_or_else(|| base_dir.to_path_buf(), |parent| parent.to_path_buf()),
        )
    };
    let next_base = next_base_storage.as_deref().unwrap_or(base_dir);
    // upstream: exclude.c:1393-1402 resolves FILTRULE_CLEAR_LIST via
    // pop_filter_list (exclude.c:574-590), which truncates only the local
    // section of the per-merge-file rule list. Mirror that by parsing the
    // merge file into a scope-local buffer so a '!' inside the file cannot
    // wipe parent-scope rules already in `destination`.
    let mut local: Vec<FilterRuleSpec> = Vec::new();
    let result = (|| -> Result<(), Message> {
        let contents = if is_stdin {
            read_merge_from_standard_input()?
        } else {
            read_merge_file(&resolved_path)?
        };

        parse_merge_contents(
            &contents, &options, next_base, &display, &mut local, visited,
        )
    })();
    visited.remove(&guard_key);
    if result.is_ok() {
        destination.extend(local);
        if options.excludes_self() && !is_stdin {
            let mut rule = FilterRuleSpec::exclude(original_source_text);
            rule.apply_dir_merge_overrides(&options);
            destination.push(rule);
        }
    }
    result
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
                    DirMergeEnforcedKind::Include => FilterRuleSpec::include(token.to_owned()),
                    DirMergeEnforcedKind::Exclude => FilterRuleSpec::exclude(token.to_owned()),
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
                token.to_owned()
            };

            process_merge_directive(&directive, options, base_dir, display, destination, visited)?;
        }
        return Ok(());
    }

    for line in contents.lines() {
        // upstream: exclude.c:1514 parse_filter_file - a line is skipped only
        // when it is empty or (line parsing) begins with `;`/`#`. Whitespace is
        // never stripped, so leading whitespace and whitespace-only lines fall
        // through to the rule parser and error, while trailing whitespace stays
        // part of the pattern verbatim (exclude.c:1313, strlen length).
        if line.is_empty() {
            continue;
        }
        if options.allows_comments() && (line.starts_with('#') || line.starts_with(';')) {
            continue;
        }

        if line == "!" {
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
            // upstream: FILTRULE_NO_PREFIXES takes the whole line as the pattern
            // verbatim (exclude.c:1122-1124,1313), so no whitespace is trimmed.
            let mut rule = match kind {
                DirMergeEnforcedKind::Include => FilterRuleSpec::include(line.to_owned()),
                DirMergeEnforcedKind::Exclude => FilterRuleSpec::exclude(line.to_owned()),
            };
            rule.apply_dir_merge_overrides(options);
            destination.push(rule);
            continue;
        }

        process_merge_directive(line, options, base_dir, display, destination, visited)?;
    }

    Ok(())
}

/// Parses a single line from a merge file and appends its rule to
/// `destination`, applying the enclosing merge `options` and recursing into any
/// nested merge directive the line introduces.
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
        // A blank line inside a merge file contributes no rule.
        Ok(FilterDirective::Noop) => {}
        // upstream: exclude.c:1441-1443 a non-merge FILTRULE_CVS_IGNORE rule
        // (`-C`) inside a merge file expands to the global CVS defaults at this
        // position in the chain.
        Ok(FilterDirective::CvsDefaults) => {
            let rules = cvs_default_exclude_rules().map_err(|error| {
                let detail = error.to_string();
                rsync_error!(
                    1,
                    format!("failed to process merge file '{display}': {detail}")
                )
                .with_role(Role::Client)
            })?;
            destination.extend(rules);
        }
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
    if keyword.contains('=') {
        return false;
    }

    matches!(
        keyword,
        "include" | "exclude" | "show" | "hide" | "protect" | "risk" | "exclude-if-present"
    ) || keyword.starts_with("merge")
        || keyword.starts_with("dir-merge")
        || keyword == "."
        || keyword == ":"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_argument_include() {
        assert!(merge_directive_requires_argument("include"));
    }

    #[test]
    fn requires_argument_exclude() {
        assert!(merge_directive_requires_argument("exclude"));
    }

    #[test]
    fn requires_argument_show() {
        assert!(merge_directive_requires_argument("show"));
    }

    #[test]
    fn requires_argument_hide() {
        assert!(merge_directive_requires_argument("hide"));
    }

    #[test]
    fn requires_argument_protect() {
        assert!(merge_directive_requires_argument("protect"));
    }

    #[test]
    fn requires_argument_risk() {
        assert!(merge_directive_requires_argument("risk"));
    }

    #[test]
    fn requires_argument_exclude_if_present() {
        assert!(merge_directive_requires_argument("exclude-if-present"));
    }

    #[test]
    fn requires_argument_merge() {
        assert!(merge_directive_requires_argument("merge"));
    }

    #[test]
    fn requires_argument_merge_with_modifiers() {
        assert!(merge_directive_requires_argument("merge,n"));
        assert!(merge_directive_requires_argument("merge,e"));
    }

    #[test]
    fn requires_argument_dir_merge() {
        assert!(merge_directive_requires_argument("dir-merge"));
    }

    #[test]
    fn requires_argument_dot() {
        assert!(merge_directive_requires_argument("."));
    }

    #[test]
    fn requires_argument_colon() {
        assert!(merge_directive_requires_argument(":"));
    }

    #[test]
    fn does_not_require_argument_with_equals() {
        assert!(!merge_directive_requires_argument("include=*.txt"));
        assert!(!merge_directive_requires_argument("exclude=*.log"));
    }

    #[test]
    fn does_not_require_argument_for_unknown() {
        assert!(!merge_directive_requires_argument("clear"));
        assert!(!merge_directive_requires_argument("!"));
        assert!(!merge_directive_requires_argument("unknown"));
    }

    #[test]
    fn does_not_require_argument_for_shorthand() {
        // Single-char shortcuts like + and - don't require separate arguments
        assert!(!merge_directive_requires_argument("+"));
        assert!(!merge_directive_requires_argument("-"));
    }
}
