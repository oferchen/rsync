//! Duplicate source-operand parity between oc-rsync and upstream.
//!
//! A user may name the same source more than once on the command line (via shell
//! variables or wildcard expansion). Rsync's `clean_flist()` sorts the file list
//! and drops the duplicates so each name is generated and updated exactly once,
//! rather than racing two copies of the same name through the pipeline. This
//! check names one source directory's contents several times in a single
//! transfer over every transport and asserts oc-rsync reaches the exact same
//! destination upstream does - one clean copy - with a successful exit, plus a
//! non-vacuous guard that the destination equals the source (no doubled or
//! corrupted entry survived the de-duplication).
//!
//! It builds the transfer commands directly rather than via `pull_into`, which
//! appends a single source operand and so cannot list the source repeatedly.
//!
//! upstream: flist.c `clean_flist()` (referenced by `testsuite/duplicates.test`),
//! which removes duplicate names after sorting the file list.

use std::path::Path;
use std::process::{Command, Output};

use crate::commands::validate::support;
use crate::commands::validate::transport::{DaemonHandle, Transport};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The duplicate source-operand parity check.
pub struct Duplicates;

/// How many times the same source is named in the transfer command.
const REPEATS: usize = 4;

/// Preserve metadata; `--numeric-ids` keeps owner comparison host-stable.
const FLAGS: &[&str] = &["-rlptgoD", "--numeric-ids"];

impl Check for Duplicates {
    fn name(&self) -> &'static str {
        "duplicates"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("duplicates");
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

impl Duplicates {
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

        let daemon = match transport {
            Transport::Daemon => match DaemonHandle::start(ctx.upstream, src, ctx.work) {
                Ok(handle) => Some(handle),
                Err(e) => return CheckOutcome::skip(self.name(), label, format!("daemon: {e}")),
            },
            _ => None,
        };
        let module_url = daemon.as_ref().map(|d| d.module_url());
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        let up_dst = root.join(format!("up-{label}"));
        let oc_dst = root.join(format!("oc-{label}"));

        let up_transport = transport.for_upstream();
        let up_operand = source_operand(up_transport, src, module_url.as_deref());
        match transfer(
            ctx.upstream,
            ctx.upstream,
            up_transport,
            &up_operand,
            &up_dst,
            &flags,
        ) {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), label, "upstream", other),
        }
        let oc_operand = source_operand(transport, src, module_url.as_deref());
        match transfer(
            ctx.oc,
            ctx.upstream,
            transport,
            &oc_operand,
            &oc_dst,
            &flags,
        ) {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), label, "oc", other),
        }
        drop(daemon);

        // Non-vacuous: de-duplication must yield exactly the source tree in both
        // destinations - no doubled or corrupted entry survived.
        if let Some(diff) = support::content_diff(&oc_dst, src) {
            return CheckOutcome::fail(self.name(), label, format!("oc dest != source: {diff}"));
        }
        if let Some(diff) = support::content_diff(&up_dst, src) {
            return CheckOutcome::fail(
                self.name(),
                label,
                format!("upstream dest != source: {diff}"),
            );
        }
        match support::content_diff(&oc_dst, &up_dst) {
            Some(diff) => CheckOutcome::fail(self.name(), label, diff),
            None => CheckOutcome::pass(self.name(), label),
        }
    }
}

/// The single source-operand form for `transport`; the caller pushes [`REPEATS`]
/// copies of it onto the command to name the same source repeatedly.
fn source_operand(transport: Transport, src: &Path, module_url: Option<&str>) -> String {
    match transport {
        Transport::Local => format!("{}/", src.display()),
        Transport::SshSubprocess => format!("localhost:{}/", src.display()),
        Transport::Russh => format!("ssh://localhost{}/", src.display()),
        Transport::Daemon => module_url.unwrap_or("").to_string(),
    }
}

/// Reset `dst`, append [`REPEATS`] copies of the source operand plus the
/// destination, and run the transfer capturing output.
fn transfer(
    client: &Path,
    upstream: &Path,
    transport: Transport,
    operand: &str,
    dst: &Path,
    flags: &[String],
) -> TaskResult<Output> {
    reset_dir(dst)?;
    let dst_arg = format!("{}/", dst.display());
    let mut cmd = Command::new(client);
    cmd.args(flags);
    if !matches!(transport, Transport::Local | Transport::Daemon) {
        cmd.arg(format!("--rsync-path={}", upstream.display()));
    }
    if matches!(transport, Transport::SshSubprocess) {
        cmd.arg("-e")
            .arg("ssh -o BatchMode=yes -o StrictHostKeyChecking=no");
    }
    for _ in 0..REPEATS {
        cmd.arg(operand);
    }
    cmd.arg(&dst_arg);
    cmd.output()
        .map_err(|e| TaskError::Validation(format!("spawn rsync: {e}")))
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

/// Build the source tree. Idempotent: removes any prior tree first. A couple of
/// files plus a nested file, all mtime-backdated so the quick-check is
/// deterministic. Distinct names let the content compare catch any accidental
/// duplication.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;
    std::fs::write(src.join("name1"), b"the first file\n").map_err(|e| e.to_string())?;
    std::fs::write(src.join("name2"), b"the second file\n").map_err(|e| e.to_string())?;
    std::fs::write(sub.join("name3"), b"the nested file\n").map_err(|e| e.to_string())?;
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
    use super::*;
    use std::path::Path;

    #[test]
    fn source_operand_carries_trailing_slash_per_transport() {
        let src = Path::new("/work/duplicates/src");
        assert_eq!(
            source_operand(Transport::Local, src, None),
            "/work/duplicates/src/"
        );
        assert_eq!(
            source_operand(Transport::SshSubprocess, src, None),
            "localhost:/work/duplicates/src/"
        );
        assert_eq!(
            source_operand(Transport::Daemon, src, Some("rsync://127.0.0.1:9/m/")),
            "rsync://127.0.0.1:9/m/"
        );
    }

    #[test]
    fn repeats_yields_multiple_identical_operands() {
        // The transfer names the source [`REPEATS`] times; mirror that expansion
        // and assert it produces more than one copy of the same operand - the
        // duplicate condition the check exercises.
        let operand = source_operand(Transport::Local, Path::new("/s"), None);
        let args: Vec<String> = (0..REPEATS).map(|_| operand.clone()).collect();
        assert!(args.len() > 1);
        assert!(args.iter().all(|a| a == &operand));
    }
}
