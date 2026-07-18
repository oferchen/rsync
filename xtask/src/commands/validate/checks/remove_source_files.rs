//! `--remove-source-files` source-deletion parity between oc-rsync and upstream.
//!
//! Upstream removes each *transferred regular file* from the sending side after
//! it is delivered, but never removes directories - even ones it just emptied
//! (`main.c` / `finish_transfer`). This check verifies oc-rsync deletes exactly
//! the same source entries upstream does: the regular files disappear from both
//! source trees while every directory survives in both.
//!
//! Because `--remove-source-files` mutates the source, no source tree can be
//! shared between two runs. Every client run gets its own pristine copy of an
//! identical layout (`f1.txt`, `sub/f2.txt`, and an empty `emptydir/`), so the
//! only variable across a cell's two runs is the client under test.
//!
//! The daemon transport is skipped: the shared read-only module rejects
//! `--remove-source-files` outright (upstream `main.c:938`), so removal cannot be
//! exercised over it. For ssh/russh the sender - and therefore the remover - is
//! always upstream, so those cells prove oc-as-receiver propagates the flag over
//! the wire without disturbing the result; the `local` cell is the one that
//! drives oc-rsync's own removal path.

use std::path::Path;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The `--remove-source-files` source-deletion parity check.
pub struct RemoveSourceFiles;

/// Preserve metadata, remove transferred source files, itemize, numeric ids.
const FLAGS: &[&str] = &["-rlptgoD", "--remove-source-files", "-i", "--numeric-ids"];

/// Regular files that `--remove-source-files` must delete from the source.
const REMOVED_FILES: [&str; 2] = ["f1.txt", "sub/f2.txt"];

/// Directories that must survive in the source (rsync never removes dirs).
const KEPT_DIRS: [&str; 2] = ["emptydir", "sub"];

impl Check for RemoveSourceFiles {
    fn name(&self) -> &'static str {
        "remove-source-files"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("remove-source-files");
        // A pristine reference of the delivered layout, never used as a transfer
        // source, so it is never mutated by `--remove-source-files`.
        let reference = root.join("expected");
        if let Err(e) = build_source(&reference) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        ctx.transports
            .iter()
            .map(|&t| self.cell(ctx, t, &root, &reference, &flags))
            .collect()
    }
}

impl RemoveSourceFiles {
    /// Run one transport cell: build two pristine sources, pull each into a
    /// fresh dest, then compare delivered content and surviving source state.
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        reference: &Path,
        flags: &[String],
    ) -> CheckOutcome {
        let label = transport.label();
        if transport == Transport::Daemon {
            return CheckOutcome::skip(
                self.name(),
                label,
                "read-only daemon module rejects --remove-source-files (upstream main.c:938)",
            );
        }
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }

        // Fresh, identical source per client run; each run consumes its own.
        let src_oc = root.join(format!("src-oc-{label}"));
        let src_up = root.join(format!("src-up-{label}"));
        if let Err(e) = build_source(&src_oc) {
            return CheckOutcome::skip(self.name(), label, format!("oc source: {e}"));
        }
        if let Err(e) = build_source(&src_up) {
            return CheckOutcome::skip(self.name(), label, format!("upstream source: {e}"));
        }
        let oc_dst = root.join(format!("oc-{label}"));
        let up_dst = root.join(format!("up-{label}"));

        let up = pull_into(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            &src_up,
            &up_dst,
            flags,
            ctx.work,
        );
        if let Some(bad) = non_success(self.name(), label, "upstream", up) {
            return bad;
        }
        let oc = pull_into(
            transport,
            ctx.oc,
            ctx.upstream,
            &src_oc,
            &oc_dst,
            flags,
            ctx.work,
        );
        if let Some(bad) = non_success(self.name(), label, "oc", oc) {
            return bad;
        }

        // Files were delivered: both dests equal the reference layout.
        let expected = support::entry_count(reference);
        if support::entry_count(&up_dst) != expected || support::entry_count(&oc_dst) != expected {
            return CheckOutcome::fail(self.name(), label, "destination entry count != expected");
        }
        if let Some(diff) = support::content_diff(&oc_dst, reference) {
            return CheckOutcome::fail(self.name(), label, format!("oc dest {diff}"));
        }
        if let Some(diff) = support::content_diff(&up_dst, reference) {
            return CheckOutcome::fail(self.name(), label, format!("upstream dest {diff}"));
        }

        // Source removal parity: identical survivors, files gone, dirs kept.
        let oc_src = rel_strings(&src_oc);
        let up_src = rel_strings(&src_up);
        if ctx.verbose {
            eprintln!(
                "[remove-source-files/{label}] oc survivors={oc_src:?} upstream survivors={up_src:?}"
            );
        }
        match source_diff(&oc_src, &up_src) {
            Some(diff) => CheckOutcome::fail(self.name(), label, diff),
            None => CheckOutcome::pass(self.name(), label),
        }
    }
}

/// Sorted relative entries of `root` as strings, for set comparison.
fn rel_strings(root: &Path) -> Vec<String> {
    support::rel_entries(root)
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

/// First divergence between two post-transfer source trees, or `None` when they
/// match and satisfy the non-vacuous facts: the removed files are absent from
/// both and the kept directories are present in both.
fn source_diff(oc: &[String], up: &[String]) -> Option<String> {
    if oc != up {
        return Some(format!(
            "source survivors differ: oc={oc:?} upstream={up:?}"
        ));
    }
    for f in REMOVED_FILES {
        if oc.iter().any(|e| e == f) {
            return Some(format!("source file {f} not removed"));
        }
    }
    for d in KEPT_DIRS {
        if !oc.iter().any(|e| e == d) {
            return Some(format!("source dir {d} unexpectedly removed"));
        }
    }
    None
}

/// Map a failed or unrunnable transfer to a fail/skip outcome; `None` on success.
fn non_success(
    check: &'static str,
    label: &str,
    who: &str,
    result: crate::error::TaskResult<std::process::Output>,
) -> Option<CheckOutcome> {
    match result {
        Ok(out) if out.status.success() => None,
        Ok(out) => {
            let code = out.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&out.stderr);
            Some(CheckOutcome::fail(
                check,
                label,
                format!("{who} exited {code}: {}", stderr.trim()),
            ))
        }
        Err(e) => Some(CheckOutcome::skip(
            check,
            label,
            format!("{who} could not run: {e}"),
        )),
    }
}

/// Build one pristine source tree. Idempotent: removes any prior tree first.
/// Layout: `f1.txt`, `sub/f2.txt`, and an empty `emptydir/`. Mtimes are backdated
/// so rsync's quick-check does not skip the transfer.
fn build_source(dir: &Path) -> Result<(), String> {
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let sub = dir.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir.join("emptydir")).map_err(|e| e.to_string())?;

    std::fs::write(dir.join("f1.txt"), b"one\n").map_err(|e| e.to_string())?;
    std::fs::write(sub.join("f2.txt"), b"two\n").map_err(|e| e.to_string())?;

    for entry in support::rel_entries(dir) {
        let path = dir.join(&entry);
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
    use super::source_diff;

    fn owned(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn matching_survivors_pass() {
        let survivors = owned(&["emptydir", "sub"]);
        assert!(source_diff(&survivors, &survivors).is_none());
    }

    #[test]
    fn a_lingering_source_file_is_reported() {
        let oc = owned(&["emptydir", "f1.txt", "sub"]);
        let up = owned(&["emptydir", "f1.txt", "sub"]);
        assert!(source_diff(&oc, &up).unwrap().contains("not removed"));
    }

    #[test]
    fn a_removed_directory_is_reported() {
        let both = owned(&["emptydir"]);
        assert!(
            source_diff(&both, &both)
                .unwrap()
                .contains("unexpectedly removed")
        );
    }

    #[test]
    fn diverging_survivor_sets_are_reported() {
        let oc = owned(&["emptydir", "sub"]);
        let up = owned(&["emptydir", "f1.txt", "sub"]);
        assert!(source_diff(&oc, &up).unwrap().contains("survivors differ"));
    }
}
