//! Transfer-skip decision parity between oc-rsync and upstream.
//!
//! Rsync has several options that change *whether* a file is transferred at all:
//! `--existing` (update only files already in the destination), `--ignore-existing`
//! (create only files missing from the destination), `--size-only` (compare by
//! size alone, ignoring mtime), and `--update`/`-u` (never overwrite a receiver
//! file that is newer than the source). Each scenario pre-seeds a destination
//! that differs from the source in the specific way the option keys on, then
//! asserts oc-rsync reaches the exact same destination tree upstream does - i.e.
//! it transferred and skipped exactly the same files - plus a non-vacuous check
//! that the option actually changed the outcome.
//!
//! Because every scenario depends on a pre-seeded destination, this check cannot
//! use `transport::pull_into` (which wipes the destination first). It seeds both
//! the oc and upstream destinations identically, builds the client `Command`
//! directly per transport - reusing `pull_into`'s operand forms - and runs the
//! transfer *without* resetting the seeded tree. Upstream rsync is the ground
//! truth for both runs; the ssh transports are skipped when no sshd answers on
//! localhost:22.

use std::path::Path;
use std::process::{Command, Output};

use crate::commands::validate::support;
use crate::commands::validate::transport::{DaemonHandle, Transport};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The transfer-skip decision parity check.
pub struct TransferConditions;

/// Source file present in the destination for the existing/update scenarios.
const KEEP: &str = "keep.txt";
/// Nested source file, exercising directory creation on the receiver.
const KEEP2: &str = "sub/keep2.txt";
/// Source file *absent* from the seeded destination; its creation is what the
/// existing/ignore-existing scenarios turn on.
const NEW: &str = "new.txt";

/// Source bytes for [`KEEP`].
const KEEP_SRC: &[u8] = b"keep-source-body\n";
/// Source bytes for [`KEEP2`].
const KEEP2_SRC: &[u8] = b"keep2-source-body\n";
/// Source bytes for [`NEW`].
const NEW_SRC: &[u8] = b"new-source-body\n";

/// Source mtime (2021-03-04). Backdated so the quick-check does not re-transfer
/// on mtime alone where a scenario needs skips to happen.
const SRC_MTIME: &str = "@1614830767";
/// Older than the source (2020-09): used for destination seeds that must lose to
/// the source anyway (they also differ in size, forcing the update).
const OLDER: &str = "@1600000000";
/// Newer than the source (2023-11): a receiver file with this mtime is the
/// precondition `--size-only` must skip past and `-u` must refuse to overwrite.
const NEWER: &str = "@1700000000";

/// One transfer-skip option and its seeding/verification strategy.
#[derive(Clone, Copy)]
enum Scenario {
    /// `--existing`: update files already present, never create missing ones.
    Existing,
    /// `--ignore-existing`: skip files already present, create only missing ones.
    IgnoreExisting,
    /// `--size-only`: skip files whose size matches, ignoring mtime.
    SizeOnly,
    /// `--update` (`-u`): never overwrite a receiver file newer than the source.
    Update,
}

/// Scenarios exercised for every transport, in report order.
const SCENARIOS: [Scenario; 4] = [
    Scenario::Existing,
    Scenario::IgnoreExisting,
    Scenario::SizeOnly,
    Scenario::Update,
];

impl Scenario {
    /// Stable short label used in the cell name.
    fn label(self) -> &'static str {
        match self {
            Scenario::Existing => "existing",
            Scenario::IgnoreExisting => "ignore-existing",
            Scenario::SizeOnly => "size-only",
            Scenario::Update => "update",
        }
    }

    /// The rsync flag under test for this scenario.
    fn flag(self) -> &'static str {
        match self {
            Scenario::Existing => "--existing",
            Scenario::IgnoreExisting => "--ignore-existing",
            Scenario::SizeOnly => "--size-only",
            Scenario::Update => "-u",
        }
    }

    /// The complete flag set: shared base plus the scenario flag.
    fn flags(self) -> [&'static str; 3] {
        ["-rlptgoD", "--numeric-ids", self.flag()]
    }
}

impl Check for TransferConditions {
    fn name(&self) -> &'static str {
        "transfer-conditions"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("transfer-conditions");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let mut out = Vec::new();
        for &transport in ctx.transports {
            for scenario in SCENARIOS {
                out.push(self.cell(ctx, transport, scenario, &root, &src));
            }
        }
        out
    }
}

impl TransferConditions {
    /// Run one (transport, scenario) cell: seed both destinations identically,
    /// transfer with each client, then assert the two destinations agree and the
    /// option changed the outcome.
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        scenario: Scenario,
        root: &Path,
        src: &Path,
    ) -> CheckOutcome {
        let label = transport.label();
        let cell = format!("{}/{label}", scenario.label());
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), cell, "no sshd on localhost:22");
        }

        let oc_dst = root.join(format!("oc-{}-{label}", scenario.label()));
        let up_dst = root.join(format!("up-{}-{label}", scenario.label()));

        // Seed both destinations identically before their respective transfers.
        if let Err(e) = seed_one(scenario, &oc_dst) {
            return CheckOutcome::skip(self.name(), cell, format!("seed oc dest: {e}"));
        }
        if let Err(e) = seed_one(scenario, &up_dst) {
            return CheckOutcome::skip(self.name(), cell, format!("seed upstream dest: {e}"));
        }

        // Non-vacuous guard: the seeded precondition the option keys on must hold
        // before the transfer, or the cell would pass without exercising it.
        if let Err(e) = precondition(scenario, &oc_dst) {
            return CheckOutcome::fail(self.name(), cell, format!("oc {e}"));
        }
        if let Err(e) = precondition(scenario, &up_dst) {
            return CheckOutcome::fail(self.name(), cell, format!("upstream {e}"));
        }

        // The daemon transport needs one live upstream daemon shared by both
        // client runs; keep it alive for the whole cell.
        let daemon = if transport == Transport::Daemon {
            match DaemonHandle::start(ctx.upstream, src, ctx.work) {
                Ok(handle) => Some(handle),
                Err(e) => return CheckOutcome::skip(self.name(), cell, format!("daemon: {e}")),
            }
        } else {
            None
        };
        let daemon_url = daemon.as_ref().map(|d| d.module_url());
        let flags = scenario.flags();

        match run_client(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            src,
            &up_dst,
            &flags,
            daemon_url.as_deref(),
        ) {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), &cell, "upstream", other),
        }
        match run_client(
            transport,
            ctx.oc,
            ctx.upstream,
            src,
            &oc_dst,
            &flags,
            daemon_url.as_deref(),
        ) {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), &cell, "oc", other),
        }
        drop(daemon);

        match verify(scenario, &oc_dst, &up_dst, src) {
            Ok(()) => CheckOutcome::pass(self.name(), cell),
            Err(diff) => {
                if ctx.verbose {
                    eprintln!(
                        "[transfer-conditions/{cell}] oc {:?} upstream {:?}",
                        support::rel_entries(&oc_dst),
                        support::rel_entries(&up_dst),
                    );
                }
                CheckOutcome::fail(self.name(), cell, diff)
            }
        }
    }
}

/// Build one client rsync `Command` for `transport` and run it into the
/// (already-seeded) destination. The destination is never reset here, so the
/// scenario's pre-seeded tree survives into the transfer.
///
/// Operand forms mirror `transport::pull_into`: `local` is a filesystem copy;
/// `ssh-subprocess` uses `-e ssh localhost:<src>`; `russh` uses an `ssh://` URL;
/// `daemon` uses the module URL passed in `daemon_url`. The sender is always
/// `upstream` for the network transports (`--rsync-path` / upstream daemon).
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

/// Seed `dst` for `scenario`, recreating it empty first (idempotent).
///
/// - `existing`: only [`KEEP`]/[`KEEP2`] present, shorter (different-size) older
///   content, and [`NEW`] absent - so `--existing` has files to update and one
///   to refuse to create.
/// - `ignore-existing`: only [`KEEP`] present with differing older content -
///   `--ignore-existing` must keep it and create the rest.
/// - `size-only`: all three files present with equal-length-but-different
///   content and a *newer* mtime - `--size-only` must skip them on size alone.
/// - `update`: only [`KEEP`] present with a *newer* mtime and differing content -
///   `-u` must refuse to overwrite it.
fn seed_one(scenario: Scenario, dst: &Path) -> Result<(), String> {
    reset(dst)?;
    match scenario {
        Scenario::Existing => {
            write_file(dst, KEEP, b"old\n")?;
            write_file(dst, KEEP2, b"old2\n")?;
            backdate(dst, &[KEEP, KEEP2], OLDER)?;
        }
        Scenario::IgnoreExisting => {
            write_file(dst, KEEP, b"stale-keep-content\n")?;
            backdate(dst, &[KEEP], OLDER)?;
        }
        Scenario::SizeOnly => {
            write_file(dst, KEEP, &differing_same_len(KEEP_SRC))?;
            write_file(dst, KEEP2, &differing_same_len(KEEP2_SRC))?;
            write_file(dst, NEW, &differing_same_len(NEW_SRC))?;
            backdate(dst, &[KEEP, KEEP2, NEW], NEWER)?;
        }
        Scenario::Update => {
            write_file(dst, KEEP, b"receiver-newer-keep-content\n")?;
            backdate(dst, &[KEEP], NEWER)?;
        }
    }
    Ok(())
}

/// Assert the seeded precondition the option keys on holds before the transfer.
///
/// A false precondition means a seeding bug, not a divergence, so callers turn
/// it into a `fail` rather than let the cell pass vacuously.
fn precondition(scenario: Scenario, dst: &Path) -> Result<(), String> {
    match scenario {
        Scenario::Existing => {
            if dst.join(NEW).exists() {
                return Err("dest new.txt present before transfer".into());
            }
        }
        Scenario::IgnoreExisting => {
            if reads(dst, KEEP).as_deref() == Some(KEEP_SRC) {
                return Err("dest keep.txt does not differ from source".into());
            }
        }
        Scenario::SizeOnly => {
            let seed = reads(dst, KEEP).ok_or("dest keep.txt missing")?;
            if seed.len() != KEEP_SRC.len() {
                return Err("dest keep.txt size differs from source".into());
            }
            if seed == KEEP_SRC {
                return Err("dest keep.txt does not differ from source".into());
            }
        }
        Scenario::Update => {
            if reads(dst, KEEP).as_deref() == Some(KEEP_SRC) {
                return Err("dest keep.txt does not differ from source".into());
            }
        }
    }
    Ok(())
}

/// After both transfers, assert the destinations agree and the option changed
/// the outcome (a plain transfer would have differed).
fn verify(scenario: Scenario, oc_dst: &Path, up_dst: &Path, src: &Path) -> Result<(), String> {
    if let Some(diff) = support::content_diff(oc_dst, up_dst) {
        return Err(format!("oc and upstream dests differ: {diff}"));
    }
    match scenario {
        Scenario::Existing => {
            for d in [oc_dst, up_dst] {
                if d.join(NEW).exists() {
                    return Err("new.txt created despite --existing".into());
                }
            }
        }
        Scenario::IgnoreExisting => {
            for d in [oc_dst, up_dst] {
                if !d.join(NEW).exists() {
                    return Err("new.txt not created under --ignore-existing".into());
                }
                if reads(d, KEEP).as_deref() == Some(KEEP_SRC) {
                    return Err("keep.txt overwritten despite --ignore-existing".into());
                }
            }
        }
        Scenario::SizeOnly => {
            if support::content_diff(oc_dst, src).is_none() {
                return Err("--size-only did not skip: dest equals source".into());
            }
        }
        Scenario::Update => {
            for d in [oc_dst, up_dst] {
                if reads(d, KEEP).as_deref() == Some(KEEP_SRC) {
                    return Err("keep.txt overwritten despite -u".into());
                }
            }
        }
    }
    Ok(())
}

/// Build the source tree, backdating every file to [`SRC_MTIME`]. Idempotent:
/// removes any prior tree first.
fn build_fixture(src: &Path) -> Result<(), String> {
    reset(src)?;
    write_file(src, KEEP, KEEP_SRC)?;
    write_file(src, KEEP2, KEEP2_SRC)?;
    write_file(src, NEW, NEW_SRC)?;
    backdate(src, &[KEEP, KEEP2, NEW], SRC_MTIME)
}

/// A buffer of the same length as `src` but differing in every byte, so a
/// destination seeded with it matches the source's size while differing in
/// content - exactly the state `--size-only` must skip.
fn differing_same_len(src: &[u8]) -> Vec<u8> {
    src.iter().map(|b| b.wrapping_add(1)).collect()
}

/// Read `root/rel`, returning its bytes or `None` when absent.
fn reads(root: &Path, rel: &str) -> Option<Vec<u8>> {
    std::fs::read(root.join(rel)).ok()
}

/// Write `bytes` to `root/rel`, creating parent directories as needed.
fn write_file(root: &Path, rel: &str, bytes: &[u8]) -> Result<(), String> {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, bytes).map_err(|e| e.to_string())
}

/// Set each `rel`'s mtime under `root` to `epoch` via `touch -d @epoch`.
fn backdate(root: &Path, rels: &[&str], epoch: &str) -> Result<(), String> {
    for rel in rels {
        let path = root.join(rel);
        support::capture("touch", &["-h", "-d", epoch, &path.to_string_lossy()])
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Recreate `dir` as an empty directory.
fn reset(dir: &Path) -> Result<(), String> {
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())
}

/// Distinguish a genuine divergence (non-zero exit) from an unrunnable cell.
fn skip_or_fail(
    check: &'static str,
    cell: &str,
    who: &str,
    result: TaskResult<Output>,
) -> CheckOutcome {
    match result {
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let code = out.status.code().unwrap_or(-1);
            CheckOutcome::fail(
                check,
                cell,
                format!("{who} exited {code}: {}", stderr.trim()),
            )
        }
        Err(e) => CheckOutcome::skip(check, cell, format!("{who} could not run: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn differing_same_len_keeps_length_and_changes_every_byte() {
        let src = b"keep-source-body\n";
        let out = differing_same_len(src);
        assert_eq!(out.len(), src.len());
        assert_ne!(out.as_slice(), src);
        for (a, b) in src.iter().zip(out.iter()) {
            assert_ne!(a, b);
        }
    }

    #[test]
    fn differing_same_len_handles_empty() {
        assert!(differing_same_len(b"").is_empty());
    }

    #[test]
    fn every_scenario_flag_set_carries_its_flag_after_the_shared_base() {
        for scenario in SCENARIOS {
            let flags = scenario.flags();
            assert_eq!(flags[0], "-rlptgoD");
            assert_eq!(flags[1], "--numeric-ids");
            assert_eq!(flags[2], scenario.flag());
        }
    }

    #[test]
    fn scenario_labels_are_unique() {
        let labels: Vec<&str> = SCENARIOS.iter().map(|s| s.label()).collect();
        let mut sorted = labels.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(labels.len(), sorted.len());
    }

    #[test]
    fn size_only_seed_flag_is_size_only() {
        assert_eq!(Scenario::SizeOnly.flag(), "--size-only");
    }
}
