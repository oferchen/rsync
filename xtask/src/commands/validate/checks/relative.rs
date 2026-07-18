//! `-R` / `--relative` path-recreation parity between oc-rsync and upstream.
//!
//! With `-R`, rsync recreates the source-operand path components under the
//! destination; the `/./` dot-root pivot in the operand marks where recreation
//! begins. `--no-implied-dirs` suppresses recreating the implied leading dirs'
//! attributes. This check transfers a source operand carrying a `/./` pivot
//! (`<src>/./a/b/c/`) over every transport in `ctx.transports`, in two scenarios
//! (`implied`, `no-implied`), and asserts oc-rsync's destination tree is byte-
//! and structure-identical to upstream's and that the `-R` subpath was actually
//! recreated. It builds the transfer commands directly rather than via
//! `pull_into`, which would append a trailing `/` and its own operand and so
//! could not carry the source-preserving `/./` path.

use std::path::Path;
use std::process::{Command, Output};

use crate::commands::validate::support;
use crate::commands::validate::transport::{DaemonHandle, Transport};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The `-R` path-recreation parity check.
pub struct Relative;

/// The subpath after the `/./` pivot; `-R` recreates exactly this under dest.
const SUBPATH: &str = "a/b/c";

/// A leaf that must exist under the destination once `-R` recreates the path.
/// Asserting it exists keeps the content compare non-vacuous.
const RECREATED_LEAF: &str = "a/b/c/leaf.txt";

/// One `-R` scenario: a report name and the exact rsync flag set.
struct Scenario {
    /// Short scenario name used in the cell label.
    name: &'static str,
    /// Complete rsync flag set for the transfer.
    flags: &'static [&'static str],
}

/// The two `-R` variants exercised per transport.
const SCENARIOS: [Scenario; 2] = [
    Scenario {
        name: "implied",
        flags: &["-rlptgoD", "-R", "--numeric-ids"],
    },
    Scenario {
        name: "no-implied",
        flags: &["-rlptgoD", "-R", "--no-implied-dirs", "--numeric-ids"],
    },
];

impl Check for Relative {
    fn name(&self) -> &'static str {
        "relative"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("relative");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }

        let mut outcomes = Vec::new();
        for &transport in ctx.transports {
            for scenario in &SCENARIOS {
                outcomes.push(self.cell(ctx, transport, scenario, &root, &src));
            }
        }
        outcomes
    }
}

impl Relative {
    /// Run one (transport, scenario) cell for both clients and compare results.
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        scenario: &Scenario,
        root: &Path,
        src: &Path,
    ) -> CheckOutcome {
        let label = format!("{} {}", transport.label(), scenario.name);
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }

        // For the daemon cell the sender is one upstream `--daemon` shared by
        // both runs; keep it alive until both transfers finish.
        let daemon = match transport {
            Transport::Daemon => match DaemonHandle::start(ctx.upstream, src, ctx.work) {
                Ok(handle) => Some(handle),
                Err(e) => return CheckOutcome::skip(self.name(), label, format!("daemon: {e}")),
            },
            _ => None,
        };
        let module_url = daemon.as_ref().map(|d| d.module_url());

        let flags: Vec<String> = scenario.flags.iter().map(|s| s.to_string()).collect();
        let up_dst = root.join(format!("up-{}-{}", transport.label(), scenario.name));
        let oc_dst = root.join(format!("oc-{}-{}", transport.label(), scenario.name));

        // Upstream reference runs over the transport upstream can speak (russh
        // maps to the ssh subprocess); oc runs over the transport under test.
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
            other => return skip_or_fail(self.name(), &label, "upstream", other),
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
            other => return skip_or_fail(self.name(), &label, "oc", other),
        }

        drop(daemon);

        // Non-vacuous: the `-R` path must actually have been recreated.
        if !oc_dst.join(RECREATED_LEAF).exists() {
            if ctx.verbose {
                dump_entries(&oc_dst, &up_dst);
            }
            return CheckOutcome::fail(
                self.name(),
                label,
                format!("-R did not recreate {RECREATED_LEAF}"),
            );
        }
        if let Some(diff) = support::content_diff(&oc_dst, &up_dst) {
            if ctx.verbose {
                dump_entries(&oc_dst, &up_dst);
            }
            return CheckOutcome::fail(self.name(), label, diff);
        }
        CheckOutcome::pass(self.name(), label)
    }
}

/// Build the source operand carrying the `/./` dot-root pivot for `transport`.
///
/// The pivot marks where `-R` begins recreating path components, so everything
/// after `/./` (here [`SUBPATH`]) is rebuilt under the destination. Only the
/// daemon form needs `module_url` (its module already ends in `/m/`).
fn source_operand(transport: Transport, src: &Path, module_url: Option<&str>) -> String {
    match transport {
        Transport::Local => format!("{}/./{SUBPATH}/", src.display()),
        Transport::SshSubprocess => format!("localhost:{}/./{SUBPATH}/", src.display()),
        Transport::Russh => format!("ssh://localhost{}/./{SUBPATH}/", src.display()),
        Transport::Daemon => format!("{}./{SUBPATH}/", module_url.unwrap_or("")),
    }
}

/// Reset `dst`, build the per-transport command, and run it capturing output.
fn transfer(
    client: &Path,
    upstream: &Path,
    transport: Transport,
    operand: &str,
    dst: &Path,
    flags: &[String],
) -> TaskResult<Output> {
    reset_dir(dst)?;
    let dst_arg = dst.display().to_string();
    let mut cmd = build_command(client, upstream, transport, operand, &dst_arg, flags);
    cmd.output()
        .map_err(|e| TaskError::Validation(format!("spawn rsync: {e}")))
}

/// Assemble the rsync command for `transport` with the source-preserving
/// `operand` (not `pull_into`, whose trailing-slash operand would drop `-R`'s
/// path). Network cells set the upstream sender via `--rsync-path`.
fn build_command(
    client: &Path,
    upstream: &Path,
    transport: Transport,
    operand: &str,
    dst_arg: &str,
    flags: &[String],
) -> Command {
    let mut cmd = Command::new(client);
    cmd.args(flags);
    match transport {
        Transport::Local | Transport::Daemon => {
            cmd.arg(operand).arg(dst_arg);
        }
        Transport::SshSubprocess => {
            cmd.arg(format!("--rsync-path={}", upstream.display()))
                .arg("-e")
                .arg("ssh -o BatchMode=yes -o StrictHostKeyChecking=no")
                .arg(operand)
                .arg(dst_arg);
        }
        Transport::Russh => {
            cmd.arg(format!("--rsync-path={}", upstream.display()))
                .arg(operand)
                .arg(dst_arg);
        }
    }
    cmd
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

/// Print both destinations' entry lists for a verbose-mode mismatch.
fn dump_entries(oc: &Path, up: &Path) {
    eprintln!(
        "[relative] oc {} -> {:?}",
        oc.display(),
        support::rel_entries(oc)
    );
    eprintln!(
        "[relative] up {} -> {:?}",
        up.display(),
        support::rel_entries(up)
    );
}

/// Build the nested `-R` fixture. Idempotent: removes any prior tree first.
///
/// Lays out `a/b/c/leaf.txt` and `a/b/other.txt`, then backdates every entry's
/// mtime with GNU `touch` so the quick-check makes deterministic decisions.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let c = src.join("a").join("b").join("c");
    std::fs::create_dir_all(&c).map_err(|e| e.to_string())?;

    std::fs::write(c.join("leaf.txt"), b"leaf\n").map_err(|e| e.to_string())?;
    std::fs::write(src.join("a").join("b").join("other.txt"), b"other\n")
        .map_err(|e| e.to_string())?;

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
    use super::{SUBPATH, source_operand};
    use crate::commands::validate::transport::Transport;
    use std::path::Path;

    #[test]
    fn dot_root_pivot_is_placed_per_transport_form() {
        let src = Path::new("/work/relative/src");
        assert_eq!(
            source_operand(Transport::Local, src, None),
            "/work/relative/src/./a/b/c/"
        );
        assert_eq!(
            source_operand(Transport::SshSubprocess, src, None),
            "localhost:/work/relative/src/./a/b/c/"
        );
        assert_eq!(
            source_operand(Transport::Russh, src, None),
            "ssh://localhost/work/relative/src/./a/b/c/"
        );
        assert_eq!(
            source_operand(Transport::Daemon, src, Some("rsync://127.0.0.1:9/m/")),
            "rsync://127.0.0.1:9/m/./a/b/c/"
        );
    }

    #[test]
    fn daemon_operand_appends_pivot_after_the_module() {
        // The module URL already ends in `/m/`; the pivot recreates SUBPATH.
        let url = "rsync://127.0.0.1:873/m/";
        let operand = source_operand(Transport::Daemon, Path::new("/unused"), Some(url));
        assert!(operand.starts_with(url));
        assert!(operand.ends_with(&format!("./{SUBPATH}/")));
    }
}
