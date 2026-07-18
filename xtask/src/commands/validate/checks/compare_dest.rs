//! `--compare-dest` / `--copy-dest` parity: oc-rsync treats a reference
//! directory of already-present files exactly as upstream does.
//!
//! Both flags name a reference dir holding files that may already match the
//! source. They diverge on what happens to an *unchanged* file:
//!
//! - `--compare-dest` skips it entirely - the file is neither transferred nor
//!   written to the destination, because an identical copy already exists in the
//!   reference.
//! - `--copy-dest` copies it locally from the reference into the destination, so
//!   the destination ends up holding the full tree without transferring the
//!   unchanged bytes over the wire.
//!
//! Like the `--delete` and `--link-dest` checks this needs a pre-built reference
//! tree and an extra flag, so each client `Command` is built directly rather than
//! through `transport::pull_into`. For every cell a per-cell reference dir holds
//! `same.txt` and `sub/also_same.txt` byte- and mtime-identical to the source
//! (so the quick-check treats them as unchanged) plus `changed.txt` carrying
//! older, differing content. Both clients pull the source into their own fresh
//! empty destination with `--compare-dest`/`--copy-dest=<absolute ref dir>`.
//!
//! Upstream rsync is the ground truth: for each scenario oc's destination must be
//! content-identical to upstream's, and must additionally exhibit the scenario's
//! defining shape - only `changed.txt` present for `--compare-dest`, the whole
//! tree present for `--copy-dest`. The ssh transports are skipped when no sshd
//! answers on localhost:22.

use std::path::Path;
use std::process::{Command, Output};

use crate::commands::validate::support;
use crate::commands::validate::transport::{DaemonHandle, Transport};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The `--compare-dest` / `--copy-dest` reference-directory parity check.
pub struct CompareDest;

/// Files present in the reference byte- and mtime-identical to the source: the
/// quick-check treats them as unchanged relative to the reference.
const UNCHANGED: &[&str] = &["same.txt", "sub/also_same.txt"];

/// File whose source content differs from the reference: it is always
/// transferred fresh under both flags.
const CHANGED: &str = "changed.txt";

/// Shared backdated mtime (epoch seconds) applied to source and reference so the
/// quick-check treats the unchanged files as identical to the reference.
const EPOCH: &str = "@1614830767";

/// Which reference-directory flag a cell exercises.
#[derive(Clone, Copy)]
enum Scenario {
    /// `--compare-dest`: unchanged files are skipped, not written to the dest.
    Compare,
    /// `--copy-dest`: unchanged files are copied locally from the reference.
    Copy,
}

impl Scenario {
    /// Stable scenario label, also the flag's long name.
    fn label(self) -> &'static str {
        match self {
            Scenario::Compare => "compare-dest",
            Scenario::Copy => "copy-dest",
        }
    }

    /// Build the full `--<flag>=<ref>` argument for this scenario.
    fn flag(self, ref_dir: &Path) -> String {
        format!("--{}={}", self.label(), ref_dir.display())
    }
}

impl Check for CompareDest {
    fn name(&self) -> &'static str {
        "compare-dest"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("compare-dest");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let mut outcomes = Vec::with_capacity(ctx.transports.len() * 2);
        for &t in ctx.transports {
            outcomes.push(self.cell(ctx, t, &root, &src, Scenario::Compare));
            outcomes.push(self.cell(ctx, t, &root, &src, Scenario::Copy));
        }
        outcomes
    }
}

impl CompareDest {
    /// Run one (transport, scenario) cell: build the reference, pull with each
    /// client into a fresh destination using the scenario's flag, then compare
    /// content against upstream and assert the scenario's defining dest shape.
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        src: &Path,
        scenario: Scenario,
    ) -> CheckOutcome {
        let label = format!("{} {}", transport.label(), scenario.label());
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }

        let key = format!("{}-{}", transport.label(), scenario.label());

        // Per-cell reference dir, canonicalized so the flag path is absolute.
        let ref_dir = root.join(format!("ref-{key}"));
        if let Err(e) = build_reference(src, &ref_dir) {
            return CheckOutcome::skip(self.name(), label, format!("reference: {e}"));
        }
        let ref_dir = match ref_dir.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                return CheckOutcome::skip(self.name(), label, format!("canonicalize ref: {e}"));
            }
        };
        let ref_flag = scenario.flag(&ref_dir);

        let oc_dst = root.join(format!("oc-{key}"));
        let up_dst = root.join(format!("up-{key}"));

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
            &ref_flag,
            daemon_url.as_deref(),
        );
        let up = match up {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), &label, "upstream", other),
        };
        let _ = up;
        let oc = run_client(
            transport,
            ctx.oc,
            ctx.upstream,
            src,
            &oc_dst,
            &ref_flag,
            daemon_url.as_deref(),
        );
        let oc = match oc {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), &label, "oc", other),
        };
        let _ = oc;
        drop(daemon);

        // Destinations must be byte-identical regardless of scenario.
        if let Some(diff) = support::content_diff(&oc_dst, &up_dst) {
            if ctx.verbose {
                dump(&label, &oc_dst, &up_dst);
            }
            return CheckOutcome::fail(
                self.name(),
                label,
                format!("oc and upstream dests differ: {diff}"),
            );
        }

        // Scenario-defining shape must hold in oc's destination.
        let shape = match scenario {
            Scenario::Compare => evaluate_compare(&oc_dst),
            Scenario::Copy => evaluate_copy(&oc_dst, src),
        };
        match shape {
            Ok(()) => CheckOutcome::pass(self.name(), label),
            Err(reason) => {
                if ctx.verbose {
                    dump(&label, &oc_dst, &up_dst);
                }
                CheckOutcome::fail(self.name(), label, reason)
            }
        }
    }
}

/// Build one client rsync `Command` for `transport` and pull `src/` into a fresh
/// `dst/` with `ref_flag` (a full `--compare-dest=`/`--copy-dest=` argument). The
/// destination is recreated empty first so the reference is the only basis.
///
/// Flags are `-rlptgoD --numeric-ids <ref_flag>`. Operand forms mirror
/// `transport::pull_into`: `local` copies from a path, `ssh-subprocess` uses
/// `-e ssh localhost:<src>`, `russh` an `ssh://` URL, `daemon` the module URL in
/// `daemon_url`. The sender is always `upstream` for the network transports.
fn run_client(
    transport: Transport,
    client: &Path,
    upstream: &Path,
    src: &Path,
    dst: &Path,
    ref_flag: &str,
    daemon_url: Option<&str>,
) -> TaskResult<Output> {
    reset_dir(dst)?;
    let dst_arg = format!("{}/", dst.display());
    let mut cmd = Command::new(client);
    cmd.arg("-rlptgoD").arg("--numeric-ids").arg(ref_flag);

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

/// `--compare-dest` shape: the changed file must land in the destination while
/// every unchanged file is skipped (matched against the reference, never
/// written). Non-vacuous: a client that copies everything trips the second loop,
/// one that transfers nothing trips the first check.
fn evaluate_compare(dst: &Path) -> Result<(), String> {
    if !dst.join(CHANGED).exists() {
        return Err(format!("{CHANGED} was not written to the dest"));
    }
    for rel in UNCHANGED {
        if dst.join(rel).exists() {
            return Err(format!(
                "{rel} was written to the dest despite matching the reference"
            ));
        }
    }
    Ok(())
}

/// `--copy-dest` shape: the destination must hold the full source tree with
/// correct content - the unchanged files copied from the reference plus the
/// transferred `changed.txt`. Equality with the source is non-vacuous: a missing
/// or wrong-content file yields a diff.
fn evaluate_copy(dst: &Path, src: &Path) -> Result<(), String> {
    match support::content_diff(dst, src) {
        Some(diff) => Err(format!("copy-dest tree differs from source: {diff}")),
        None => Ok(()),
    }
}

/// Print each destination's entry set (verbose failures) so a shape or content
/// divergence is legible.
fn dump(label: &str, oc_dst: &Path, up_dst: &Path) {
    eprintln!(
        "[compare-dest/{label}] oc entries: {:?}",
        support::rel_entries(oc_dst)
    );
    eprintln!(
        "[compare-dest/{label}] upstream entries: {:?}",
        support::rel_entries(up_dst)
    );
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
/// reference (matched by both flags).
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;

    std::fs::write(src.join("same.txt"), b"same-content\n").map_err(|e| e.to_string())?;
    std::fs::write(sub.join("also_same.txt"), b"also-same-content\n").map_err(|e| e.to_string())?;
    // Longer, different content so the reference's older copy quick-checks as
    // changed and is transferred fresh under both flags.
    std::fs::write(src.join(CHANGED), b"changed-new-longer-content\n")
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
    // Older, shorter content: differs from the source so it is transferred.
    std::fs::write(ref_dir.join(CHANGED), b"old\n").map_err(|e| e.to_string())?;

    // Match the source mtimes on the unchanged files so the quick-check treats
    // them as unchanged; the changed file's mtime is irrelevant (size differs).
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
    use super::{CHANGED, UNCHANGED, evaluate_compare, evaluate_copy};
    use std::fs;
    use std::path::Path;

    /// Seed a full source tree: both unchanged files plus the changed file.
    fn seed_full(root: &Path) {
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join(UNCHANGED[0]), b"same-content\n").unwrap();
        fs::write(root.join(UNCHANGED[1]), b"also-same-content\n").unwrap();
        fs::write(root.join(CHANGED), b"changed-new-longer-content\n").unwrap();
    }

    #[test]
    fn compare_accepts_only_changed_present() {
        // --compare-dest correct outcome: only the changed file was written.
        let dst = tempfile::tempdir().unwrap();
        fs::write(dst.path().join(CHANGED), b"changed-new-longer-content\n").unwrap();
        assert!(evaluate_compare(dst.path()).is_ok());
    }

    #[test]
    fn compare_rejects_written_unchanged_file() {
        // A client that copied an unchanged file breaks --compare-dest semantics.
        let dst = tempfile::tempdir().unwrap();
        fs::create_dir_all(dst.path().join("sub")).unwrap();
        fs::write(dst.path().join(CHANGED), b"changed-new-longer-content\n").unwrap();
        fs::write(dst.path().join(UNCHANGED[0]), b"same-content\n").unwrap();
        let err = evaluate_compare(dst.path()).unwrap_err();
        assert!(err.contains(UNCHANGED[0]));
    }

    #[test]
    fn compare_rejects_missing_changed_file() {
        let dst = tempfile::tempdir().unwrap();
        let err = evaluate_compare(dst.path()).unwrap_err();
        assert!(err.contains(CHANGED));
    }

    #[test]
    fn copy_accepts_full_tree_matching_source() {
        // --copy-dest correct outcome: the whole source tree is present.
        let (src, dst) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        seed_full(src.path());
        seed_full(dst.path());
        assert!(evaluate_copy(dst.path(), src.path()).is_ok());
    }

    #[test]
    fn copy_rejects_missing_unchanged_file() {
        let (src, dst) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        seed_full(src.path());
        // Destination is missing the unchanged files - only the changed one landed.
        fs::write(dst.path().join(CHANGED), b"changed-new-longer-content\n").unwrap();
        assert!(evaluate_copy(dst.path(), src.path()).is_err());
    }
}
