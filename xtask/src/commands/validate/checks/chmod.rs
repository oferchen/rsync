//! `--chmod` permission-transform parity between oc-rsync and upstream.
//!
//! Builds a fixture with varied starting permissions and a subdir, then pulls
//! it with each client over every transport while applying one `--chmod` spec,
//! and asserts oc's destination is byte-identical to upstream's and carries the
//! same mode bits on every entry. Two representative specs are exercised as
//! separate cells - one numeric (`D2755,F640`) and one symbolic (`ug=rwX,o=`) -
//! so both `--chmod` grammars are covered. The symbolic spec uses a capital
//! `X` (`ug=rwX,o=`) so directories stay traversable - a lowercase `x` would
//! strip the destination root's execute bit and upstream itself would abort
//! with `exit 23 ... Permission denied`. `--chmod` needs no privilege, so this
//! runs fully as an unprivileged user.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The `--chmod` parity check.
pub struct Chmod;

/// One `--chmod` spec to exercise as its own matrix cell.
struct Spec {
    /// Short label for the cell (the spec without the `--chmod=` prefix).
    label: &'static str,
    /// The full `--chmod=...` argument passed to rsync.
    arg: &'static str,
    /// A file mode the transform must produce on at least one regular file,
    /// used as a non-vacuous guard so a bug that ignores `--chmod` on both
    /// sides cannot pass silently.
    expect_file_mode: u32,
}

/// Numeric and symbolic `--chmod` specs, one cell each.
const SPECS: &[Spec] = &[
    Spec {
        label: "D2755,F640",
        arg: "--chmod=D2755,F640",
        expect_file_mode: 0o640,
    },
    // Capital `X` sets execute only on directories and already-executable
    // files, so directories (including the destination root) stay traversable
    // while a plain 0644 file becomes user rw, group rw, other none => 0o660.
    Spec {
        label: "ug=rwX,o=",
        arg: "--chmod=ug=rwX,o=",
        expect_file_mode: 0o660,
    },
];

impl Check for Chmod {
    fn name(&self) -> &'static str {
        "chmod"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("chmod");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let expected = support::entry_count(&src);

        let mut outcomes = Vec::new();
        for &transport in ctx.transports {
            for (n, spec) in SPECS.iter().enumerate() {
                outcomes.push(self.cell(ctx, transport, &root, spec, n, expected));
            }
        }
        outcomes
    }
}

impl Chmod {
    /// Run one `(transport, spec)` cell: pull with oc and with upstream applying
    /// the same spec, then require identical content, identical mode bits, and
    /// evidence the transform actually took effect.
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        spec: &Spec,
        n: usize,
        expected: usize,
    ) -> CheckOutcome {
        let label = format!("{} {}", transport.label(), spec.label);
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }
        let src = root.join("src");
        let src = src.as_path();
        let oc_dst = root.join(format!("oc-{}-{n}", transport.label()));
        let up_dst = root.join(format!("up-{}-{n}", transport.label()));
        let flags: Vec<String> = ["-rlptgoD", "--numeric-ids", spec.arg]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let up = match pull_into(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            src,
            &up_dst,
            &flags,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), &label, "upstream", other),
        };
        let _ = up;
        let oc = match pull_into(
            transport,
            ctx.oc,
            ctx.upstream,
            src,
            &oc_dst,
            &flags,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), &label, "oc", other),
        };
        let _ = oc;

        // Genuine-result guard: both trees must be fully populated.
        if support::entry_count(&up_dst) != expected || support::entry_count(&oc_dst) != expected {
            return CheckOutcome::fail(self.name(), label, "destination entry count != source");
        }
        if let Some(diff) = support::content_diff(&oc_dst, &up_dst) {
            return CheckOutcome::fail(self.name(), label, diff);
        }
        if let Some(diff) = mode_diff(&oc_dst, &up_dst) {
            if ctx.verbose {
                eprint_modes(&oc_dst, &up_dst);
            }
            return CheckOutcome::fail(self.name(), label, diff);
        }
        // Non-vacuous guard: if both sides silently ignored `--chmod`, their
        // trees would still agree; require the transform to have taken effect.
        if !transform_took_effect(&oc_dst, spec.expect_file_mode) {
            return CheckOutcome::fail(
                self.name(),
                label,
                format!(
                    "--chmod had no effect: no regular file with mode {:o}",
                    spec.expect_file_mode
                ),
            );
        }
        CheckOutcome::pass(self.name(), label)
    }
}

/// Map a tree to per-entry permission bits (`mode & 0o7777`) for comparison.
fn mode_map(root: &Path) -> BTreeMap<std::path::PathBuf, u32> {
    support::rel_entries(root)
        .into_iter()
        .filter_map(|rel| {
            let meta = root.join(&rel).symlink_metadata().ok()?;
            Some((rel, meta.mode() & 0o7777))
        })
        .collect()
}

/// First per-entry mode divergence between two trees, or `None` when identical.
fn mode_diff(oc: &Path, up: &Path) -> Option<String> {
    let (a, b) = (mode_map(oc), mode_map(up));
    for (rel, oc_mode) in &a {
        match b.get(rel) {
            None => return Some(format!("missing {} in upstream tree", rel.display())),
            Some(up_mode) if oc_mode != up_mode => {
                return Some(format!(
                    "mode differs at {}: oc={oc_mode:o} upstream={up_mode:o}",
                    rel.display()
                ));
            }
            _ => {}
        }
    }
    None
}

/// True if at least one regular file under `dst` has exactly `expected` mode
/// bits, i.e. the `--chmod` transform was applied rather than ignored.
fn transform_took_effect(dst: &Path, expected: u32) -> bool {
    support::rel_entries(dst).iter().any(|rel| {
        matches!(
            dst.join(rel).symlink_metadata(),
            Ok(m) if m.file_type().is_file() && m.mode() & 0o7777 == expected
        )
    })
}

/// Print every entry's mode in both trees, for `--verbose` mismatch triage.
fn eprint_modes(oc: &Path, up: &Path) {
    let (a, b) = (mode_map(oc), mode_map(up));
    let mut rels: Vec<_> = a.keys().chain(b.keys()).collect();
    rels.sort();
    rels.dedup();
    for rel in rels {
        let oc_mode = a.get(rel).map(|m| format!("{m:o}")).unwrap_or("-".into());
        let up_mode = b.get(rel).map(|m| format!("{m:o}")).unwrap_or("-".into());
        eprintln!("  {}: oc={oc_mode} upstream={up_mode}", rel.display());
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

/// Build the `--chmod` fixture. Idempotent: removes any prior tree first. Files
/// carry varied starting perms so a transform visibly changes them; mtimes are
/// backdated so the quick-check does not skip anything under test.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;

    write_mode(&src.join("f644"), b"alpha", 0o644)?;
    write_mode(&src.join("f600"), b"bravo", 0o600)?;
    write_mode(&src.join("f755"), b"charlie", 0o755)?;
    write_mode(&sub.join("f640"), b"delta", 0o640)?;

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

fn write_mode(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| e.to_string())?;
    set_mode(path, mode)
}

fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{mode_diff, set_mode};
    use std::fs;

    #[test]
    fn mode_diff_none_for_identical_modes() {
        // Seed equal modes explicitly (portable; no reliance on `touch -d @epoch`).
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for root in [a.path(), b.path()] {
            let f = root.join("f");
            fs::write(&f, b"x").unwrap();
            set_mode(&f, 0o640).unwrap();
        }
        assert!(mode_diff(a.path(), b.path()).is_none());
    }

    #[test]
    fn mode_diff_names_the_diverging_path() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        fs::write(a.path().join("f"), b"x").unwrap();
        fs::write(b.path().join("f"), b"x").unwrap();
        set_mode(&a.path().join("f"), 0o640).unwrap();
        set_mode(&b.path().join("f"), 0o600).unwrap();
        let diff = mode_diff(a.path(), b.path()).unwrap();
        assert!(diff.contains("mode differs"));
        assert!(diff.contains("f"));
    }
}
