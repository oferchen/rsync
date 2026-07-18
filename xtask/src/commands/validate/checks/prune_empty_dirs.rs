//! Empty-directory pruning parity between oc-rsync and upstream (`-m`).
//!
//! Builds a fixture mixing file-bearing directories with wholly empty ones
//! (a lone empty dir, an empty nested chain, and a dir that carries no direct
//! files but has a file-bearing subdirectory), then pulls it with `-m`
//! (`--prune-empty-dirs`) using each client over every transport. It asserts
//! oc's destination tree is identical to upstream's - the same directories
//! pruned and the same kept - and, as a non-vacuous guard, that the source
//! genuinely held the empty dirs, that oc omitted exactly those, and that oc
//! kept every directory leading to a file.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The empty-directory pruning parity check.
pub struct PruneEmptyDirs;

/// Recurse and preserve metadata, and prune empty directories (`-m`).
const FLAGS: &[&str] = &["-rlptgoD", "-m", "--numeric-ids"];

/// Directories that hold no file anywhere beneath them; `-m` must drop these.
const PRUNE_TARGETS: &[&str] = &["empty", "empties/a/b"];

/// Directories that lead to a file; `-m` must keep these.
const SURVIVORS: &[&str] = &["full", "mixed/onlysub"];

impl Check for PruneEmptyDirs {
    fn name(&self) -> &'static str {
        "prune-empty-dirs"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("prune-empty-dirs");
        if let Err(e) = build_fixture(&root.join("src")) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        ctx.transports
            .iter()
            .map(|&t| self.cell(ctx, t, &root, &flags))
            .collect()
    }
}

impl PruneEmptyDirs {
    /// Run one transport cell: pull with oc and with upstream, then assert both
    /// destinations pruned identically and that the prune actually happened.
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        flags: &[String],
    ) -> CheckOutcome {
        let label = transport.label();
        let src = root.join("src");
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }
        let oc_dst = root.join(format!("oc-{label}"));
        let up_dst = root.join(format!("up-{label}"));

        match pull_into(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            &src,
            &up_dst,
            flags,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), label, "upstream", other),
        }
        match pull_into(
            transport,
            ctx.oc,
            ctx.upstream,
            &src,
            &oc_dst,
            flags,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), label, "oc", other),
        }

        if ctx.verbose {
            eprintln!(
                "[prune-empty-dirs/{label}] oc kept {:?}",
                sorted(&present_dirs(&oc_dst))
            );
            eprintln!(
                "[prune-empty-dirs/{label}] upstream kept {:?}",
                sorted(&present_dirs(&up_dst))
            );
        }

        // oc and upstream must prune and keep exactly the same directories.
        if let Some(diff) = support::content_diff(&oc_dst, &up_dst) {
            return CheckOutcome::fail(self.name(), label, diff);
        }
        // oc's surviving dirs must be precisely those that lead to a file.
        let expected = surviving_dirs(&source_files(&src));
        if let Some(diff) = dir_set_diff(&present_dirs(&oc_dst), &expected) {
            return CheckOutcome::fail(self.name(), label, format!("oc {diff}"));
        }
        // Non-vacuous guards: the empty dirs existed in the source, oc dropped
        // them, and oc kept every file-bearing directory.
        match prune_guard(&src, &oc_dst) {
            Some(reason) => CheckOutcome::fail(self.name(), label, reason),
            None => CheckOutcome::pass(self.name(), label),
        }
    }
}

/// Directories that must survive `-m`: every ancestor directory of every file.
///
/// A directory is kept iff at least one file lives at or below it, so the
/// surviving set is exactly the union of each file's ancestor directories.
fn surviving_dirs(files: &[PathBuf]) -> BTreeSet<PathBuf> {
    let mut dirs = BTreeSet::new();
    for file in files {
        let mut cur = file.parent();
        while let Some(dir) = cur {
            if dir.as_os_str().is_empty() {
                break;
            }
            dirs.insert(dir.to_path_buf());
            cur = dir.parent();
        }
    }
    dirs
}

/// Relative paths of the regular files under `root` (directories excluded).
fn source_files(root: &Path) -> Vec<PathBuf> {
    support::rel_entries(root)
        .into_iter()
        .filter(|rel| {
            root.join(rel)
                .symlink_metadata()
                .map(|m| m.is_file())
                .unwrap_or(false)
        })
        .collect()
}

/// Relative paths of the directories present under `root`.
fn present_dirs(root: &Path) -> BTreeSet<PathBuf> {
    support::rel_entries(root)
        .into_iter()
        .filter(|rel| {
            root.join(rel)
                .symlink_metadata()
                .map(|m| m.is_dir())
                .unwrap_or(false)
        })
        .collect()
}

/// First divergence between an actual and expected directory set, else `None`.
fn dir_set_diff(actual: &BTreeSet<PathBuf>, expected: &BTreeSet<PathBuf>) -> Option<String> {
    if let Some(extra) = actual.difference(expected).next() {
        return Some(format!("kept an empty directory {}", extra.display()));
    }
    if let Some(missing) = expected.difference(actual).next() {
        return Some(format!(
            "pruned a file-bearing directory {}",
            missing.display()
        ));
    }
    None
}

/// Confirm the prune was real: the empty dirs existed in the source, are gone
/// from the destination, and every file-bearing directory remains.
fn prune_guard(src: &Path, dst: &Path) -> Option<String> {
    for target in PRUNE_TARGETS {
        if !src.join(target).is_dir() {
            return Some(format!("source lacked empty dir {target}"));
        }
        if dst.join(target).exists() {
            return Some(format!("did not prune empty dir {target}"));
        }
    }
    for survivor in SURVIVORS {
        if !dst.join(survivor).is_dir() {
            return Some(format!("pruned file-bearing dir {survivor}"));
        }
    }
    None
}

/// Sorted relative paths, for stable verbose output.
fn sorted(dirs: &BTreeSet<PathBuf>) -> Vec<String> {
    dirs.iter().map(|p| p.display().to_string()).collect()
}

/// Distinguish a genuine divergence from an unrunnable cell (e.g. ssh refused).
fn skip_or_fail(
    check: &'static str,
    label: &str,
    who: &str,
    result: Result<std::process::Output, crate::error::TaskError>,
) -> CheckOutcome {
    match result {
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let code = out.status.code().unwrap_or(-1);
            CheckOutcome::fail(
                check,
                label,
                format!("{who} exited {code}: {}", stderr.trim()),
            )
        }
        Err(e) => CheckOutcome::skip(check, label, format!("{who} could not run: {e}")),
    }
}

/// Build the prune fixture. Idempotent: removes any prior tree first.
///
/// Layout (`*` = pruned by `-m`, others kept):
/// ```text
///   full/keep.txt            full/, full/nested/ kept (hold files)
///   full/nested/leaf.txt
///   empty/                   * lone empty directory
///   empties/a/b/             * empty nested chain (no file anywhere)
///   mixed/onlysub/leaf.txt   mixed/, mixed/onlysub/ kept (subdir holds a file)
/// ```
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let full_nested = src.join("full").join("nested");
    std::fs::create_dir_all(&full_nested).map_err(|e| e.to_string())?;
    std::fs::write(src.join("full").join("keep.txt"), b"keep").map_err(|e| e.to_string())?;
    std::fs::write(full_nested.join("leaf.txt"), b"leaf").map_err(|e| e.to_string())?;

    std::fs::create_dir_all(src.join("empty")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(src.join("empties").join("a").join("b")).map_err(|e| e.to_string())?;

    let onlysub = src.join("mixed").join("onlysub");
    std::fs::create_dir_all(&onlysub).map_err(|e| e.to_string())?;
    std::fs::write(onlysub.join("leaf.txt"), b"mixed").map_err(|e| e.to_string())?;

    // Backdate mtimes so the quick-check does not skip anything under test.
    for entry in support::rel_entries(src) {
        let path = src.join(&entry);
        support::capture(
            "touch",
            &["-h", "-d", "@1614830767", &path.to_string_lossy()],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{dir_set_diff, surviving_dirs};
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn paths(items: &[&str]) -> Vec<PathBuf> {
        items.iter().map(PathBuf::from).collect()
    }

    fn dir_set(items: &[&str]) -> BTreeSet<PathBuf> {
        items.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn surviving_dirs_is_the_union_of_file_ancestors() {
        let files = paths(&[
            "full/keep.txt",
            "full/nested/leaf.txt",
            "mixed/onlysub/leaf.txt",
        ]);
        let kept = surviving_dirs(&files);
        // Every ancestor of a file survives; the empty dirs never appear.
        assert_eq!(
            kept,
            dir_set(&["full", "full/nested", "mixed", "mixed/onlysub"])
        );
        assert!(!kept.contains(&PathBuf::from("empty")));
        assert!(!kept.contains(&PathBuf::from("empties/a/b")));
    }

    #[test]
    fn dir_set_diff_flags_a_kept_empty_dir_and_a_pruned_survivor() {
        let expected = dir_set(&["full", "full/nested"]);
        // A destination that kept an empty "empty/" diverges by keeping too much.
        let kept_too_much = dir_set(&["full", "full/nested", "empty"]);
        assert!(
            dir_set_diff(&kept_too_much, &expected)
                .unwrap()
                .contains("kept an empty directory")
        );
        // A destination missing "full/nested" pruned a directory that held a file.
        let pruned_too_much = dir_set(&["full"]);
        assert!(
            dir_set_diff(&pruned_too_much, &expected)
                .unwrap()
                .contains("pruned a file-bearing directory")
        );
        assert!(dir_set_diff(&expected, &expected).is_none());
    }
}
