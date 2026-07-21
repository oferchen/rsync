//! `-E` / `--executability` exec-bit parity between oc-rsync and upstream.
//!
//! Ports upstream testsuite `executability.test`. Used *without* `-p` (the flag
//! set is `-rltgoD`, i.e. archive minus permissions), `--executability` copies
//! only the execute bit: it makes each destination regular file executable, or
//! not, to match the source, while leaving the destination file's remaining
//! permission bits exactly as they were. Upstream's rule (rsync.c) is: if the
//! source is not executable, clear all three exec bits of the dest's current
//! mode; if the source is executable and the dest currently has none, set the
//! exec bits derived from the dest's read bits (`(mode & 0444) >> 2`).
//!
//! Because `-E` only adjusts the exec bit of a *pre-existing* destination file,
//! this check pre-seeds each destination with the same files as the source but
//! *inverted* executability over a fixed base perm - `run.sh` seeded non-exec
//! (`0644`), `data.txt` seeded exec (`0755`), `sub/tool` seeded non-exec
//! (`0644`). Seeds carry the source's byte length and mtime, and `-I` forces
//! every file to be visited, so only `-E`'s exec-bit logic can change anything.
//!
//! Since the destination is pre-seeded, this check cannot use
//! `transport::pull_into` (which wipes the destination first). It builds the
//! `Command` directly - reusing `pull_into`'s per-transport operand forms - and
//! runs the transfer without resetting the seeded destination.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::commands::validate::support;
use crate::commands::validate::transport::{DaemonHandle, Transport};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The `--executability` exec-bit parity check.
pub struct Executability;

/// Archive minus perms, plus `--executability`; `-I` forces every file to be
/// visited so the exec-bit re-evaluation always runs despite matching size+mtime.
const FLAGS: &[&str] = &["-rltgoD", "--executability", "--numeric-ids", "-I"];

/// A fixed epoch (2021-03-04) applied to both source and seeded destination so
/// their mtimes match and only `-E`'s exec-bit logic differs.
const MTIME: &str = "@1614830767";

/// One fixture file: its bytes, the source mode, the inverted destination-seed
/// mode, and whether the source is executable (so `-E` must leave the dest file
/// user-executable).
struct FileSpec {
    /// Path relative to the transfer root.
    rel: &'static str,
    /// File bytes, written identically to source and seed so sizes match.
    body: &'static [u8],
    /// Mode the source file carries.
    src_mode: u32,
    /// Mode the destination is pre-seeded with (inverted executability).
    seed_mode: u32,
}

/// The fixture: an executable script, a non-executable data file, and a nested
/// executable tool. Each seed inverts the source's executability over a fixed
/// base so `-E` has a visible exec bit to flip in each direction.
const FILES: &[FileSpec] = &[
    FileSpec {
        rel: "run.sh",
        body: b"#!/bin/sh\necho run\n",
        src_mode: 0o755,
        seed_mode: 0o644,
    },
    FileSpec {
        rel: "data.txt",
        body: b"plain data payload\n",
        src_mode: 0o644,
        seed_mode: 0o755,
    },
    FileSpec {
        rel: "sub/tool",
        body: b"#!/bin/sh\nexec tool\n",
        src_mode: 0o750,
        seed_mode: 0o644,
    },
];

impl Check for Executability {
    fn name(&self) -> &'static str {
        "executability"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("executability");
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

impl Executability {
    /// Run one transport cell: seed both destinations with inverted exec bits,
    /// transfer with each client, then require identical mode bits and evidence
    /// that `-E` flipped the exec bit in each direction.
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

        let up_dst = root.join(format!("up-{label}"));
        let oc_dst = root.join(format!("oc-{label}"));
        if let Err(e) = seed_dest(&up_dst) {
            return CheckOutcome::skip(self.name(), label, format!("seed upstream dest: {e}"));
        }
        if let Err(e) = seed_dest(&oc_dst) {
            return CheckOutcome::skip(self.name(), label, format!("seed oc dest: {e}"));
        }

        // Seed guard: the destination `run.sh` must start non-executable while
        // the source is executable, or `-E` has nothing to flip and the check
        // would pass vacuously.
        if user_exec_of(&oc_dst.join("run.sh")) != Some(false) {
            return CheckOutcome::fail(
                self.name(),
                label,
                "seed guard: dest run.sh was not seeded non-executable",
            );
        }

        let up = match run_transfer(
            ctx.upstream,
            ctx.upstream,
            transport.for_upstream(),
            src,
            &up_dst,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), label, "upstream", other),
        };
        let _ = up;
        let oc = match run_transfer(ctx.oc, ctx.upstream, transport, src, &oc_dst, ctx.work) {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), label, "oc", other),
        };
        let _ = oc;

        // The two destination trees must agree on every entry's mode bits.
        if let Some(diff) = mode_diff(&oc_dst, &up_dst) {
            if ctx.verbose {
                eprintln!("[executability/{label}] {diff}");
            }
            return CheckOutcome::fail(self.name(), label, diff);
        }

        // Non-vacuous guard: `-E` must have made the executable source's dest
        // file user-executable and the non-executable source's dest file not,
        // in both trees.
        for (path, want) in [
            (oc_dst.join("run.sh"), true),
            (oc_dst.join("data.txt"), false),
            (up_dst.join("run.sh"), true),
            (up_dst.join("data.txt"), false),
        ] {
            if user_exec_of(&path) != Some(want) {
                return CheckOutcome::fail(
                    self.name(),
                    label,
                    format!(
                        "-E did not set exec bit as expected: {} user-exec != {want}",
                        path.display()
                    ),
                );
            }
        }

        CheckOutcome::pass(self.name(), label)
    }
}

/// True if `mode`'s user-execute bit is set.
fn user_exec(mode: u32) -> bool {
    mode & 0o100 != 0
}

/// The user-execute bit of `path`, or `None` when it cannot be stat'd.
fn user_exec_of(path: &Path) -> Option<bool> {
    path.symlink_metadata().ok().map(|m| user_exec(m.mode()))
}

/// Map a tree to per-entry permission bits (`mode & 0o7777`) for comparison.
fn mode_map(root: &Path) -> BTreeMap<PathBuf, u32> {
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

/// Build the transfer command for one cell without touching the destination.
///
/// Mirrors `transport::pull_into`'s operand forms - local copy, ssh subprocess,
/// russh `ssh://` URL, or an upstream `rsync://` daemon - but omits the
/// destination reset so the pre-seeded files survive into the transfer.
fn run_transfer(
    client: &Path,
    upstream: &Path,
    transport: Transport,
    src: &Path,
    dst: &Path,
    work: &Path,
) -> TaskResult<Output> {
    let dst_arg = format!("{}/", dst.display());
    let mut cmd = Command::new(client);
    cmd.args(FLAGS);

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
            let daemon = DaemonHandle::start(upstream, src, work)?;
            cmd.arg(daemon.module_url()).arg(&dst_arg);
            let out = spawn(cmd)?;
            drop(daemon);
            return Ok(out);
        }
    }
    spawn(cmd)
}

/// Run a prepared command, capturing its output.
fn spawn(mut cmd: Command) -> TaskResult<Output> {
    cmd.output()
        .map_err(|e| TaskError::Validation(format!("failed to spawn {cmd:?}: {e}")))
}

/// Distinguish a genuine divergence (non-zero exit) from an unrunnable cell.
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

/// Build the source tree with per-file modes, backdating every file to
/// [`MTIME`]. Idempotent: removes any prior tree first.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(src.join("sub")).map_err(|e| e.to_string())?;
    for f in FILES {
        write_mode(&src.join(f.rel), f.body, f.src_mode)?;
    }
    backdate(src)
}

/// Pre-seed a destination with inverted executability over a fixed base perm.
///
/// Recreates `dst` empty, writes each file's bytes (source length, so size
/// matches) at its inverted seed mode, then backdates every file to [`MTIME`]
/// so its mtime matches the source. Only `-E`'s exec-bit logic can then differ.
fn seed_dest(dst: &Path) -> Result<(), String> {
    if dst.exists() {
        std::fs::remove_dir_all(dst).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(dst.join("sub")).map_err(|e| e.to_string())?;
    for f in FILES {
        write_mode(&dst.join(f.rel), f.body, f.seed_mode)?;
    }
    backdate(dst)
}

/// Set every fixture file's mtime under `root` to [`MTIME`] via `touch`.
fn backdate(root: &Path) -> Result<(), String> {
    for f in FILES {
        let path = root.join(f.rel);
        support::capture("touch", &["-h", "-d", MTIME, &path.to_string_lossy()])
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Write `bytes` to `path` then set its mode.
fn write_mode(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| e.to_string())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{FILES, mode_diff, user_exec, write_mode};

    #[test]
    fn user_exec_reads_the_owner_execute_bit() {
        assert!(user_exec(0o755));
        assert!(user_exec(0o700));
        assert!(!user_exec(0o644));
        assert!(!user_exec(0o600));
    }

    #[test]
    fn user_exec_of_file_reflects_set_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let exec = dir.path().join("run.sh");
        let plain = dir.path().join("data.txt");
        write_mode(&exec, b"x", 0o755).unwrap();
        write_mode(&plain, b"y", 0o644).unwrap();
        assert_eq!(super::user_exec_of(&exec), Some(true));
        assert_eq!(super::user_exec_of(&plain), Some(false));
    }

    #[test]
    fn mode_diff_none_for_identical_modes() {
        // Explicit set_permissions; no reliance on GNU `touch -d @epoch`.
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for root in [a.path(), b.path()] {
            write_mode(&root.join("f"), b"x", 0o750).unwrap();
        }
        assert!(mode_diff(a.path(), b.path()).is_none());
    }

    #[test]
    fn mode_diff_names_the_diverging_path() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write_mode(&a.path().join("f"), b"x", 0o755).unwrap();
        write_mode(&b.path().join("f"), b"x", 0o644).unwrap();
        let diff = mode_diff(a.path(), b.path()).unwrap();
        assert!(diff.contains("mode differs"));
        assert!(diff.contains("f"));
    }

    #[test]
    fn seed_inverts_executability_of_every_source_file() {
        // The seed must flip each source file's exec bit so `-E` has work to do.
        for f in FILES {
            assert_ne!(
                user_exec(f.src_mode),
                user_exec(f.seed_mode),
                "{} seed must invert source executability",
                f.rel
            );
        }
    }
}
