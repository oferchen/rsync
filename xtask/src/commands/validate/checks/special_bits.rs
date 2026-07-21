//! Setuid / setgid / sticky bit parity between oc-rsync and upstream.
//!
//! Builds a fixture whose entries carry the special permission bits (setuid
//! `04000`, setgid `02000`, sticky `01000`), pulls it with each client over
//! every transport, and asserts oc's destination preserves those bits exactly
//! as upstream does. Some filesystems silently strip setuid/setgid on write, so
//! the check probes the work filesystem first and skips gracefully when the bits
//! cannot survive locally.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The special-permission-bit parity check.
pub struct SpecialBits;

/// Preserve perms/owner/group without hardlinks; `-p` inside `-rlptgoD` carries
/// the full mode word, including the setuid/setgid/sticky bits.
const FLAGS: &[&str] = &["-rlptgoD", "--numeric-ids"];

impl Check for SpecialBits {
    fn name(&self) -> &'static str {
        "special-bits"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("special-bits");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        // Support probe: skip the whole matrix if the work fs cannot hold the
        // setuid/setgid bits we are about to compare.
        if !fs_preserves_special_bits(&src) {
            return vec![CheckOutcome::skip(
                self.name(),
                "support",
                "filesystem strips setuid/setgid",
            )];
        }
        let expected = support::entry_count(&src);
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        ctx.transports
            .iter()
            .map(|&t| self.cell(ctx, t, &root, &src, &flags, expected))
            .collect()
    }
}

impl SpecialBits {
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        src: &Path,
        flags: &[String],
        expected: usize,
    ) -> CheckOutcome {
        let label = transport.label();
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }
        let oc_dst = root.join(format!("oc-{label}"));
        let up_dst = root.join(format!("up-{label}"));

        let up = match pull_into(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            src,
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
            src,
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
        // Non-vacuous guard: the special bits must actually be present in oc's
        // destination, so a symmetric both-sides-drop bug still fails the check.
        if let Some(missing) = missing_special_bit(&oc_dst) {
            return CheckOutcome::fail(self.name(), label, missing);
        }
        match mode_diff(&oc_dst, &up_dst) {
            Some(diff) => {
                if ctx.verbose {
                    dump_modes(&oc_dst, &up_dst);
                }
                CheckOutcome::fail(self.name(), label, diff)
            }
            None => CheckOutcome::pass(self.name(), label),
        }
    }
}

/// Map a tree to per-entry `mode & 0o7777` for comparison.
fn mode_map(root: &Path) -> BTreeMap<PathBuf, u32> {
    support::rel_entries(root)
        .into_iter()
        .filter_map(|rel| {
            let meta = root.join(&rel).symlink_metadata().ok()?;
            Some((rel, meta.mode() & 0o7777))
        })
        .collect()
}

/// First permission-word divergence between two trees, or `None` when identical.
fn mode_diff(oc: &Path, up: &Path) -> Option<String> {
    let (a, b) = (mode_map(oc), mode_map(up));
    for (rel, oc_mode) in &a {
        match b.get(rel) {
            None => return Some(format!("missing {} in upstream tree", rel.display())),
            Some(up_mode) if up_mode != oc_mode => {
                return Some(format!(
                    "mode differs at {}: oc={:04o} upstream={:04o}",
                    rel.display(),
                    oc_mode,
                    up_mode
                ));
            }
            Some(_) => {}
        }
    }
    None
}

/// Report the first special bit that failed to arrive in `dst`, or `None` when
/// both the setuid file and setgid file kept their marker bit.
fn missing_special_bit(dst: &Path) -> Option<String> {
    if !has_bit(&dst.join("suid_file"), 0o4000) {
        return Some("suid_file lost the setuid bit in oc destination".into());
    }
    if !has_bit(&dst.join("sgid_file"), 0o2000) {
        return Some("sgid_file lost the setgid bit in oc destination".into());
    }
    None
}

/// True when `path` exists and `mode & bit` is set.
fn has_bit(path: &Path, bit: u32) -> bool {
    path.symlink_metadata()
        .map(|m| m.mode() & bit == bit)
        .unwrap_or(false)
}

/// Print each entry's oc/upstream permission word for verbose diagnostics.
fn dump_modes(oc: &Path, up: &Path) {
    let (a, b) = (mode_map(oc), mode_map(up));
    for (rel, oc_mode) in &a {
        let up_mode = b.get(rel).copied().unwrap_or(0);
        eprintln!(
            "    {}: oc={:04o} upstream={:04o}",
            rel.display(),
            oc_mode,
            up_mode
        );
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

/// Re-stat the fixture's setuid/setgid files; the work filesystem preserves the
/// bits only when both markers survived the initial `chmod`.
fn fs_preserves_special_bits(src: &Path) -> bool {
    has_bit(&src.join("suid_file"), 0o4000) && has_bit(&src.join("sgid_file"), 0o2000)
}

/// Build the special-bits fixture. Idempotent: removes any prior tree first.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(src).map_err(|e| e.to_string())?;

    write_mode(&src.join("suid_file"), b"setuid", 0o4755)?;
    write_mode(&src.join("sgid_file"), b"setgid", 0o2755)?;
    write_mode(&src.join("normal"), b"control", 0o644)?;

    let sub_sgid = src.join("sub_sgid");
    std::fs::create_dir_all(&sub_sgid).map_err(|e| e.to_string())?;
    write_mode(&sub_sgid.join("inside"), b"plain", 0o644)?;
    set_mode(&sub_sgid, 0o2775)?;

    let sticky = src.join("sticky");
    std::fs::create_dir_all(&sticky).map_err(|e| e.to_string())?;
    set_mode(&sticky, 0o1777)?;

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
    fn mode_diff_none_for_identical_special_bits() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for root in [a.path(), b.path()] {
            let f = root.join("suid");
            fs::write(&f, b"x").unwrap();
            // Explicit set_permissions on both sides - no GNU `touch -d @epoch`.
            set_mode(&f, 0o4755).unwrap();
        }
        assert!(mode_diff(a.path(), b.path()).is_none());
    }

    #[test]
    fn mode_diff_names_the_diverging_entry_and_modes() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (fa, fb) = (a.path().join("sgid"), b.path().join("sgid"));
        fs::write(&fa, b"x").unwrap();
        fs::write(&fb, b"x").unwrap();
        set_mode(&fa, 0o2755).unwrap();
        set_mode(&fb, 0o0755).unwrap();
        let diff = mode_diff(a.path(), b.path()).unwrap();
        assert!(diff.contains("sgid"));
        assert!(diff.contains("oc=2755"));
        assert!(diff.contains("upstream=0755"));
    }
}
