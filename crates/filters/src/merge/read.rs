//! File I/O for reading merge files from disk.
//!
//! Provides [`read_rules`] for single-file reads and [`read_rules_recursive`]
//! for automatic expansion of nested `. FILE` (merge) directives.
//!
//! upstream: exclude.c:parse_filter_file() - merge file reading

use std::fs;
use std::path::Path;

use crate::{FilterAction, FilterRule};

use super::error::MergeFileError;
use super::parse::parse_rules;

/// Reads filter rules from a merge file.
///
/// The file is read once and all rules are returned. Lines starting with
/// `#` or `;` are treated as comments. Empty lines are ignored.
///
/// # Recursion
///
/// If the file contains `. FILE` (merge) rules, those files are NOT automatically
/// read. The caller should handle Merge rules by calling this function recursively
/// if desired, or use [`read_rules_recursive`] for automatic expansion.
///
/// # Errors
///
/// Returns an error if the file cannot be read or contains invalid syntax.
pub fn read_rules(path: &Path) -> Result<Vec<FilterRule>, MergeFileError> {
    let content = fs::read_to_string(path).map_err(|e| MergeFileError::io_error(path, &e))?;
    parse_rules(&content, path)
}

/// Reads filter rules from a merge file, recursively expanding nested merge rules.
///
/// Unlike [`read_rules`], this function automatically reads and inlines rules
/// from any `. FILE` (merge) directives encountered. DirMerge rules (`: FILE`)
/// are returned as-is since they are processed during directory traversal.
///
/// # Arguments
///
/// * `path` - The merge file to read
/// * `max_depth` - Maximum recursion depth to prevent infinite loops (typically 10)
///
/// # Errors
///
/// Returns an error if any file cannot be read or contains invalid syntax.
pub fn read_rules_recursive(
    path: &Path,
    max_depth: usize,
) -> Result<Vec<FilterRule>, MergeFileError> {
    read_rules_recursive_impl(path, max_depth, 0)
}

fn read_rules_recursive_impl(
    path: &Path,
    max_depth: usize,
    current_depth: usize,
) -> Result<Vec<FilterRule>, MergeFileError> {
    if current_depth > max_depth {
        return Err(MergeFileError {
            path: path.display().to_string(),
            line: None,
            message: format!("maximum merge depth ({max_depth}) exceeded at depth {current_depth}"),
        });
    }

    let rules = read_rules(path)?;
    let base_dir = path.parent();

    let mut expanded = Vec::with_capacity(rules.len());
    for rule in rules {
        if rule.action() == FilterAction::Merge {
            let merge_path = if rule.pattern().starts_with('/') {
                Path::new(rule.pattern()).to_path_buf()
            } else if let Some(base) = base_dir {
                base.join(rule.pattern())
            } else {
                Path::new(rule.pattern()).to_path_buf()
            };

            // Each nested merge file owns its own clear-rules scope. Resolve
            // any `!` within the nested rule sequence here so it does not
            // leak into the current file's accumulated rules.
            //
            // upstream: exclude.c:1393-1402 — FILTRULE_CLEAR_LIST inside a
            // nested parse_filter_file() invocation only frees the local
            // portion of that file's list before continuing.
            let nested = read_rules_recursive_impl(&merge_path, max_depth, current_depth + 1)?;
            expanded.extend(scope_local_clear(nested));
        } else {
            expanded.push(rule);
        }
    }

    Ok(expanded)
}

/// Resolves `Clear` rules within a merge file's expanded rule sequence so
/// they only clear rules accumulated within that same merge file's scope.
///
/// Iterates the sequence and drops any rules preceding a `Clear` whose side
/// flags the `Clear` covers. The `Clear` itself is consumed; surviving
/// rules (including those that the `Clear` did not cover) are returned.
/// Callers emit the result into the outer scope's rule list so parent
/// rules remain untouched.
///
/// upstream: exclude.c:1393-1402 — `FILTRULE_CLEAR_LIST` calls
/// `pop_filter_list(listp)` and then sets `listp->head = NULL`, removing
/// only the local-scope rules between `head` and `tail`. Inherited rules
/// (which are parent-scope rules in our model) are preserved.
pub(crate) fn scope_local_clear(rules: Vec<FilterRule>) -> Vec<FilterRule> {
    let mut result: Vec<FilterRule> = Vec::with_capacity(rules.len());
    for rule in rules {
        if rule.action() == FilterAction::Clear {
            let clears_sender = rule.applies_to_sender();
            let clears_receiver = rule.applies_to_receiver();
            if !clears_sender && !clears_receiver {
                continue;
            }
            result.retain_mut(|prior| {
                if clears_sender {
                    prior.applies_to_sender = false;
                }
                if clears_receiver {
                    prior.applies_to_receiver = false;
                }
                prior.applies_to_sender || prior.applies_to_receiver
            });
            continue;
        }
        result.push(rule);
    }
    result
}
