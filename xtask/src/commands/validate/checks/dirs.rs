//! `-d` / `--dirs` non-recursive directory parity between oc-rsync and upstream.
//!
//! With `-d` (`--dirs`) rsync includes directories it encounters but does not
//! recurse into them: a directory named on the command line with a trailing
//! slash has its immediate entries transferred, yet any subdirectory among those
//! entries is created empty rather than descended. This check pulls a two-level
//! fixture (`top.txt` plus `subdir/nested.txt`) with `-d` (no `-r`) over every
//! transport and asserts oc-rsync's destination is byte- and structure-identical
//! to upstream's, plus a non-vacuous guard that `-d` actually stopped the
//! recursion: the subdirectory exists in the destination but its nested file
//! does not.
//!
//! upstream: options.c:628 `{"dirs", 'd', ..., &xfer_dirs, 2, ...}`; the man page
//! is explicit that a directory's contents are copied only when the operand ends
//! in a trailing slash (or is `.`), while nested subdirectories are never
//! descended without `--recursive`.

use std::path::Path;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The `-d` / `--dirs` non-recursive directory parity check.
pub struct Dirs;

/// Preserve metadata and request non-recursive directory transfer. `-d` is the
/// option under test; deliberately no `-r`. `--numeric-ids` keeps owner
/// comparison host-stable.
const FLAGS: &[&str] = &["-dlptgoD", "--numeric-ids"];

/// Top-level file whose presence proves the immediate entries were transferred.
const TOP: &str = "top.txt";
/// Subdirectory encountered among the immediate entries; `-d` creates it empty.
const SUBDIR: &str = "subdir";
/// File nested one level below the operand; `-d` must NOT transfer it.
const NESTED: &str = "subdir/nested.txt";

impl Check for Dirs {
    fn name(&self) -> &'static str {
        "dirs"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("dirs");
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

impl Dirs {
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

        // Non-vacuous: `-d` transferred the immediate entries (top file present,
        // subdir created) but did not recurse (nested file absent). Assert on
        // both destinations so upstream anchors the expected semantics.
        for (who, dst) in [("oc", &oc_dst), ("upstream", &up_dst)] {
            if !dst.join(TOP).exists() {
                return CheckOutcome::fail(self.name(), label, format!("{who} missing {TOP}"));
            }
            if !dst.join(SUBDIR).is_dir() {
                return CheckOutcome::fail(
                    self.name(),
                    label,
                    format!("{who} did not create {SUBDIR}"),
                );
            }
            if dst.join(NESTED).exists() {
                return CheckOutcome::fail(
                    self.name(),
                    label,
                    format!("{who} recursed into {SUBDIR} despite -d"),
                );
            }
        }

        match support::content_diff(&oc_dst, &up_dst) {
            Some(diff) => CheckOutcome::fail(self.name(), label, diff),
            None => CheckOutcome::pass(self.name(), label),
        }
    }
}

/// Build the two-level fixture. Idempotent: removes any prior tree first. Every
/// entry's mtime is backdated with GNU `touch` so the quick-check makes
/// deterministic decisions and does not re-transfer on mtime alone.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join(SUBDIR);
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;
    std::fs::write(src.join(TOP), b"top-level file\n").map_err(|e| e.to_string())?;
    std::fs::write(src.join(NESTED), b"nested file\n").map_err(|e| e.to_string())?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_lives_below_the_named_subdir() {
        // `-d` keys on this layering: NESTED is one level deeper than SUBDIR, so
        // a non-recursing transfer creates SUBDIR but never NESTED.
        assert_eq!(NESTED, format!("{SUBDIR}/nested.txt"));
        assert!(NESTED.starts_with(SUBDIR));
        assert_ne!(TOP, NESTED);
    }

    #[test]
    fn flags_request_dirs_without_recursion() {
        // The bundled short-flag group must carry `d` but never `r`, so the
        // transfer includes directories without recursing into them.
        assert!(FLAGS[0].contains('d'));
        assert!(!FLAGS[0].contains('r'));
    }
}
