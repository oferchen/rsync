//! The destination-directory decision of upstream's `get_local_name()`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::local_copy::{CopyContext, LocalCopyError, LocalCopyPlan};

use super::metadata::{
    compute_relative_paths, operand_link_stat, operand_stat_path, resolve_effective_metadata,
};

/// One operand that reaches upstream's first file list.
pub(super) struct ListedOperand {
    /// Its index in `plan.sources()`.
    pub(super) index: usize,
    path: PathBuf,
    occurrences: usize,
    /// Whether it is a directory; a `--delete-missing-args` entry is not.
    pub(super) is_dir: bool,
    /// Whether the walk descends into it: a directory under `-r`, or a
    /// trailing-slash directory under `-d`.
    expands: bool,
    /// Whether it names its contents (`src/`, `.`) rather than itself.
    copy_contents: bool,
    /// Whether `--relative` lists its leading directories with it
    /// (flist.c:2979-2997; protocol 30+ forces `implied_dirs`, flist.c:2780-2781).
    has_implied_dirs: bool,
    /// Its name in the file list, the prefix of its children.
    listed: Option<PathBuf>,
}

/// The operands of upstream's first file list (flist.c:2948-2966), in order:
/// every operand `link_stat()` can read, minus those the filter rules or the
/// `skipping directory` gate drop. A missing operand is listed only under
/// `--delete-missing-args`, as a mode-0 entry (flist.c:2952-2955).
pub(super) fn listed_operands(context: &CopyContext, plan: &LocalCopyPlan) -> Vec<ListedOperand> {
    let mut operands = Vec::new();
    for (index, (source, &occurrences)) in plan
        .sources()
        .iter()
        .zip(plan.source_occurrences())
        .enumerate()
    {
        let path = operand_stat_path(context, source);
        // upstream: flist.c:2948-2958 - an operand link_stat() cannot read lists
        // nothing.
        let file_type = match operand_link_stat(context, source, &path) {
            Ok(metadata) => {
                match resolve_effective_metadata(context, &path, &metadata, metadata.file_type()) {
                    Ok((_, file_type)) => Some(file_type),
                    Err(_) => continue,
                }
            }
            Err(_) if context.delete_missing_args_enabled() => None,
            Err(_) => continue,
        };
        let is_dir = file_type.is_some_and(|file_type| file_type.is_dir());
        let expands = is_dir
            && (context.recursive_enabled() || source.copy_contents() && context.dirs_enabled());
        if is_dir && !expands && !context.dirs_enabled() {
            continue;
        }
        let (relative_root, relative_parent) = compute_relative_paths(context, source);
        let listed = if source.copy_contents() {
            relative_root
        } else {
            relative_root.or_else(|| path.file_name().map(PathBuf::from))
        };
        // upstream: flist.c:2961 - a mode-0 missing entry skips the filter check.
        if file_type.is_some()
            && listed
                .as_deref()
                .is_some_and(|name| !walk_lists(context, name, is_dir))
        {
            continue;
        }
        operands.push(ListedOperand {
            index,
            path: path.into_owned(),
            occurrences,
            is_dir,
            expands,
            copy_contents: source.copy_contents(),
            has_implied_dirs: relative_parent.is_some(),
            listed,
        });
    }
    operands
}

/// The part of upstream's `file_total` that `get_local_name()` acts on.
pub(super) enum FileTotal<'a> {
    /// More than one entry: the destination must be a directory (main.c:785,
    /// main.c:808).
    Many,
    /// At most one entry, and the operand that supplies it, if any.
    AtMostOne(Option<&'a ListedOperand>),
}

/// Counts upstream's `file_total` over `operands` as far as `get_local_name()`
/// needs it.
///
/// `file_total` counts the first file list (flist.c:3060): every listed
/// operand, a repeated one included, plus the implied directories `--relative`
/// sends with it. When that list holds a single expanded directory, the
/// directory's own contents join it before the receiver decides (flist.c:3096
/// `send_extra_file_list()`, main.c:1241 `recv_additional_file_list()`), so a
/// directory none of whose children is listed stays a single entry.
pub(super) fn file_total<'a>(
    context: &CopyContext,
    operands: &'a [ListedOperand],
) -> Result<FileTotal<'a>, LocalCopyError> {
    let total: usize = operands.iter().map(|operand| operand.occurrences).sum();
    let Some(operand) = operands.last() else {
        return Ok(FileTotal::AtMostOne(None));
    };
    if total > 1
        || operand.has_implied_dirs
        || operand.expands && lists_a_child(context, &operand.path, operand.listed.as_deref())?
    {
        return Ok(FileTotal::Many);
    }
    Ok(FileTotal::AtMostOne(Some(operand)))
}

/// Parses the per-directory merge files upstream reads before the receiver
/// creates the destination, failing as upstream does with nothing created.
///
/// upstream: flist.c:2117 send_directory() -> push_local_filters() reads a
/// directory's merge files as the sender lists its contents, and the
/// receiver's get_local_name() (main.c:808-822) runs once the first file list
/// has arrived. That list holds a contents operand's (`src/`) children and,
/// without INC_RECURSE, the whole tree; with INC_RECURSE a named directory's
/// children join it only when it is the list's sole entry (flist.c:3096
/// send_extra_file_list()), otherwise they follow in a later list. Merge files
/// deeper in the tree are read while the copy is running, as by upstream's
/// sender; only the operand directories' own are read here.
pub(super) fn parse_operand_merge_files(
    context: &CopyContext,
    operands: &[ListedOperand],
) -> Result<(), LocalCopyError> {
    let total: usize = operands.iter().map(|operand| operand.occurrences).sum();
    for operand in operands.iter().filter(|operand| operand.expands) {
        if context.inc_recurse_enabled() && !operand.copy_contents && total > 1 {
            continue;
        }
        let _rules = context.enter_directory(&operand.path, operand.listed.as_deref())?;
    }
    Ok(())
}

/// Whether the walk of `dir` would list at least one child, judged by the rules
/// it applies there, the directory's own per-dir merge files included.
fn lists_a_child(
    context: &CopyContext,
    dir: &Path,
    prefix: Option<&Path>,
) -> Result<bool, LocalCopyError> {
    let _rules = context.enter_directory(dir, prefix)?;
    // upstream: flist.c:2383-2387 send_directory() - a directory that cannot be
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
