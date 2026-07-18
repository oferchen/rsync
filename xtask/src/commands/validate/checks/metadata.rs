//! Content + POSIX metadata parity between oc-rsync and upstream.
//!
//! Builds a fixture carrying varied permissions, a setgid dir, backdated
//! mtimes, a symlink, and a hardlink pair, then pulls it with each client over
//! every transport and asserts oc's destination is byte- and attribute-
//! identical to upstream's (perms, mtime, uid/gid, hardlink count).

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The metadata parity check.
pub struct Metadata;

/// Preserve as much metadata as a non-root user reliably can.
const FLAGS: &[&str] = &["-rlptgoD", "-H", "--numeric-ids"];

impl Check for Metadata {
    fn name(&self) -> &'static str {
        "metadata"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("metadata");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src, ctx.edge_cases) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let expected = support::entry_count(&src);
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        ctx.transports
            .iter()
            .map(|&t| self.cell(ctx, t, &root, &src, &flags, expected))
            .collect()
    }
}

impl Metadata {
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
        match attr_diff(&oc_dst, &up_dst) {
            Some(diff) => CheckOutcome::fail(self.name(), label, diff),
            None => CheckOutcome::pass(self.name(), label),
        }
    }
}

/// Map a tree to per-entry `(mode, mtime, uid, gid, nlink)` for comparison.
fn attr_map(root: &Path) -> BTreeMap<std::path::PathBuf, (u32, i64, u32, u32, u64)> {
    support::rel_entries(root)
        .into_iter()
        .filter_map(|rel| {
            let meta = root.join(&rel).symlink_metadata().ok()?;
            let attrs = (
                meta.mode() & 0o7777,
                meta.mtime(),
                meta.uid(),
                meta.gid(),
                meta.nlink(),
            );
            Some((rel, attrs))
        })
        .collect()
}

/// First per-attribute divergence between two trees, or `None` when identical.
fn attr_diff(oc: &Path, up: &Path) -> Option<String> {
    let (a, b) = (attr_map(oc), attr_map(up));
    for (rel, oc_attrs) in &a {
        let Some(up_attrs) = b.get(rel) else {
            return Some(format!("missing {} in oc tree", rel.display()));
        };
        let fields = ["perms", "mtime", "uid", "gid", "nlink"];
        let mismatches: Vec<&str> = fields
            .iter()
            .enumerate()
            .filter(|(i, _)| !attr_eq(*i, oc_attrs, up_attrs))
            .map(|(_, name)| *name)
            .collect();
        if !mismatches.is_empty() {
            return Some(format!(
                "{} differs at {}",
                mismatches.join("/"),
                rel.display()
            ));
        }
    }
    None
}

fn attr_eq(field: usize, a: &(u32, i64, u32, u32, u64), b: &(u32, i64, u32, u32, u64)) -> bool {
    match field {
        0 => a.0 == b.0,
        1 => a.1 == b.1,
        2 => a.2 == b.2,
        3 => a.3 == b.3,
        _ => a.4 == b.4,
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

/// Build the metadata fixture. Idempotent: removes any prior tree first. When
/// `edge_cases` is set, adds empty files, names with spaces and unicode, deeper
/// nesting, and a dangling symlink.
fn build_fixture(src: &Path, edge_cases: bool) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;

    write_mode(&src.join("f644"), b"alpha", 0o644)?;
    write_mode(&src.join("f600"), b"bravo", 0o600)?;
    write_mode(&src.join("f755"), b"charlie", 0o755)?;
    write_mode(&sub.join("f640"), b"delta", 0o640)?;

    // Hardlink pair.
    std::fs::write(src.join("h1"), b"linked").map_err(|e| e.to_string())?;
    std::fs::hard_link(src.join("h1"), src.join("h2")).map_err(|e| e.to_string())?;

    // Symlink.
    std::os::unix::fs::symlink("../f644", sub.join("link")).map_err(|e| e.to_string())?;

    if edge_cases {
        write_mode(&src.join("empty"), b"", 0o644)?;
        write_mode(&src.join("with space.txt"), b"spaced", 0o644)?;
        write_mode(&src.join("caf\u{e9}.txt"), b"unicode", 0o644)?;
        let deep = sub.join("deep");
        std::fs::create_dir_all(&deep).map_err(|e| e.to_string())?;
        write_mode(&deep.join("deeper.txt"), b"nested", 0o600)?;
        std::os::unix::fs::symlink("does-not-exist", sub.join("dangling"))
            .map_err(|e| e.to_string())?;
    }

    // setgid directory.
    set_mode(&sub, 0o2775)?;

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
    use super::{attr_diff, set_mode};
    use std::fs;

    #[test]
    fn attr_diff_none_for_hard_linked_identical_entries() {
        // A hard link shares the inode, so mode/mtime/uid/gid/nlink all match
        // without depending on a GNU-only `touch -d @epoch` (portable to BSD).
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let source = a.path().join("f");
        fs::write(&source, b"x").unwrap();
        set_mode(&source, 0o644).unwrap();
        fs::hard_link(&source, b.path().join("f")).unwrap();
        assert!(attr_diff(a.path(), b.path()).is_none());
    }

    #[test]
    fn attr_diff_names_the_diverging_permission() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        fs::write(a.path().join("f"), b"x").unwrap();
        fs::write(b.path().join("f"), b"x").unwrap();
        set_mode(&a.path().join("f"), 0o644).unwrap();
        set_mode(&b.path().join("f"), 0o600).unwrap();
        assert!(attr_diff(a.path(), b.path()).unwrap().contains("perms"));
    }
}
