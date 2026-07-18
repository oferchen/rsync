//! `-x`/`--one-file-system` parity between oc-rsync and upstream.
//!
//! Builds a small tree that lives entirely on a single filesystem, then pulls
//! it with each client over every transport and asserts oc's destination is
//! byte-identical to upstream's. On a single filesystem `-x` must be a no-op:
//! nothing crosses a mount boundary, so the whole tree still transfers.
//!
//! Scope note: the *cross-mount pruning* path - where `-x` stops recursion at a
//! nested mount of a different filesystem inside the source tree - is not
//! exercised here. Creating such a nested mount requires root, so that path is
//! intentionally out of scope. This check validates the common single-filesystem
//! case that is fully testable as an ordinary, unprivileged user.

use std::path::Path;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The one-file-system parity check.
pub struct OneFileSystem;

/// Recurse and preserve metadata, with `-x` under test and numeric ids so the
/// comparison never depends on name-service lookups.
const FLAGS: &[&str] = &["-rlptgoD", "-x", "--numeric-ids"];

impl Check for OneFileSystem {
    fn name(&self) -> &'static str {
        "one-file-system"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("one-file-system");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let expected = support::entry_count(&src);
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        ctx.transports
            .iter()
            .map(|&t| self.cell(ctx, t, &root, &flags, expected))
            .collect()
    }
}

impl OneFileSystem {
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

        let up = pull_into(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            &src,
            &up_dst,
            flags,
            ctx.work,
        );
        match up {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), label, "upstream", other),
        }
        let oc = pull_into(
            transport,
            ctx.oc,
            ctx.upstream,
            &src,
            &oc_dst,
            flags,
            ctx.work,
        );
        match oc {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), label, "oc", other),
        }

        // Non-vacuous guard: `-x` on a single filesystem prunes nothing, so both
        // destinations must carry the whole source tree, not an empty subset.
        if support::entry_count(&up_dst) != expected {
            return CheckOutcome::fail(self.name(), label, "upstream dest entry count != source");
        }
        if support::entry_count(&oc_dst) != expected {
            return CheckOutcome::fail(self.name(), label, "oc dest entry count != source");
        }
        match support::content_diff(&oc_dst, &up_dst) {
            Some(diff) => CheckOutcome::fail(self.name(), label, diff),
            None => CheckOutcome::pass(self.name(), label),
        }
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

/// Build the single-filesystem fixture. Idempotent: removes any prior tree
/// first. The whole tree - `a.txt`, `sub/b.txt`, `sub/deep/c.txt` - lives on one
/// filesystem, so `-x` has no mount boundary to stop at. Mtimes are backdated so
/// the quick-check never skips a file under test.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let deep = src.join("sub").join("deep");
    std::fs::create_dir_all(&deep).map_err(|e| e.to_string())?;

    std::fs::write(src.join("a.txt"), b"alpha").map_err(|e| e.to_string())?;
    std::fs::write(src.join("sub").join("b.txt"), b"bravo").map_err(|e| e.to_string())?;
    std::fs::write(deep.join("c.txt"), b"charlie").map_err(|e| e.to_string())?;

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
