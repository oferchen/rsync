//! `--modify-window=N` quick-check tolerance parity between oc-rsync and upstream.
//!
//! Rsync's quick-check skips a file whose size and modification time both match
//! the destination. `--modify-window=N` widens the mtime comparison so two times
//! within `N` seconds count as equal (`same_time()` in util1.c). This check
//! pre-seeds a destination whose file has the *same size* but *different content*
//! and an mtime one second past the source's, then transfers with
//! `--modify-window=2` over every transport. Because the mtimes fall inside the
//! window and the sizes match, the quick-check must skip the file: oc-rsync's
//! destination must equal upstream's and both must still hold the seeded
//! (differing) content. A control run without the window - where the one-second
//! mtime gap forces a transfer - anchors the non-vacuous claim that the option
//! actually changed the decision.
//!
//! Like `transfer_conditions`, every scenario depends on a pre-seeded
//! destination, so it cannot use `pull_into` (which wipes the destination). It
//! seeds both destinations identically and runs the client directly without
//! resetting the seeded tree.
//!
//! upstream: util1.c:1477 `same_time()` (returns equal when `|f1-f2| <=
//! modify_window`); generator.c:1722 applies it in the unchanged-file path.

use std::path::Path;
use std::process::{Command, Output};

use crate::commands::validate::support;
use crate::commands::validate::transport::{DaemonHandle, Transport};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The `--modify-window` quick-check tolerance parity check.
pub struct ModifyWindow;

/// The single file exercised: same size in source and seed, differing content.
const FILE: &str = "data.txt";
/// Source bytes for [`FILE`].
const SRC_BODY: &[u8] = b"modify-window-source-body\n";

/// Source mtime (2021-03-04). Backdated so the quick-check does not re-transfer
/// on mtime alone in the windowed run.
const SRC_MTIME: &str = "@1614830767";
/// Destination seed mtime: exactly one second past the source. Inside the
/// window (2s) the quick-check treats it as equal; without the window the gap
/// forces a transfer.
const SEED_MTIME: &str = "@1614830768";

/// The modify-window width, in seconds. Two exceeds the one-second seed gap.
const WINDOW: &str = "--modify-window=2";

/// Shared metadata-preserving base for both the windowed and control runs.
const BASE: [&str; 2] = ["-rlptgoD", "--numeric-ids"];

impl Check for ModifyWindow {
    fn name(&self) -> &'static str {
        "modify-window"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("modify-window");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        // Control probe (host-local, transport-independent): confirm the seeded
        // one-second mtime gap really forces a transfer without the window, so
        // the windowed skip below is a genuine behavior change, not a no-op.
        if let Err(e) = assert_control_transfers(ctx.upstream, &src, &root) {
            return vec![CheckOutcome::skip(self.name(), "control", e)];
        }
        ctx.transports
            .iter()
            .map(|&t| self.cell(ctx, t, &root, &src))
            .collect()
    }
}

impl ModifyWindow {
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

        let oc_dst = root.join(format!("oc-{label}"));
        let up_dst = root.join(format!("up-{label}"));
        if let Err(e) = seed(&oc_dst).and_then(|()| seed(&up_dst)) {
            return CheckOutcome::skip(self.name(), label, format!("seed: {e}"));
        }
        // Non-vacuous precondition: the seed must differ from the source in
        // content while matching its size, or the window would have nothing to
        // skip past.
        if let Err(e) = precondition(&oc_dst) {
            return CheckOutcome::fail(self.name(), label, e);
        }

        let daemon = match transport {
            Transport::Daemon => match DaemonHandle::start(ctx.upstream, src, ctx.work) {
                Ok(handle) => Some(handle),
                Err(e) => return CheckOutcome::skip(self.name(), label, format!("daemon: {e}")),
            },
            _ => None,
        };
        let module_url = daemon.as_ref().map(|d| d.module_url());
        let flags = windowed_flags();

        match run_client(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            src,
            &up_dst,
            &flags,
            module_url.as_deref(),
        ) {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), label, "upstream", other),
        }
        match run_client(
            transport,
            ctx.oc,
            ctx.upstream,
            src,
            &oc_dst,
            &flags,
            module_url.as_deref(),
        ) {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), label, "oc", other),
        }
        drop(daemon);

        // The window must have skipped the file: both destinations still hold
        // the seeded content, which differs from the source.
        for (who, dst) in [("oc", &oc_dst), ("upstream", &up_dst)] {
            if std::fs::read(dst.join(FILE)).ok().as_deref() == Some(SRC_BODY) {
                return CheckOutcome::fail(
                    self.name(),
                    label,
                    format!("{who} overwrote {FILE} despite --modify-window"),
                );
            }
        }
        match support::content_diff(&oc_dst, &up_dst) {
            Some(diff) => CheckOutcome::fail(self.name(), label, diff),
            None => CheckOutcome::pass(self.name(), label),
        }
    }
}

/// The windowed flag set: shared base plus `--modify-window=2`.
fn windowed_flags() -> Vec<String> {
    BASE.iter()
        .map(|s| s.to_string())
        .chain(std::iter::once(WINDOW.to_string()))
        .collect()
}

/// Verify the seeded one-second mtime gap forces a transfer *without* the
/// window: a local run with only [`BASE`] must overwrite the seed with the
/// source content. Otherwise the windowed skip would be indistinguishable from
/// a quick-check that skipped for some unrelated reason.
fn assert_control_transfers(upstream: &Path, src: &Path, root: &Path) -> Result<(), String> {
    let dst = root.join("control");
    seed(&dst)?;
    let flags: Vec<String> = BASE.iter().map(|s| s.to_string()).collect();
    let out = run_client(
        Transport::Local,
        upstream,
        upstream,
        src,
        &dst,
        &flags,
        None,
    )
    .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "control run exited {}",
            out.status.code().unwrap_or(-1)
        ));
    }
    if std::fs::read(dst.join(FILE)).ok().as_deref() != Some(SRC_BODY) {
        return Err("control run did not overwrite seed without --modify-window".into());
    }
    Ok(())
}

/// Build one client rsync command for `transport` and run it into the
/// already-seeded `dst` without resetting it. Mirrors the operand forms of
/// `transport::pull_into`, but leaves the seeded tree in place.
fn run_client(
    transport: Transport,
    client: &Path,
    upstream: &Path,
    src: &Path,
    dst: &Path,
    flags: &[String],
    module_url: Option<&str>,
) -> TaskResult<Output> {
    let dst_arg = format!("{}/", dst.display());
    let mut cmd = Command::new(client);
    cmd.args(flags);
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
            let url = module_url
                .ok_or_else(|| TaskError::Validation("daemon transport without url".into()))?;
            cmd.arg(url).arg(&dst_arg);
        }
    }
    cmd.output()
        .map_err(|e| TaskError::Validation(format!("spawn rsync: {e}")))
}

/// (Re)seed `dst` with a same-size, differing-content file whose mtime is one
/// second past the source's.
fn seed(dst: &Path) -> Result<(), String> {
    if dst.exists() {
        std::fs::remove_dir_all(dst).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    std::fs::write(dst.join(FILE), differing_same_len(SRC_BODY)).map_err(|e| e.to_string())?;
    support::capture(
        "touch",
        &["-h", "-d", SEED_MTIME, &dst.join(FILE).to_string_lossy()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Assert the seeded precondition holds: same size as the source, different
/// content. A false precondition is a seeding bug, not a divergence.
fn precondition(dst: &Path) -> Result<(), String> {
    let seed = std::fs::read(dst.join(FILE)).map_err(|e| e.to_string())?;
    if seed.len() != SRC_BODY.len() {
        return Err("seed size differs from source".into());
    }
    if seed == SRC_BODY {
        return Err("seed content does not differ from source".into());
    }
    Ok(())
}

/// A buffer of the same length as `src` but differing in every byte, so the
/// seeded destination matches the source's size while differing in content.
fn differing_same_len(src: &[u8]) -> Vec<u8> {
    src.iter().map(|b| b.wrapping_add(1)).collect()
}

/// Build the single-file source, backdated to [`SRC_MTIME`]. Idempotent.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(src).map_err(|e| e.to_string())?;
    std::fs::write(src.join(FILE), SRC_BODY).map_err(|e| e.to_string())?;
    support::capture(
        "touch",
        &["-h", "-d", SRC_MTIME, &src.join(FILE).to_string_lossy()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn differing_same_len_keeps_length_and_changes_every_byte() {
        let out = differing_same_len(SRC_BODY);
        assert_eq!(out.len(), SRC_BODY.len());
        assert_ne!(out.as_slice(), SRC_BODY);
        for (a, b) in SRC_BODY.iter().zip(out.iter()) {
            assert_ne!(a, b);
        }
    }

    #[test]
    fn seed_mtime_is_one_second_past_source() {
        // The gap must be positive and smaller than the window; encode both.
        let src: i64 = SRC_MTIME.trim_start_matches('@').parse().unwrap();
        let seed: i64 = SEED_MTIME.trim_start_matches('@').parse().unwrap();
        assert_eq!(seed - src, 1);
        assert_eq!(WINDOW, "--modify-window=2");
    }

    #[test]
    fn windowed_flags_carry_base_then_window() {
        let flags = windowed_flags();
        assert_eq!(flags[0], "-rlptgoD");
        assert_eq!(flags.last().unwrap(), WINDOW);
    }

    #[test]
    fn precondition_rejects_a_seed_equal_to_the_source() {
        let tmp = tempfile::tempdir().unwrap();
        let dst = tmp.path().join("dst");
        std::fs::create_dir_all(&dst).unwrap();
        // Same-size, differing content passes; identical content is rejected as a
        // vacuous seed (no window skip to observe).
        std::fs::write(dst.join(FILE), differing_same_len(SRC_BODY)).unwrap();
        assert!(precondition(&dst).is_ok());
        std::fs::write(dst.join(FILE), SRC_BODY).unwrap();
        assert!(precondition(&dst).is_err());
    }
}
