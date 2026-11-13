//! The destination-directory decision of upstream's `get_local_name()`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::local_copy::{CopyContext, LocalCopyError, LocalCopyPlan};

use super::metadata::{
    compute_relative_paths, operand_link_stat, operand_stat_path, resolve_effective_metadata,
};

/// Whether upstream's `file_total` exceeds one for this copy, which makes
/// `get_local_name()` create the destination as a directory (main.c:808).
///
/// `file_total` counts the first file list (flist.c:3060): every listed
/// operand, a repeated one included, after the filter rules (flist.c:2958-2961)
/// and the `skipping directory` gate (flist.c:2963-2966). When that list holds a
/// single expanded directory, the directory's own contents join it before the
/// receiver decides (flist.c:3096 `send_extra_file_list()`, main.c:1241
/// `recv_additional_file_list()`), so a directory none of whose children is
/// listed stays a single entry.
pub(super) fn file_total_exceeds_one(
    context: &CopyContext,
    plan: &LocalCopyPlan,
) -> Result<bool, LocalCopyError> {
    let mut total = 0;
    let mut expanded = None;
    for (source, &occurrences) in plan.sources().iter().zip(plan.source_occurrences()) {
        let path = operand_stat_path(context, source);
        // upstream: flist.c:2937-2947 - an operand link_stat() cannot read lists
        // nothing.
        let Ok(metadata) = operand_link_stat(context, source, &path) else {
            continue;
        };
        let Ok((_, file_type)) =
            resolve_effective_metadata(context, &path, &metadata, metadata.file_type())
        else {
            continue;
        };
        let is_dir = file_type.is_dir();
        let expands = is_dir
            && (context.recursive_enabled() || source.copy_contents() && context.dirs_enabled());
        if is_dir && !expands && !context.dirs_enabled() {
            continue;
        }
        let (relative_root, _) = compute_relative_paths(context, source);
        let listed = if source.copy_contents() {
            relative_root
        } else {
            relative_root.or_else(|| path.file_name().map(PathBuf::from))
        };
        if listed
            .as_deref()
            .is_some_and(|name| !walk_lists(context, name, is_dir))
        {
            continue;
        }
        total += occurrences;
        if total > 1 {
            return Ok(true);
        }
        expanded = expands.then(|| (path.into_owned(), listed));
    }
    match expanded {
        Some((dir, prefix)) => lists_a_child(context, &dir, prefix.as_deref()),
        None => Ok(false),
    }
}

/// Whether the walk of `dir` would list at least one child, judged by the rules
/// it applies there, the directory's own per-dir merge files included.
fn lists_a_child(
    context: &CopyContext,
    dir: &Path,
    prefix: Option<&Path>,
) -> Result<bool, LocalCopyError> {
    let _rules = context.enter_directory(dir, prefix)?;
    // upstream: flist.c:2372-2376 send_directory() - a directory that cannot be
    // opened lists nothing.
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(false);
    };
    for entry in entries.flatten() {
        let child = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&child) else {
            continue;
        };
        let Ok((_, file_type)) =
            resolve_effective_metadata(context, &child, &metadata, metadata.file_type())
        else {
            continue;
        };
        let name = entry.file_name();
        let relative = prefix.map_or_else(|| PathBuf::from(&name), |prefix| prefix.join(&name));
        if walk_lists(context, &relative, file_type.is_dir()) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether the walk lists `relative`: allowed by the filter rules, or, under
/// `--prune-empty-dirs`, a directory excluded only by a non-directory rule,
/// which the walk still descends.
fn walk_lists(context: &CopyContext, relative: &Path, is_dir: bool) -> bool {
    context.allows(relative, is_dir)
        || is_dir
            && context.prune_empty_dirs_enabled()
            && context.excluded_dir_by_non_dir_rule(relative)
}
