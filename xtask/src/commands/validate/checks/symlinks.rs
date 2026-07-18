//! Symlink-handling parity between oc-rsync and upstream.
//!
//! Builds a fixture holding a real file, a safe relative symlink, a symlink into
//! a subdirectory, an unsafe symlink whose target escapes the tree, and a
//! directory symlink, then pulls it with each client over every transport under
//! three symlink modes:
//!
//! - `preserve` (`-l`, inside `-rlptgoD`): symlinks are copied verbatim as
//!   symlinks, never dereferenced.
//! - `safe-links` (`--safe-links`): the outside-pointing symlink is dropped by
//!   both clients while the safe links survive.
//! - `copy-links` (`-L`): every symlink is dereferenced into a regular copy of
//!   its target.
//!
//! Each cell asserts oc's destination is entry-for-entry identical to upstream's
//! via [`support::content_diff`] (which already compares symlink targets) and
//! adds a mode-specific, non-vacuous guard so the comparison cannot pass on two
//! degenerate trees.

use std::path::Path;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The symlink-handling parity check.
pub struct Symlinks;

/// Body of the real file every safe symlink resolves to.
const TARGET_BODY: &[u8] = b"target-body";
/// Body of the nested real file `sub/inner.txt`.
const INNER_BODY: &[u8] = b"inner-body";
/// Body of the referent that lives outside the transfer root.
const OUTSIDE_BODY: &[u8] = b"outside-body";

/// One symlink-handling mode under test.
#[derive(Clone, Copy)]
enum Scenario {
    /// `-l` (inside `-rlptgoD`): copy symlinks verbatim as symlinks.
    Preserve,
    /// `--safe-links`: drop symlinks whose target escapes the transfer root.
    SafeLinks,
    /// `-L`: dereference symlinks into regular copies of their targets.
    CopyLinks,
}

impl Scenario {
    /// All modes, in report order.
    const ALL: [Scenario; 3] = [Scenario::Preserve, Scenario::SafeLinks, Scenario::CopyLinks];

    /// Stable label used in the cell name and destination directory tag.
    fn name(self) -> &'static str {
        match self {
            Scenario::Preserve => "preserve",
            Scenario::SafeLinks => "safe-links",
            Scenario::CopyLinks => "copy-links",
        }
    }

    /// The complete rsync flag set for this mode.
    fn flags(self) -> &'static [&'static str] {
        match self {
            Scenario::Preserve => &["-rlptgoD", "--numeric-ids"],
            Scenario::SafeLinks => &["-rlptgoD", "--safe-links", "--numeric-ids"],
            Scenario::CopyLinks => &["-rLptgoD", "--numeric-ids"],
        }
    }

    /// Mode-specific, non-vacuous guard on a destination tree. Returns the first
    /// violation, or `None` when the mode's effect is genuine.
    fn violation(self, dst: &Path) -> Option<String> {
        match self {
            Scenario::Preserve => preserve_violation(dst),
            Scenario::SafeLinks => safe_links_violation(dst),
            Scenario::CopyLinks => copy_links_violation(dst),
        }
    }
}

impl Check for Symlinks {
    fn name(&self) -> &'static str {
        "symlinks"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("symlinks");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }

        let mut outcomes = Vec::new();
        for &transport in ctx.transports {
            for scenario in Scenario::ALL {
                outcomes.push(self.cell(ctx, transport, scenario));
            }
        }
        outcomes
    }
}

impl Symlinks {
    /// Run one `(transport, scenario)` cell: pull with upstream and with oc under
    /// the same flags, then compare the resulting trees and the mode's effect.
    fn cell(&self, ctx: &ValidateCtx, transport: Transport, scenario: Scenario) -> CheckOutcome {
        let label = format!("{} {}", transport.label(), scenario.name());
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }
        // A non-chroot daemon munges symlinks (prefixes "/rsyncd-munged/"), which
        // makes every link absolute; --safe-links then drops all of them, so the
        // "unsafe dropped, rel survives" contract cannot hold. upstream:
        // rsyncd.conf munge symlinks defaults enabled when use chroot is off.
        if matches!(
            (transport, scenario),
            (Transport::Daemon, Scenario::SafeLinks)
        ) {
            return CheckOutcome::skip(
                self.name(),
                label,
                "daemon munges symlinks; --safe-links drops every link",
            );
        }

        let root = ctx.work.join("symlinks");
        let src = root.join("src");
        let tag = format!("{}-{}", transport.label(), scenario.name());
        let oc_dst = root.join(format!("oc-{tag}"));
        let up_dst = root.join(format!("up-{tag}"));
        let flags: Vec<String> = scenario.flags().iter().map(|s| s.to_string()).collect();

        match pull_into(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            &src,
            &up_dst,
            &flags,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), &label, "upstream", other),
        }
        match pull_into(
            transport,
            ctx.oc,
            ctx.upstream,
            &src,
            &oc_dst,
            &flags,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), &label, "oc", other),
        }

        // Identical trees prove both clients handled every symlink the same way,
        // including symlink targets (compared by content_diff).
        if let Some(diff) = support::content_diff(&oc_dst, &up_dst) {
            if ctx.verbose {
                eprintln!("[symlinks/{label}] oc entries: {:?}", entry_names(&oc_dst));
                eprintln!(
                    "[symlinks/{label}] upstream entries: {:?}",
                    entry_names(&up_dst)
                );
            }
            return CheckOutcome::fail(self.name(), label, diff);
        }

        // Non-vacuous guard: the mode's characteristic effect must be visible.
        if let Some(msg) = scenario.violation(&oc_dst) {
            return CheckOutcome::fail(self.name(), label, msg);
        }

        CheckOutcome::pass(self.name(), label)
    }
}

/// Sorted relative entry names of a destination tree, for verbose dumps.
fn entry_names(dst: &Path) -> Vec<String> {
    support::rel_entries(dst)
        .iter()
        .map(|p| p.display().to_string())
        .collect()
}

/// File type of a destination entry, without following symlinks.
fn kind(dst: &Path, rel: &str) -> Option<std::fs::FileType> {
    dst.join(rel).symlink_metadata().ok().map(|m| m.file_type())
}

/// Under `-l`, every fixture symlink must arrive as a symlink (none dereferenced)
/// and the unsafe link is kept verbatim.
fn preserve_violation(dst: &Path) -> Option<String> {
    for rel in ["rel", "to_inner", "unsafe", "dirlink"] {
        match kind(dst, rel) {
            Some(t) if t.is_symlink() => {}
            Some(_) => return Some(format!("{rel} was dereferenced, expected a symlink")),
            None => return Some(format!("{rel} missing under -l preserve")),
        }
    }
    None
}

/// Under `--safe-links`, the outside-pointing `unsafe` link must be gone while
/// the safe `rel` link survives as a symlink.
fn safe_links_violation(dst: &Path) -> Option<String> {
    if kind(dst, "unsafe").is_some() {
        return Some("unsafe symlink survived --safe-links".to_string());
    }
    match kind(dst, "rel") {
        Some(t) if t.is_symlink() => None,
        Some(_) => Some("rel was dereferenced under --safe-links".to_string()),
        None => Some("safe rel symlink was dropped by --safe-links".to_string()),
    }
}

/// Under `-L`, `rel` must become a regular file carrying the target's bytes.
fn copy_links_violation(dst: &Path) -> Option<String> {
    match kind(dst, "rel") {
        Some(t) if t.is_file() => {}
        Some(_) => return Some("rel is still a symlink under -L".to_string()),
        None => return Some("rel missing under -L".to_string()),
    }
    match std::fs::read(dst.join("rel")) {
        Ok(bytes) if bytes == TARGET_BODY => None,
        Ok(_) => Some("dereferenced rel content != target.txt".to_string()),
        Err(e) => Some(format!("cannot read dereferenced rel: {e}")),
    }
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

/// Build the symlink fixture. Idempotent: removes any prior tree first. Creates a
/// real file, a safe relative symlink, a symlink into a subdirectory, an unsafe
/// symlink escaping the tree, and a directory symlink. The unsafe link's referent
/// is a real file placed beside `src` so `-L` can dereference it cleanly.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;

    std::fs::write(src.join("target.txt"), TARGET_BODY).map_err(|e| e.to_string())?;
    std::fs::write(sub.join("inner.txt"), INNER_BODY).map_err(|e| e.to_string())?;

    // The referent of the unsafe link lives outside the transfer root.
    let outside = src
        .parent()
        .ok_or("fixture src has no parent")?
        .join("outside");
    std::fs::write(&outside, OUTSIDE_BODY).map_err(|e| e.to_string())?;

    std::os::unix::fs::symlink("target.txt", src.join("rel")).map_err(|e| e.to_string())?;
    std::os::unix::fs::symlink("sub/inner.txt", src.join("to_inner")).map_err(|e| e.to_string())?;
    std::os::unix::fs::symlink("../outside", src.join("unsafe")).map_err(|e| e.to_string())?;
    std::os::unix::fs::symlink("sub", src.join("dirlink")).map_err(|e| e.to_string())?;

    // Backdate mtimes so the quick-check does not skip anything under test; `-h`
    // touches the symlinks themselves rather than their referents.
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
    use super::{TARGET_BODY, copy_links_violation, preserve_violation, safe_links_violation};
    use std::fs;
    use std::os::unix::fs::symlink;

    fn all_links(dir: &std::path::Path) {
        symlink("target.txt", dir.join("rel")).unwrap();
        symlink("sub/inner.txt", dir.join("to_inner")).unwrap();
        symlink("../outside", dir.join("unsafe")).unwrap();
        symlink("sub", dir.join("dirlink")).unwrap();
    }

    #[test]
    fn preserve_violation_none_when_all_symlinks_survive() {
        let dir = tempfile::tempdir().unwrap();
        all_links(dir.path());
        assert!(preserve_violation(dir.path()).is_none());
    }

    #[test]
    fn preserve_violation_flags_a_dereferenced_link() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        symlink("sub/inner.txt", d.join("to_inner")).unwrap();
        symlink("../outside", d.join("unsafe")).unwrap();
        symlink("sub", d.join("dirlink")).unwrap();
        // `rel` arrives as a regular file, i.e. dereferenced.
        fs::write(d.join("rel"), TARGET_BODY).unwrap();
        assert!(preserve_violation(d).unwrap().contains("was dereferenced"));
    }

    #[test]
    fn safe_links_violation_none_when_unsafe_dropped_and_rel_kept() {
        let dir = tempfile::tempdir().unwrap();
        symlink("target.txt", dir.path().join("rel")).unwrap();
        assert!(safe_links_violation(dir.path()).is_none());
    }

    #[test]
    fn safe_links_violation_flags_a_surviving_unsafe_link() {
        let dir = tempfile::tempdir().unwrap();
        symlink("target.txt", dir.path().join("rel")).unwrap();
        symlink("../outside", dir.path().join("unsafe")).unwrap();
        let msg = safe_links_violation(dir.path()).unwrap();
        assert!(msg.contains("unsafe symlink survived"));
    }

    #[test]
    fn safe_links_violation_flags_a_dropped_safe_link() {
        let dir = tempfile::tempdir().unwrap();
        // `unsafe` correctly gone, but the safe `rel` link was dropped too.
        let msg = safe_links_violation(dir.path()).unwrap();
        assert!(msg.contains("was dropped"));
    }

    #[test]
    fn copy_links_violation_none_for_regular_file_with_target_body() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("rel"), TARGET_BODY).unwrap();
        assert!(copy_links_violation(dir.path()).is_none());
    }

    #[test]
    fn copy_links_violation_flags_a_link_that_was_not_dereferenced() {
        let dir = tempfile::tempdir().unwrap();
        symlink("target.txt", dir.path().join("rel")).unwrap();
        let msg = copy_links_violation(dir.path()).unwrap();
        assert!(msg.contains("still a symlink"));
    }

    #[test]
    fn copy_links_violation_flags_wrong_dereferenced_content() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("rel"), b"not-the-target").unwrap();
        assert!(copy_links_violation(dir.path()).unwrap().contains("!="));
    }
}
