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
            // Resolve the merge file path relative to the current file's directory
            let merge_path = if rule.pattern().starts_with('/') {
                Path::new(rule.pattern()).to_path_buf()
            } else if let Some(base) = base_dir {
                base.join(rule.pattern())
            } else {
                Path::new(rule.pattern()).to_path_buf()
            };

            let nested = read_rules_recursive_impl(&merge_path, max_depth, current_depth + 1)?;
            expanded.extend(nested);
        } else {
            expanded.push(rule);
        }
    }

    Ok(expanded)
}
