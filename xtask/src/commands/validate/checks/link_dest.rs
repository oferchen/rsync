//! `--link-dest` parity: oc-rsync hardlinks unchanged files from a reference
//! directory instead of copying them, exactly as upstream does.
//!
//! Like the `--delete` check, this needs a pre-built reference tree and an extra
//! flag, so it builds each client `Command` directly rather than going through
//! `transport::pull_into`. For every cell a per-cell reference dir `ref-<label>`
//! is created holding `same.txt` and `sub/also_same.txt` byte- and mtime-
//! identical to the source, plus `changed.txt` carrying older, differing
//! content. Both clients pull the source into their own fresh empty destination
//! with `--link-dest=<absolute ref dir>`.
//!
//! Upstream rsync is the ground truth. A file that is unchanged relative to the
//! reference must be hardlinked into the destination (shared inode, `nlink >= 2`)
//! rather than copied; a changed file must be transferred fresh (a distinct
//! inode). The check asserts oc's per-file hardlink decision matches upstream's
//! and that the destination trees are content-identical. The ssh transports are
//! skipped when no sshd answers on localhost:22.

use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Command, Output};

use crate::commands::validate::support;
use crate::commands::validate::transport::{DaemonHandle, Transport};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The `--link-dest` hardlink-parity check.
pub struct LinkDest;

/// Files present in the reference byte- and mtime-identical to the source: these
/// must be hardlinked from the reference, not copied.
const UNCHANGED: &[&str] = &["same.txt", "sub/also_same.txt"];

/// File whose source content differs from the reference: it must be copied
/// fresh, never hardlinked.
const CHANGED: &str = "changed.txt";

/// Shared backdated mtime (epoch seconds) applied to source and reference so the
/// quick-check treats the unchanged files as identical and hardlinks them.
const EPOCH: &str = "@1614830767";

impl Check for LinkDest {
    fn name(&self) -> &'static str {
        "link-dest"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("link-dest");
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

impl LinkDest {
    /// Run one transport cell: build the reference, pull with each client into a
    /// fresh destination using `--link-dest`, then compare content and per-file
    /// hardlink decisions against upstream.
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

        // Per-cell reference dir, canonicalized so `--link-dest` is absolute.
        let ref_dir = root.join(format!("ref-{label}"));
        if let Err(e) = build_reference(src, &ref_dir) {
            return CheckOutcome::skip(self.name(), label, format!("reference: {e}"));
        }
        let ref_dir = match ref_dir.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                return CheckOutcome::skip(self.name(), label, format!("canonicalize ref: {e}"));
            }
        };

        let oc_dst = root.join(format!("oc-{label}"));
        let up_dst = root.join(format!("up-{label}"));

        // One live upstream daemon shared by both client runs for this cell.
        let daemon = if transport == Transport::Daemon {
            match DaemonHandle::start(ctx.upstream, src, ctx.work) {
                Ok(handle) => Some(handle),
                Err(e) => return CheckOutcome::skip(self.name(), label, format!("daemon: {e}")),
            }
        } else {
            None
        };
        let daemon_url = daemon.as_ref().map(|d| d.module_url());

        let up = run_client(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            src,
            &up_dst,
            &ref_dir,
            daemon_url.as_deref(),
        );
        let up = match up {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), label, "upstream", other),
        };
        let _ = up;
        let oc = run_client(
            transport,
            ctx.oc,
            ctx.upstream,
            src,
            &oc_dst,
            &ref_dir,
            daemon_url.as_deref(),
        );
        let oc = match oc {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), label, "oc", other),
        };
        let _ = oc;
        drop(daemon);

        // Destinations must be byte-identical.
        if let Some(diff) = support::content_diff(&oc_dst, &up_dst) {
            if ctx.verbose {
                dump(label, &oc_dst, &up_dst, &ref_dir);
            }
            return CheckOutcome::fail(
                self.name(),
                label,
                format!("oc and upstream dests differ: {diff}"),
            );
        }

        // Per-file hardlink decisions must match upstream, and be non-vacuous.
        match evaluate(&oc_dst, &up_dst, &ref_dir) {
            Ok(()) => CheckOutcome::pass(self.name(), label),
            Err(reason) => {
                if ctx.verbose {
                    dump(label, &oc_dst, &up_dst, &ref_dir);
                }
                CheckOutcome::fail(self.name(), label, reason)
            }
        }
    }
}

/// Build one client rsync `Command` for `transport` and pull `src/` into a fresh
/// `dst/` with `--link-dest=<ref_dir>`. The destination is recreated empty first
/// so the reference is the only basis for hardlinking.
///
/// Flags are `-rlptgoD --link-dest=<ref> --numeric-ids`. Operand forms mirror
/// `transport::pull_into`: `local` copies from a path, `ssh-subprocess` uses
/// `-e ssh localhost:<src>`, `russh` an `ssh://` URL, `daemon` the module URL in
/// `daemon_url`. The sender is always `upstream` for the network transports.
fn run_client(
    transport: Transport,
    client: &Path,
    upstream: &Path,
    src: &Path,
    dst: &Path,
    ref_dir: &Path,
    daemon_url: Option<&str>,
) -> TaskResult<Output> {
    reset_dir(dst)?;
    let dst_arg = format!("{}/", dst.display());
    let link_dest = format!("--link-dest={}", ref_dir.display());
    let mut cmd = Command::new(client);
    cmd.arg("-rlptgoD").arg(&link_dest).arg("--numeric-ids");

    match transport {
        Transport::Local => {
            cmd.arg(format!("{}/", src.display())).arg(&dst_arg);
        }
        Transport::SshSubprocess => {
            cmd.arg(format!("--rsync-path={}", upstream.display()))
                .arg("-e")
                .arg("ssh -o BatchMode=yes -o StrictHostKeyChecking=no")
                .arg(format!("localhost:{}/", src.display()))
                .arg(&dst_arg);
        }
        Transport::Russh => {
            cmd.arg(format!("--rsync-path={}", upstream.display()))
                .arg(format!("ssh://localhost{}/", src.display()))
                .arg(&dst_arg);
        }
        Transport::Daemon => {
            let url = daemon_url
                .ok_or_else(|| TaskError::Validation("daemon transport without url".into()))?;
            cmd.arg(url).arg(&dst_arg);
        }
    }

    cmd.output()
        .map_err(|e| TaskError::Validation(format!("failed to spawn {cmd:?}: {e}")))
}

/// Per-file hardlink facts relative to the reference copy.
struct LinkFacts {
    /// Inode of the destination entry.
    dest_ino: u64,
    /// Inode of the matching reference entry.
    ref_ino: u64,
    /// Hard-link count of the destination entry.
    nlink: u64,
    /// True when destination and reference share inode and device (hardlinked).
    linked: bool,
}

/// Read hardlink facts for `rel` in `dest` against the same path in `ref_dir`.
fn link_facts(dest: &Path, ref_dir: &Path, rel: &str) -> Result<LinkFacts, String> {
    let d = dest
        .join(rel)
        .symlink_metadata()
        .map_err(|e| format!("stat {rel} in dest: {e}"))?;
    let r = ref_dir
        .join(rel)
        .symlink_metadata()
        .map_err(|e| format!("stat {rel} in reference: {e}"))?;
    Ok(LinkFacts {
        dest_ino: d.ino(),
        ref_ino: r.ino(),
        nlink: d.nlink(),
        linked: d.ino() == r.ino() && d.dev() == r.dev(),
    })
}

/// Assert oc's per-file hardlink decision matches upstream's: both hardlink the
/// unchanged files (shared inode, `nlink >= 2`) and both copy the changed file.
///
/// Non-vacuous: the unchanged files must actually be hardlinked in oc's
/// destination, so a client that silently copies everything fails here.
fn evaluate(oc_dst: &Path, up_dst: &Path, ref_dir: &Path) -> Result<(), String> {
    for rel in UNCHANGED {
        let oc = link_facts(oc_dst, ref_dir, rel)?;
        let up = link_facts(up_dst, ref_dir, rel)?;
        if !oc.linked {
            return Err(format!(
                "{rel}: oc did not hardlink to reference (dest ino {} vs ref ino {})",
                oc.dest_ino, oc.ref_ino
            ));
        }
        if oc.nlink < 2 {
            return Err(format!("{rel}: oc hardlink has nlink {} (< 2)", oc.nlink));
        }
        if oc.linked != up.linked {
            return Err(format!(
                "{rel}: hardlink decision differs (oc linked={} upstream linked={})",
                oc.linked, up.linked
            ));
        }
    }

    let oc = link_facts(oc_dst, ref_dir, CHANGED)?;
    let up = link_facts(up_dst, ref_dir, CHANGED)?;
    if oc.linked {
        return Err(format!(
            "{CHANGED}: oc hardlinked a changed file to the reference (ino {})",
            oc.dest_ino
        ));
    }
    if oc.linked != up.linked {
        return Err(format!(
            "{CHANGED}: hardlink decision differs (oc linked={} upstream linked={})",
            oc.linked, up.linked
        ));
    }
    Ok(())
}

/// Print each client's per-file inode / nlink versus the reference (verbose
/// failures), so a hardlink-decision divergence is legible.
fn dump(label: &str, oc_dst: &Path, up_dst: &Path, ref_dir: &Path) {
    for (who, dir) in [("oc", oc_dst), ("upstream", up_dst)] {
        for rel in UNCHANGED.iter().copied().chain(std::iter::once(CHANGED)) {
            match link_facts(dir, ref_dir, rel) {
                Ok(f) => eprintln!(
                    "[link-dest/{label}] {who} {rel}: dest_ino={} ref_ino={} nlink={} linked={}",
                    f.dest_ino, f.ref_ino, f.nlink, f.linked
                ),
                Err(e) => eprintln!("[link-dest/{label}] {who} {rel}: {e}"),
            }
        }
    }
}

/// Distinguish a genuine divergence from an unrunnable cell (e.g. ssh refused).
fn skip_or_fail(
    check: &'static str,
    label: &str,
    who: &str,
    result: TaskResult<Output>,
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

/// Recreate `dir` as an empty directory.
fn reset_dir(dir: &Path) -> TaskResult<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)
            .map_err(|e| TaskError::Validation(format!("remove {}: {e}", dir.display())))?;
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| TaskError::Validation(format!("create {}: {e}", dir.display())))
}

/// Build the source fixture: two files that will match the reference plus one
/// whose content will differ. Idempotent: removes any prior tree first. Mtimes
/// are backdated so the unchanged files quick-check as identical to the
/// reference and are therefore hardlinked.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;

    std::fs::write(src.join("same.txt"), b"same-content\n").map_err(|e| e.to_string())?;
    std::fs::write(sub.join("also_same.txt"), b"also-same-content\n").map_err(|e| e.to_string())?;
    // Longer, different content so the reference's older copy quick-checks as
    // changed and is copied fresh rather than hardlinked.
    std::fs::write(src.join("changed.txt"), b"changed-new-longer-content\n")
        .map_err(|e| e.to_string())?;

    backdate(src)
}

/// Build the per-cell reference: the two unchanged files copied byte- and
/// mtime-identical from the source, plus an older `changed.txt`. Idempotent.
fn build_reference(src: &Path, ref_dir: &Path) -> Result<(), String> {
    if ref_dir.exists() {
        std::fs::remove_dir_all(ref_dir).map_err(|e| e.to_string())?;
    }
    let sub = ref_dir.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;

    for rel in UNCHANGED {
        std::fs::copy(src.join(rel), ref_dir.join(rel)).map_err(|e| e.to_string())?;
    }
    // Older, shorter content: differs from the source so it is not hardlinked.
    std::fs::write(ref_dir.join(CHANGED), b"old\n").map_err(|e| e.to_string())?;

    // Match the source mtimes on the unchanged files so the quick-check hardlinks
    // them; the changed file's mtime is irrelevant (its size already differs).
    for rel in UNCHANGED {
        touch_epoch(&ref_dir.join(rel))?;
    }
    Ok(())
}

/// Backdate every entry under `root` to the shared epoch mtime.
fn backdate(root: &Path) -> Result<(), String> {
    for entry in support::rel_entries(root) {
        touch_epoch(&root.join(&entry))?;
    }
    Ok(())
}

/// Set `path`'s mtime to the shared epoch via GNU `touch -h -d @epoch`.
fn touch_epoch(path: &Path) -> Result<(), String> {
    support::capture("touch", &["-h", "-d", EPOCH, &path.to_string_lossy()])
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{CHANGED, UNCHANGED, evaluate};
    use std::fs;

    /// Seed `dir` from `ref_dir`: hardlink the unchanged files, copy the changed
    /// one to a fresh inode - the correct `--link-dest` outcome.
    fn seed_like_link_dest(dir: &std::path::Path, ref_dir: &std::path::Path) {
        fs::create_dir_all(dir.join("sub")).unwrap();
        for rel in UNCHANGED {
            fs::hard_link(ref_dir.join(rel), dir.join(rel)).unwrap();
        }
        fs::write(dir.join(CHANGED), b"fresh-copy").unwrap();
    }

    fn seed_reference(ref_dir: &std::path::Path) {
        fs::create_dir_all(ref_dir.join("sub")).unwrap();
        for rel in UNCHANGED {
            fs::write(ref_dir.join(rel), b"same").unwrap();
        }
        fs::write(ref_dir.join(CHANGED), b"old").unwrap();
    }

    #[test]
    fn evaluate_accepts_linked_unchanged_and_copied_changed() {
        let r = tempfile::tempdir().unwrap();
        let oc = tempfile::tempdir().unwrap();
        let up = tempfile::tempdir().unwrap();
        seed_reference(r.path());
        seed_like_link_dest(oc.path(), r.path());
        seed_like_link_dest(up.path(), r.path());
        assert!(evaluate(oc.path(), up.path(), r.path()).is_ok());
    }

    #[test]
    fn evaluate_rejects_hardlinked_changed_file() {
        let r = tempfile::tempdir().unwrap();
        let oc = tempfile::tempdir().unwrap();
        let up = tempfile::tempdir().unwrap();
        seed_reference(r.path());
        seed_like_link_dest(up.path(), r.path());
        // oc wrongly hardlinks the changed file to the reference.
        fs::create_dir_all(oc.path().join("sub")).unwrap();
        for rel in UNCHANGED {
            fs::hard_link(r.path().join(rel), oc.path().join(rel)).unwrap();
        }
        fs::hard_link(r.path().join(CHANGED), oc.path().join(CHANGED)).unwrap();
        let err = evaluate(oc.path(), up.path(), r.path()).unwrap_err();
        assert!(err.contains(CHANGED));
    }
}
