//! Filter-rule parity between oc-rsync and upstream.
//!
//! Builds a tree that trips several exclude kinds (`*.log`, `*.tmp`, a whole
//! `cache/` directory) at the top level and nested, backdates every mtime, then
//! pulls it with each client over every transport under one `--exclude` set.
//! The check asserts oc's destination is entry-for-entry identical to
//! upstream's - which proves both clients excluded and included exactly the same
//! files - and additionally that the excluded names are genuinely absent while
//! at least one file did survive, so the comparison cannot pass vacuously on two
//! empty trees.

use std::path::Path;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The filter-rule parity check.
pub struct Filters;

/// A representative exclude mix: a suffix rule, a second suffix rule, and a
/// directory rule. The point is that oc and upstream must agree on the result.
const FLAGS: &[&str] = &[
    "-rlptgoD",
    "--numeric-ids",
    "--exclude=*.log",
    "--exclude=*.tmp",
    "--exclude=cache/",
];

impl Check for Filters {
    fn name(&self) -> &'static str {
        "filters"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("filters");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        ctx.transports
            .iter()
            .map(|&t| self.cell(ctx, t, &root, &src, &flags))
            .collect()
    }
}

impl Filters {
    /// Run one transport cell: pull with upstream and with oc under the same
    /// exclude set, then compare the resulting trees and the exclusion effect.
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        src: &Path,
        flags: &[String],
    ) -> CheckOutcome {
        let label = transport.label();
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }
        let oc_dst = root.join(format!("oc-{label}"));
        let up_dst = root.join(format!("up-{label}"));

        match pull_into(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            src,
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
            src,
            &oc_dst,
            flags,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), label, "oc", other),
        }

        // Identical trees prove both clients excluded and included the same set.
        if let Some(diff) = support::content_diff(&oc_dst, &up_dst) {
            return CheckOutcome::fail(self.name(), label, diff);
        }

        // Non-vacuous guard: the excluded names must be gone and something kept.
        if let Some(msg) = exclusion_violation(&oc_dst) {
            return CheckOutcome::fail(self.name(), label, msg);
        }

        if ctx.verbose {
            eprintln!(
                "[filters/{label}] oc kept: {:?}",
                survivors(&oc_dst)
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
            );
            eprintln!(
                "[filters/{label}] upstream kept: {:?}",
                survivors(&up_dst)
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
            );
        }

        CheckOutcome::pass(self.name(), label)
    }
}

/// Relative entries surviving the filter in a destination tree.
fn survivors(dst: &Path) -> Vec<std::path::PathBuf> {
    support::rel_entries(dst)
}

/// Verify the excludes actually took effect: no `*.log`/`*.tmp`/`cache` entry
/// remains and at least one regular file survived. Returns a message on the
/// first violation, or `None` when the exclusion result is genuine.
fn exclusion_violation(dst: &Path) -> Option<String> {
    let entries = survivors(dst);
    for rel in &entries {
        let name = rel.to_string_lossy();
        if name.ends_with(".log") || name.ends_with(".tmp") {
            return Some(format!("excluded suffix survived: {name}"));
        }
        if rel
            .components()
            .any(|c| c.as_os_str() == std::ffi::OsStr::new("cache"))
        {
            return Some(format!("excluded directory survived: {name}"));
        }
    }
    let kept_file = entries.iter().any(|rel| {
        dst.join(rel)
            .symlink_metadata()
            .map(|m| m.is_file())
            .unwrap_or(false)
    });
    if !kept_file {
        return Some("no file survived the filter (vacuous result)".to_string());
    }
    None
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

/// Build the filter fixture. Idempotent: removes any prior tree first. Exercises
/// top-level and nested keeps and drops, a `*.tmp` suffix, and a `cache/`
/// directory that a directory rule should prune whole.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    let deep = src.join("nested").join("deep");
    let cache = src.join("cache");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&deep).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;

    std::fs::write(src.join("keep.txt"), b"keep-top").map_err(|e| e.to_string())?;
    std::fs::write(src.join("drop.log"), b"drop-top").map_err(|e| e.to_string())?;
    std::fs::write(src.join("scratch.tmp"), b"drop-tmp").map_err(|e| e.to_string())?;
    std::fs::write(sub.join("keep.txt"), b"keep-sub").map_err(|e| e.to_string())?;
    std::fs::write(sub.join("drop.log"), b"drop-sub").map_err(|e| e.to_string())?;
    std::fs::write(deep.join("keep.txt"), b"keep-deep").map_err(|e| e.to_string())?;
    std::fs::write(cache.join("blob.bin"), b"cached").map_err(|e| e.to_string())?;

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
    use super::exclusion_violation;
    use std::fs;

    #[test]
    fn exclusion_violation_flags_a_surviving_excluded_suffix() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("keep.txt"), b"x").unwrap();
        fs::write(dir.path().join("bad.log"), b"x").unwrap();
        assert!(
            exclusion_violation(dir.path())
                .unwrap()
                .contains("excluded suffix")
        );
    }

    #[test]
    fn exclusion_violation_flags_a_surviving_cache_dir() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("cache")).unwrap();
        fs::write(dir.path().join("cache/blob.bin"), b"x").unwrap();
        assert!(
            exclusion_violation(dir.path())
                .unwrap()
                .contains("excluded directory")
        );
    }

    #[test]
    fn exclusion_violation_flags_a_vacuous_empty_tree() {
        let dir = tempfile::tempdir().unwrap();
        assert!(exclusion_violation(dir.path()).unwrap().contains("vacuous"));
    }

    #[test]
    fn exclusion_violation_none_when_only_kept_files_remain() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("keep.txt"), b"x").unwrap();
        fs::write(dir.path().join("sub/keep.txt"), b"x").unwrap();
        assert!(exclusion_violation(dir.path()).is_none());
    }

    // `build_fixture` backdates mtimes with GNU `touch -d @epoch`, which BSD
    // `touch` (macOS) rejects; the harness itself only runs on Linux.
    #[cfg(target_os = "linux")]
    #[test]
    fn build_fixture_is_idempotent_and_populates_the_tree() {
        use super::build_fixture;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        build_fixture(&src).unwrap();
        // Second call must not error on the pre-existing tree.
        build_fixture(&src).unwrap();
        assert!(src.join("keep.txt").is_file());
        assert!(src.join("drop.log").is_file());
        assert!(src.join("scratch.tmp").is_file());
        assert!(src.join("cache/blob.bin").is_file());
        assert!(src.join("nested/deep/keep.txt").is_file());
    }
}
