//! `--backup` parity: oc-rsync preserves overwritten destination files exactly
//! as upstream does, both with the default `~` suffix and with a `--backup-dir`.
//!
//! Like `--delete`, `--backup` only does anything when the destination already
//! holds files the transfer overwrites, so this check cannot use
//! `transport::pull_into` (which recreates the destination empty). Instead each
//! cell owns a per-side directory (`oc-<transport>-<scenario>` /
//! `up-<transport>-<scenario>`) holding a `dst/` subtree, pre-seeds `dst/` with
//! OLD versions of the source files, then transfers the NEW source over them so
//! `--backup` keeps the old copies. Giving each side its own cell directory
//! isolates the two runs: a relative `--backup-dir=../bak` resolves against the
//! receiver's destination directory (upstream main.c:1178 `change_dir`), so it
//! lands in that side's own `bak/` rather than a shared one. oc and upstream are
//! seeded identically, so only the client under test varies. Upstream rsync is
//! the ground truth for both the refreshed files and their backups. The ssh
//! transports are skipped when no sshd answers on localhost:22.

use std::path::Path;
use std::process::{Command, Output};

use crate::commands::validate::support;
use crate::commands::validate::transport::{DaemonHandle, Transport};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The `--backup` overwrite-preservation parity check.
pub struct Backup;

/// NEW source payloads (what overwrites the seeded destination).
const NEW_F1: &[u8] = b"new f1 payload\n";
const NEW_F2: &[u8] = b"new f2 payload, longer than old\n";
/// OLD destination payloads (what `--backup` must preserve). Their lengths
/// differ from the NEW payloads so the quick-check always re-transfers,
/// firing `--backup` regardless of mtime (see [`forces_retransfer`]).
const OLD_F1: &[u8] = b"old-1\n";
const OLD_F2: &[u8] = b"old-2\n";

/// One `--backup` scenario: a flag set plus the destination-relative path where
/// the backup of `f1.txt` is expected to land.
struct Scenario {
    /// Short name used in the cell label (e.g. `suffix`).
    name: &'static str,
    /// Complete rsync flag set for the transfer.
    flags: &'static [&'static str],
    /// Path of `f1.txt`'s backup relative to the per-side cell directory.
    backup_rel: &'static str,
}

/// The two scenarios exercised for every transport.
const SCENARIOS: &[Scenario] = &[
    // Default `~` suffix: the backup lives beside the file inside `dst/`.
    Scenario {
        name: "suffix",
        flags: &["-rlptgoD", "--backup", "--numeric-ids"],
        backup_rel: "dst/f1.txt~",
    },
    // `--backup-dir=../bak`: old versions collect under a sibling `bak/` tree.
    Scenario {
        name: "backup-dir",
        flags: &[
            "-rlptgoD",
            "--backup",
            "--backup-dir=../bak",
            "--numeric-ids",
        ],
        backup_rel: "bak/f1.txt",
    },
];

impl Check for Backup {
    fn name(&self) -> &'static str {
        "backup"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("backup");
        let src = root.join("src");
        // Fixture invariant: OLD and NEW must differ in length so every cell
        // actually overwrites (and thus backs up) the seeded files.
        if !forces_retransfer(OLD_F1, NEW_F1) || !forces_retransfer(OLD_F2, NEW_F2) {
            return vec![CheckOutcome::skip(
                self.name(),
                "fixture",
                "OLD/NEW payloads must differ in length",
            )];
        }
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let mut outcomes = Vec::new();
        for &transport in ctx.transports {
            for scenario in SCENARIOS {
                outcomes.push(self.cell(ctx, transport, &root, &src, scenario));
            }
        }
        outcomes
    }
}

impl Backup {
    /// Run one (transport, scenario) cell: seed both destinations with the OLD
    /// files, transfer the NEW source over each, and compare the resulting trees
    /// (refreshed files plus their backups) against upstream.
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        src: &Path,
        scenario: &Scenario,
    ) -> CheckOutcome {
        let label = format!("{} {}", transport.label(), scenario.name);
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }

        let oc_cell = root.join(format!("oc-{}-{}", transport.label(), scenario.name));
        let up_cell = root.join(format!("up-{}-{}", transport.label(), scenario.name));

        // Seed both destinations identically before their respective transfers.
        if let Err(e) = seed_dest(&oc_cell) {
            return CheckOutcome::skip(self.name(), label, format!("seed oc dest: {e}"));
        }
        if let Err(e) = seed_dest(&up_cell) {
            return CheckOutcome::skip(self.name(), label, format!("seed upstream dest: {e}"));
        }

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
            &up_cell.join("dst"),
            scenario.flags,
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
            &oc_cell.join("dst"),
            scenario.flags,
            daemon_url.as_deref(),
        );
        let oc = match oc {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), &label, "oc", other),
        };
        let _ = oc;
        drop(daemon);

        // Non-vacuous guard: the backup must actually exist in oc's tree and
        // hold the OLD content, otherwise a no-op transfer would pass trivially.
        match std::fs::read(oc_cell.join(scenario.backup_rel)) {
            Ok(bytes) if bytes == OLD_F1 => {}
            Ok(_) => {
                return CheckOutcome::fail(
                    self.name(),
                    label,
                    format!(
                        "oc backup {} does not hold the old content",
                        scenario.backup_rel
                    ),
                );
            }
            Err(_) => {
                return CheckOutcome::fail(
                    self.name(),
                    label,
                    format!("oc backup {} missing", scenario.backup_rel),
                );
            }
        }

        // Refreshed files and their backups must match upstream everywhere.
        if let Some(diff) = support::content_diff(&oc_cell, &up_cell) {
            if ctx.verbose {
                dump(&label, &oc_cell, &up_cell);
            }
            return CheckOutcome::fail(
                self.name(),
                label,
                format!("oc and upstream backup trees differ: {diff}"),
            );
        }

        CheckOutcome::pass(self.name(), label)
    }
}

/// Build one client rsync `Command` for `transport` and run it into the
/// (already-seeded) `dst`. The destination is never reset here, so `--backup`
/// operates on the pre-seeded tree.
///
/// Operand forms mirror `transport::pull_into`: `local` is a filesystem copy;
/// `ssh-subprocess` uses `-e ssh localhost:<src>`; `russh` uses an `ssh://` URL;
/// `daemon` uses the module URL in `daemon_url`. The sender is always `upstream`
/// for the network transports (`--rsync-path` / upstream daemon).
fn run_client(
    transport: Transport,
    client: &Path,
    upstream: &Path,
    src: &Path,
    dst: &Path,
    flags: &[&str],
    daemon_url: Option<&str>,
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
            let url = daemon_url
                .ok_or_else(|| TaskError::Validation("daemon transport without url".into()))?;
            cmd.arg(url).arg(&dst_arg);
        }
    }

    cmd.output()
        .map_err(|e| TaskError::Validation(format!("failed to spawn {cmd:?}: {e}")))
}

/// Seed one per-side cell with the OLD destination tree the transfer overwrites.
///
/// Recreates `cell` empty, then writes `dst/f1.txt` and `dst/sub/f2.txt` with
/// the OLD payloads. Idempotent: any prior `dst/` or `bak/` from a earlier run
/// is removed first.
fn seed_dest(cell: &Path) -> TaskResult<()> {
    reset_dir(cell)?;
    let sub = cell.join("dst").join("sub");
    std::fs::create_dir_all(&sub)
        .map_err(|e| TaskError::Validation(format!("create {}: {e}", sub.display())))?;
    std::fs::write(cell.join("dst").join("f1.txt"), OLD_F1)
        .map_err(|e| TaskError::Validation(format!("write old f1.txt: {e}")))?;
    std::fs::write(sub.join("f2.txt"), OLD_F2)
        .map_err(|e| TaskError::Validation(format!("write old sub/f2.txt: {e}")))
}

/// True when OLD and NEW payloads differ in length, guaranteeing rsync's
/// quick-check re-transfers the file (so `--backup` always fires) irrespective
/// of mtime.
fn forces_retransfer(old: &[u8], new: &[u8]) -> bool {
    old.len() != new.len()
}

/// Print both cell trees for a verbose failure.
fn dump(label: &str, oc_cell: &Path, up_cell: &Path) {
    eprintln!(
        "[backup/{label}] oc tree: {:?}",
        support::rel_entries(oc_cell)
    );
    eprintln!(
        "[backup/{label}] upstream tree: {:?}",
        support::rel_entries(up_cell)
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

/// Build the backup source fixture: `f1.txt` and `sub/f2.txt` with the NEW
/// payloads. Idempotent: removes any prior tree first. Mtimes are backdated so
/// the quick-check has a stable baseline for both clients.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;

    std::fs::write(src.join("f1.txt"), NEW_F1).map_err(|e| e.to_string())?;
    std::fs::write(sub.join("f2.txt"), NEW_F2).map_err(|e| e.to_string())?;

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
    use super::{NEW_F1, NEW_F2, OLD_F1, OLD_F2, forces_retransfer};

    #[test]
    fn fixture_payloads_force_a_retransfer() {
        // The fixture only exercises --backup if the transfer actually
        // overwrites the seeded files; equal lengths would let the quick-check
        // skip them and the check would pass vacuously.
        assert!(forces_retransfer(OLD_F1, NEW_F1));
        assert!(forces_retransfer(OLD_F2, NEW_F2));
    }

    #[test]
    fn equal_length_payloads_do_not_force_a_retransfer() {
        assert!(!forces_retransfer(b"abc", b"xyz"));
    }
}
