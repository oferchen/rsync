//! CVS-exclude (`-C` / `--cvs-exclude`) parity between oc-rsync and upstream.
//!
//! Ports upstream's `cvs-exclude.test`. Builds a tree seeded with files that the
//! built-in CVS ignore patterns should drop (`*.o`, `*.a`, `core`, `*~`, `#*`,
//! `*.bak`, and whole `CVS/` and `.git/` directories), files they must keep
//! (`main.c`, `sub/keep.txt`), and a per-directory `.cvsignore` in `sub/` that
//! names an extra file to ignore (`secret.txt`). Every mtime is backdated so the
//! quick-check cannot skip a file under test.
//!
//! For each transport the tree is pulled with upstream rsync and with oc-rsync
//! under the same `-C` flag set. The check asserts oc's destination is
//! entry-for-entry identical to upstream's (proving both dropped and kept the
//! same set), and additionally that every would-be-excluded name is genuinely
//! absent while the kept files survived - so the comparison cannot pass
//! vacuously on two empty trees.

use std::path::{Path, PathBuf};

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The CVS-exclude parity check.
pub struct CvsExclude;

/// Archive flags plus `-C`, which activates the built-in CVS ignore patterns and
/// per-directory `.cvsignore` handling. oc and upstream must agree on the result.
const FLAGS: &[&str] = &["-rlptgoD", "-C", "--numeric-ids"];

/// Files the transfer must keep (relative to the source root).
const KEPT: &[&str] = &["main.c", "sub/keep.txt"];

impl Check for CvsExclude {
    fn name(&self) -> &'static str {
        "cvs-exclude"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("cvs-exclude");
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

impl CvsExclude {
    /// Run one transport cell: pull with upstream and with oc under `-C`, then
    /// compare the resulting trees and confirm the CVS excludes genuinely fired.
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

        // Identical trees prove both clients dropped and kept the same set.
        if let Some(diff) = support::content_diff(&oc_dst, &up_dst) {
            if ctx.verbose {
                report_survivors(label, &oc_dst, &up_dst);
            }
            return CheckOutcome::fail(self.name(), label, diff);
        }

        // Non-vacuous guard: every would-be-excluded name is gone, keeps remain.
        if let Some(msg) = survivor_violation(&support::rel_entries(&oc_dst)) {
            if ctx.verbose {
                report_survivors(label, &oc_dst, &up_dst);
            }
            return CheckOutcome::fail(self.name(), label, msg);
        }

        CheckOutcome::pass(self.name(), label)
    }
}

/// Print the surviving entries of both destinations for a mismatched cell.
fn report_survivors(label: &str, oc_dst: &Path, up_dst: &Path) {
    let names = |dst: &Path| {
        support::rel_entries(dst)
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
    };
    eprintln!("[cvs-exclude/{label}] oc kept: {:?}", names(oc_dst));
    eprintln!("[cvs-exclude/{label}] upstream kept: {:?}", names(up_dst));
}

/// Classify a surviving entry against the built-in CVS ignore patterns exercised
/// by the fixture. Returns the matched pattern when the entry should have been
/// excluded, or `None` when it is legitimately kept.
///
/// Directory-name patterns (`CVS`, `.git`) match any path component so the whole
/// subtree is caught; the remaining patterns match the final path component.
fn would_be_excluded(rel: &Path) -> Option<&'static str> {
    for comp in rel.components() {
        let c = comp.as_os_str();
        if c == "CVS" {
            return Some("CVS");
        }
        if c == ".git" {
            return Some(".git");
        }
    }
    let name = rel.file_name()?.to_string_lossy();
    if name.ends_with(".o") {
        return Some("*.o");
    }
    if name.ends_with(".a") {
        return Some("*.a");
    }
    if name == "core" {
        return Some("core");
    }
    if name.ends_with('~') {
        return Some("*~");
    }
    if name.starts_with('#') {
        return Some("#*");
    }
    if name.ends_with(".bak") {
        return Some("*.bak");
    }
    None
}

/// Verify the CVS excludes took effect over a destination's surviving entries:
/// no built-in-excluded name and no `.cvsignore`-listed `sub/secret.txt`
/// survived, and every kept file is present. Returns a message on the first
/// violation, or `None` when the exclusion result is genuine. Pure over the
/// entry list so it is unit-testable without a filesystem.
fn survivor_violation(entries: &[PathBuf]) -> Option<String> {
    for rel in entries {
        if let Some(pattern) = would_be_excluded(rel) {
            return Some(format!(
                "CVS-default excluded survived: {} (matches {pattern})",
                rel.display()
            ));
        }
        if rel.to_string_lossy() == "sub/secret.txt" {
            return Some(".cvsignore-excluded survived: sub/secret.txt".to_string());
        }
    }
    for kept in KEPT {
        if !entries.iter().any(|r| r.to_string_lossy() == *kept) {
            return Some(format!("kept file missing: {kept}"));
        }
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

/// Build the CVS-exclude fixture. Idempotent: removes any prior tree first.
///
/// Layout under `src/`:
/// - `main.c`, `sub/keep.txt` - kept.
/// - `obj.o`, `lib.a`, `core`, `backup~`, `#temp#`, `data.bak` - built-in drops.
/// - `.git/config`, `CVS/Entries` - whole directories dropped by name.
/// - `sub/.cvsignore` listing `secret.txt`, and `sub/secret.txt` it excludes.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    let git = src.join(".git");
    let cvs = src.join("CVS");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&git).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&cvs).map_err(|e| e.to_string())?;

    std::fs::write(src.join("main.c"), b"int main(void){return 0;}\n")
        .map_err(|e| e.to_string())?;
    std::fs::write(src.join("obj.o"), b"object").map_err(|e| e.to_string())?;
    std::fs::write(src.join("lib.a"), b"archive").map_err(|e| e.to_string())?;
    std::fs::write(src.join("core"), b"coredump").map_err(|e| e.to_string())?;
    std::fs::write(src.join("backup~"), b"editor-backup").map_err(|e| e.to_string())?;
    std::fs::write(src.join("#temp#"), b"editor-temp").map_err(|e| e.to_string())?;
    std::fs::write(src.join("data.bak"), b"backup").map_err(|e| e.to_string())?;
    std::fs::write(git.join("config"), b"[core]\n").map_err(|e| e.to_string())?;
    std::fs::write(cvs.join("Entries"), b"D\n").map_err(|e| e.to_string())?;
    std::fs::write(sub.join("keep.txt"), b"keep-sub").map_err(|e| e.to_string())?;
    std::fs::write(sub.join(".cvsignore"), b"secret.txt\n").map_err(|e| e.to_string())?;
    std::fs::write(sub.join("secret.txt"), b"per-dir-ignored").map_err(|e| e.to_string())?;

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
    use super::{survivor_violation, would_be_excluded};
    use std::path::PathBuf;

    fn rels(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn would_be_excluded_matches_each_builtin_pattern() {
        assert_eq!(would_be_excluded(&PathBuf::from("obj.o")), Some("*.o"));
        assert_eq!(would_be_excluded(&PathBuf::from("lib.a")), Some("*.a"));
        assert_eq!(would_be_excluded(&PathBuf::from("core")), Some("core"));
        assert_eq!(would_be_excluded(&PathBuf::from("backup~")), Some("*~"));
        assert_eq!(would_be_excluded(&PathBuf::from("#temp#")), Some("#*"));
        assert_eq!(would_be_excluded(&PathBuf::from("data.bak")), Some("*.bak"));
        // Whole-directory patterns catch any nested entry.
        assert_eq!(
            would_be_excluded(&PathBuf::from("CVS/Entries")),
            Some("CVS")
        );
        assert_eq!(
            would_be_excluded(&PathBuf::from(".git/config")),
            Some(".git")
        );
    }

    #[test]
    fn would_be_excluded_keeps_source_and_kept_files() {
        assert_eq!(would_be_excluded(&PathBuf::from("main.c")), None);
        assert_eq!(would_be_excluded(&PathBuf::from("sub/keep.txt")), None);
        assert_eq!(would_be_excluded(&PathBuf::from("sub/secret.txt")), None);
    }

    #[test]
    fn survivor_violation_none_for_a_genuinely_pruned_tree() {
        let entries = rels(&["main.c", "sub", "sub/.cvsignore", "sub/keep.txt"]);
        assert!(survivor_violation(&entries).is_none());
    }

    #[test]
    fn survivor_violation_flags_a_surviving_builtin_exclude() {
        let entries = rels(&["main.c", "obj.o", "sub/keep.txt"]);
        assert!(
            survivor_violation(&entries)
                .unwrap()
                .contains("CVS-default excluded survived")
        );
    }

    #[test]
    fn survivor_violation_flags_a_surviving_cvsignore_entry() {
        let entries = rels(&["main.c", "sub/keep.txt", "sub/secret.txt"]);
        assert!(
            survivor_violation(&entries)
                .unwrap()
                .contains(".cvsignore-excluded survived")
        );
    }

    #[test]
    fn survivor_violation_flags_a_missing_kept_file() {
        let entries = rels(&["main.c"]);
        assert!(
            survivor_violation(&entries)
                .unwrap()
                .contains("kept file missing")
        );
    }
}
