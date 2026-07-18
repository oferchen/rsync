//! Hard-link preservation parity between oc-rsync and upstream.
//!
//! Builds a fixture with two multi-link inode groups (one within a directory,
//! one spanning two subdirectories) plus a standalone file, then pulls it with
//! each client over every transport under `-H`. The canonical check groups each
//! destination's regular files by `(dev, ino)` and asserts oc-rsync reproduces
//! the exact same set of hard-link groups upstream does - a client that broke
//! links into independent copies would yield no groups and fail.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The hard-link preservation parity check.
pub struct HardLinks;

/// Preserve metadata plus hard links, using numeric ids for a non-root run.
const FLAGS: &[&str] = &["-rlptgoD", "-H", "--numeric-ids"];

impl Check for HardLinks {
    fn name(&self) -> &'static str {
        "hard-links"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("hard-links");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }

        // Fixture sanity: the source must actually carry the two multi-link
        // groups the transport cells rely on, so a passing cell is non-vacuous.
        let src_groups = groups(&src);
        if !src_groups.contains(&group_of(&["a1", "a2", "a3"]))
            || !src_groups.contains(&group_of(&["d1/b1", "d2/b2"]))
        {
            return vec![CheckOutcome::skip(
                self.name(),
                "fixture",
                "source is missing the expected hard-link groups",
            )];
        }

        let expected = support::entry_count(&src);
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        ctx.transports
            .iter()
            .map(|&t| self.cell(ctx, t, &root, &flags, expected))
            .collect()
    }
}

impl HardLinks {
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        flags: &[String],
        expected: usize,
    ) -> CheckOutcome {
        let label = transport.label();
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }
        let src = root.join("src");
        let oc_dst = root.join(format!("oc-{label}"));
        let up_dst = root.join(format!("up-{label}"));

        let up = match pull_into(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            &src,
            &up_dst,
            flags,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), label, "upstream", other),
        };
        let _ = up;
        let oc = match pull_into(
            transport,
            ctx.oc,
            ctx.upstream,
            &src,
            &oc_dst,
            flags,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), label, "oc", other),
        };
        let _ = oc;

        // Genuine-result guard: both trees must be fully populated.
        if support::entry_count(&up_dst) != expected || support::entry_count(&oc_dst) != expected {
            return CheckOutcome::fail(self.name(), label, "destination entry count != source");
        }
        if let Some(diff) = support::content_diff(&oc_dst, &up_dst) {
            return CheckOutcome::fail(self.name(), label, diff);
        }

        let oc_groups = groups(&oc_dst);
        let up_groups = groups(&up_dst);
        if oc_groups != up_groups {
            if ctx.verbose {
                eprintln!("[hard-links/{label}] oc groups:       {oc_groups:?}");
                eprintln!("[hard-links/{label}] upstream groups: {up_groups:?}");
            }
            return CheckOutcome::fail(
                self.name(),
                label,
                format!("hardlink groups differ: oc={oc_groups:?} upstream={up_groups:?}"),
            );
        }
        // A client that copied each link as its own inode would leave no groups.
        if oc_groups.is_empty() {
            return CheckOutcome::fail(self.name(), label, "no hardlink groups preserved");
        }
        CheckOutcome::pass(self.name(), label)
    }
}

/// The set of hard-link groups under `root`.
///
/// Maps every regular file's `(dev, ino)` to the set of its relative paths, then
/// returns the path-sets holding two or more links (the multi-link groups).
/// Symlinks and directories are ignored. The result is independent of readdir
/// order, so it is the canonical grouping to compare two trees by.
pub fn groups(root: &Path) -> BTreeSet<BTreeSet<String>> {
    let mut by_inode: BTreeMap<(u64, u64), BTreeSet<String>> = BTreeMap::new();
    for rel in support::rel_entries(root) {
        let path = root.join(&rel);
        let Ok(meta) = path.symlink_metadata() else {
            continue;
        };
        if !meta.file_type().is_file() {
            continue;
        }
        by_inode
            .entry((meta.dev(), meta.ino()))
            .or_default()
            .insert(rel.to_string_lossy().into_owned());
    }
    by_inode
        .into_values()
        .filter(|paths| paths.len() >= 2)
        .collect()
}

/// A single expected group from string literals, for fixture assertions.
fn group_of(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|s| s.to_string()).collect()
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

/// Build the hard-link fixture. Idempotent: removes any prior tree first.
///
/// Group A (`a1`/`a2`/`a3`) is three top-level links to one inode; group B
/// (`d1/b1`/`d2/b2`) is two links to a second inode in different subdirectories;
/// `solo.txt` is a standalone regular file. All mtimes are backdated so the
/// quick-check does not skip the transfer under test.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let d1 = src.join("d1");
    let d2 = src.join("d2");
    std::fs::create_dir_all(&d1).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&d2).map_err(|e| e.to_string())?;

    // Group A: three links to one inode at the top level.
    std::fs::write(src.join("a1"), b"group-a").map_err(|e| e.to_string())?;
    std::fs::hard_link(src.join("a1"), src.join("a2")).map_err(|e| e.to_string())?;
    std::fs::hard_link(src.join("a1"), src.join("a3")).map_err(|e| e.to_string())?;

    // Group B: two links to a second inode across different subdirectories.
    std::fs::write(d1.join("b1"), b"group-b").map_err(|e| e.to_string())?;
    std::fs::hard_link(d1.join("b1"), d2.join("b2")).map_err(|e| e.to_string())?;

    // Standalone regular file with its own inode.
    std::fs::write(src.join("solo.txt"), b"solo").map_err(|e| e.to_string())?;

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
    use super::{group_of, groups};
    use std::fs;

    #[test]
    fn groups_finds_multi_link_sets_and_excludes_solo() {
        // Build the real inode layout with `std::fs::hard_link` - no GNU
        // `touch -d @epoch`, so this test is portable off Linux.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("d1")).unwrap();
        fs::create_dir(root.join("d2")).unwrap();

        fs::write(root.join("a1"), b"a").unwrap();
        fs::hard_link(root.join("a1"), root.join("a2")).unwrap();
        fs::hard_link(root.join("a1"), root.join("a3")).unwrap();

        fs::write(root.join("d1/b1"), b"b").unwrap();
        fs::hard_link(root.join("d1/b1"), root.join("d2/b2")).unwrap();

        fs::write(root.join("solo.txt"), b"s").unwrap();

        let found = groups(root);
        assert_eq!(found.len(), 2, "exactly the two multi-link groups");
        assert!(found.contains(&group_of(&["a1", "a2", "a3"])));
        assert!(found.contains(&group_of(&["d1/b1", "d2/b2"])));
        // The standalone file has one link and is never part of a group.
        assert!(!found.iter().any(|set| set.contains("solo.txt")));
    }
}
