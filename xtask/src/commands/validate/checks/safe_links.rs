//! `--safe-links` symlink-safety parity between oc-rsync and upstream.
//!
//! `--safe-links` is a security control: the receiver must drop any symlink whose
//! target escapes the transfer root, so a malicious or careless source tree can
//! never plant a link that resolves outside the destination (the classic symlink
//! escape). Two escape classes must both be refused:
//!
//! - a relative link that climbs out with `../` (`up -> ../outside`), and
//! - an absolute link (`abs -> /abs/path/outside`), which is unsafe regardless of
//!   where it points.
//!
//! A genuinely safe in-tree link (`safe -> good.txt`) must survive. This check
//! pulls the fixture with upstream and with oc under `--safe-links`, asserts the
//! destination trees are entry-for-entry identical, and adds a non-vacuous guard
//! that both escape links are gone while the safe link remains a symlink. A crash
//! or non-zero exit on the adversarial fixture is a failure, not a pass.
//!
//! upstream: util1.c `unsafe_symlink()` - a symlink is unsafe when its target is
//! absolute or uses enough `../` to leave the transfer root; generator.c:2010
//! `keep_dirlinks`/`safe_symlinks` drops unsafe links before writing them.

use std::path::Path;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Category, Check, CheckOutcome, ValidateCtx};

/// The `--safe-links` symlink-safety parity check.
pub struct SafeLinks;

/// Body of the in-tree file the safe link resolves to.
const GOOD_BODY: &[u8] = b"good-body\n";
/// Body of the referent that lives outside the transfer root (never transferred).
const OUTSIDE_BODY: &[u8] = b"outside-body\n";
/// Backdated mtime so the quick-check never skips a fixture entry.
const EPOCH: &str = "@1614830767";

impl Check for SafeLinks {
    fn name(&self) -> &'static str {
        "safe-links"
    }

    fn categories(&self) -> &'static [Category] {
        &[Category::Security]
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("safe-links");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        ctx.transports
            .iter()
            .map(|&t| self.cell(ctx, t, &root, &src))
            .collect()
    }
}

impl SafeLinks {
    /// Run one transport cell: pull the fixture with upstream and oc under
    /// `--safe-links`, compare trees, and assert the drop/keep decisions.
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        src: &Path,
    ) -> CheckOutcome {
        let label = transport.label();
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }
        // A non-chroot daemon munges symlinks (prefixes "/rsyncd-munged/"), making
        // every link absolute so --safe-links drops all of them; the "escape
        // dropped, safe kept" contract cannot hold. upstream: the daemon's
        // munge-symlinks default is on when `use chroot` is off.
        if transport == Transport::Daemon {
            return CheckOutcome::skip(
                self.name(),
                label,
                "daemon munges symlinks; --safe-links drops every link",
            );
        }

        let flags: Vec<String> = ["-rlptgoD", "--safe-links", "--numeric-ids"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let up_dst = root.join(format!("up-{label}"));
        let oc_dst = root.join(format!("oc-{label}"));

        match pull_into(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            src,
            &up_dst,
            &flags,
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
            &flags,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), label, "oc", other),
        }

        if let Some(diff) = support::content_diff(&oc_dst, &up_dst) {
            if ctx.verbose {
                eprintln!(
                    "[safe-links/{label}] oc entries: {:?}",
                    support::rel_entries(&oc_dst)
                );
                eprintln!(
                    "[safe-links/{label}] up entries: {:?}",
                    support::rel_entries(&up_dst)
                );
            }
            return CheckOutcome::fail(self.name(), label, diff);
        }
        if let Some(msg) = safe_links_violation(&oc_dst) {
            return CheckOutcome::fail(self.name(), label, msg);
        }
        CheckOutcome::pass(self.name(), label)
    }
}

/// Non-vacuous guard: both escape links (`up`, `abs`) must be gone and the safe
/// `safe` link must survive as a symlink. Returns the first violation.
fn safe_links_violation(dst: &Path) -> Option<String> {
    for escaped in ["up", "abs"] {
        if dst.join(escaped).symlink_metadata().is_ok() {
            return Some(format!("{escaped} escape symlink survived --safe-links"));
        }
    }
    match dst.join("safe").symlink_metadata() {
        Ok(m) if m.file_type().is_symlink() => None,
        Ok(_) => Some("safe link was dereferenced under --safe-links".to_string()),
        Err(_) => Some("safe in-tree link was dropped by --safe-links".to_string()),
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
                label.to_string(),
                format!("{who} exited {code}: {}", stderr.trim()),
            )
        }
        Err(e) => CheckOutcome::skip(
            check,
            label.to_string(),
            format!("{who} could not run: {e}"),
        ),
    }
}

/// Build the fixture: a real file, a safe in-tree link, a `../` escape link, and
/// an absolute escape link. The escape referent lives outside the transfer root.
/// Idempotent: removes any prior tree first.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(src).map_err(|e| e.to_string())?;
    std::fs::write(src.join("good.txt"), GOOD_BODY).map_err(|e| e.to_string())?;

    let outside = src
        .parent()
        .ok_or("fixture src has no parent")?
        .join("sl-outside");
    std::fs::write(&outside, OUTSIDE_BODY).map_err(|e| e.to_string())?;
    let outside_abs = outside.canonicalize().map_err(|e| e.to_string())?;

    std::os::unix::fs::symlink("good.txt", src.join("safe")).map_err(|e| e.to_string())?;
    std::os::unix::fs::symlink("../sl-outside", src.join("up")).map_err(|e| e.to_string())?;
    std::os::unix::fs::symlink(&outside_abs, src.join("abs")).map_err(|e| e.to_string())?;

    for entry in support::rel_entries(src) {
        let path = src.join(&entry);
        support::capture("touch", &["-h", "-d", EPOCH, &path.to_string_lossy()])
            .map(|_| ())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::safe_links_violation;
    use std::os::unix::fs::symlink;

    #[test]
    fn violation_none_when_escapes_dropped_and_safe_kept() {
        let dir = tempfile::tempdir().unwrap();
        symlink("good.txt", dir.path().join("safe")).unwrap();
        assert!(safe_links_violation(dir.path()).is_none());
    }

    #[test]
    fn violation_flags_a_surviving_relative_escape() {
        let dir = tempfile::tempdir().unwrap();
        symlink("good.txt", dir.path().join("safe")).unwrap();
        symlink("../sl-outside", dir.path().join("up")).unwrap();
        assert!(
            safe_links_violation(dir.path())
                .unwrap()
                .contains("up escape")
        );
    }

    #[test]
    fn violation_flags_a_surviving_absolute_escape() {
        let dir = tempfile::tempdir().unwrap();
        symlink("good.txt", dir.path().join("safe")).unwrap();
        symlink("/etc/hostname", dir.path().join("abs")).unwrap();
        assert!(
            safe_links_violation(dir.path())
                .unwrap()
                .contains("abs escape")
        );
    }

    #[test]
    fn violation_flags_a_dropped_safe_link() {
        let dir = tempfile::tempdir().unwrap();
        // Escapes correctly gone, but the safe link was dropped too.
        assert!(
            safe_links_violation(dir.path())
                .unwrap()
                .contains("was dropped")
        );
    }
}
