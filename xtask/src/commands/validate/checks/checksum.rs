//! `--checksum` (`-c`) transfer-decision parity between oc-rsync and upstream.
//!
//! Rsync's default quick-check skips any destination file whose size and mtime
//! equal the source's. `-c` overrides that: it compares by strong checksum, so a
//! destination file with matching size and mtime but *different content* must be
//! re-transferred. This check pre-seeds each destination so the quick-check
//! *would* skip the differing files - identical size, identical mtime - and then
//! asserts that with `-c` oc-rsync re-transfers exactly what upstream does and
//! ends byte-identical to the source, across every transport in `ctx.transports`.
//!
//! Because the destination is pre-seeded, this check cannot use
//! `transport::pull_into` (which wipes the destination first). It builds the
//! `Command` directly - reusing `pull_into`'s per-transport operand forms - and
//! runs the transfer *without* resetting the seeded destination.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Output};

use crate::commands::validate::support;
use crate::commands::validate::transport::{DaemonHandle, Transport};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The `--checksum` transfer-decision check.
pub struct Checksum;

/// Recursive, checksum-based decision, itemized output, numeric ids.
const FLAGS: &[&str] = &["-rlptgoD", "-c", "-i", "--numeric-ids"];

/// A fixed epoch (2021-03-04) applied to both source and seeded destination so
/// their mtimes match and the quick-check would skip the differing files.
const MTIME: &str = "@1614830767";

/// One fixture file: its source bytes, the bytes pre-seeded into the
/// destination (equal length, so size matches), and whether `-c` must
/// re-transfer it.
///
/// For a re-transfer file the seed differs from the source (same size, same
/// mtime, different content -> quick-check skips, `-c` does not). For a
/// non-re-transfer file the seed is byte-identical, so even `-c` leaves it.
struct FileSpec {
    /// Path relative to the transfer root.
    rel: &'static str,
    /// Source content written under `src/`.
    src: &'static [u8],
    /// Destination content pre-seeded before the transfer.
    seed: &'static [u8],
    /// True when `-c` must re-transfer this file.
    retransfer: bool,
}

/// The fixture: one identical file the checksum path must leave alone, and two
/// differing files (top-level and nested) it must re-transfer. Each seed has the
/// exact byte length of its source so size matches under the quick-check.
const FILES: &[FileSpec] = &[
    FileSpec {
        rel: "same.txt",
        src: b"identical-content",
        seed: b"identical-content",
        retransfer: false,
    },
    FileSpec {
        rel: "differ.txt",
        src: b"SOURCE-VERSION-aaaa",
        seed: b"DEST-VERSION-bbbbbb",
        retransfer: true,
    },
    FileSpec {
        rel: "sub/differ2.txt",
        src: b"nested-source-body",
        seed: b"nested-DEST-body!!",
        retransfer: true,
    },
];

impl Check for Checksum {
    fn name(&self) -> &'static str {
        "checksum"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("checksum");
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

impl Checksum {
    /// Run one transport cell: seed both destinations identically, transfer with
    /// each client, then compare re-transfer decisions and final content.
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

        // Non-vacuous guard: the seeded differing file must really differ from
        // the source before the transfer, or the checksum path is not exercised.
        let seeded = std::fs::read(oc_dst.join("differ.txt")).ok();
        let source = std::fs::read(src.join("differ.txt")).ok();
        if seeded.is_none() || seeded == source {
            return CheckOutcome::fail(
                self.name(),
                label,
                "seed guard: dest differ.txt did not differ from source",
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
        let oc = match run_transfer(ctx.oc, ctx.upstream, transport, src, &oc_dst, ctx.work) {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), label, "oc", other),
        };

        // Both destinations must now be byte-identical to the source: proves the
        // differing files were re-transferred and the identical one was kept.
        if let Some(d) = support::content_diff(&up_dst, src) {
            return CheckOutcome::fail(self.name(), label, format!("upstream dest != source: {d}"));
        }
        if let Some(d) = support::content_diff(&oc_dst, src) {
            return CheckOutcome::fail(self.name(), label, format!("oc dest != source: {d}"));
        }
        if let Some(d) = support::content_diff(&oc_dst, &up_dst) {
            return CheckOutcome::fail(
                self.name(),
                label,
                format!("oc dest != upstream dest: {d}"),
            );
        }

        let up_x = transferred_files(&String::from_utf8_lossy(&up.stdout));
        let oc_x = transferred_files(&String::from_utf8_lossy(&oc.stdout));

        if oc_x != up_x {
            if ctx.verbose {
                eprintln!("[checksum/{label}] oc transferred {oc_x:?} upstream {up_x:?}");
            }
            return CheckOutcome::fail(
                self.name(),
                label,
                format!("re-transferred set differs (oc {oc_x:?} vs upstream {up_x:?})"),
            );
        }

        for want in FILES.iter().filter(|f| f.retransfer).map(|f| f.rel) {
            if !oc_x.contains(want) {
                return CheckOutcome::fail(
                    self.name(),
                    label,
                    format!("-c did not re-transfer {want}"),
                );
            }
        }
        for unchanged in FILES.iter().filter(|f| !f.retransfer).map(|f| f.rel) {
            if oc_x.contains(unchanged) {
                return CheckOutcome::fail(
                    self.name(),
                    label,
                    format!("-c re-transferred {unchanged} despite identical content"),
                );
            }
        }

        CheckOutcome::pass(self.name(), label)
    }
}

/// Extract the set of file paths rsync itemized as transferred.
///
/// A transfer row's change string begins `>f` (received) or `<f` (sent); the
/// path follows after the single space that terminates the 11-char change
/// string. Directory, metadata-only, and summary lines are ignored - so the set
/// contains exactly the files that moved bytes.
pub fn transferred_files(stdout: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for raw in stdout.lines() {
        let line = raw.trim();
        if line.starts_with(">f") || line.starts_with("<f") {
            if let Some((_, path)) = line.split_once(' ') {
                out.insert(path.trim().to_string());
            }
        }
    }
    out
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

/// Build the source tree, backdating every file's mtime to [`MTIME`]. Idempotent:
/// removes any prior tree first.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(src.join("sub")).map_err(|e| e.to_string())?;
    for f in FILES {
        std::fs::write(src.join(f.rel), f.src).map_err(|e| e.to_string())?;
    }
    backdate(src)
}

/// Pre-seed a destination so the quick-check would skip the differing files.
///
/// Recreates `dst` empty, writes each file's seed bytes (equal length to the
/// source, so size matches), then backdates every file to [`MTIME`] so its mtime
/// matches the source too. The differing files thus present identical size and
/// mtime - which only `-c` can see through.
fn seed_dest(dst: &Path) -> Result<(), String> {
    if dst.exists() {
        std::fs::remove_dir_all(dst).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(dst.join("sub")).map_err(|e| e.to_string())?;
    for f in FILES {
        std::fs::write(dst.join(f.rel), f.seed).map_err(|e| e.to_string())?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_transferred_file_paths() {
        let out = ">fc.t...... differ.txt\n\
                   >fcst...... sub/differ2.txt\n\
                   cd+++++++++ sub/\n\
                   .f          same.txt\n\
                   sent 100 bytes  received 20 bytes\n";
        let set = transferred_files(out);
        assert!(set.contains("differ.txt"));
        assert!(set.contains("sub/differ2.txt"));
        assert!(!set.contains("same.txt"));
        assert!(!set.contains("sub/"));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn recognizes_sender_side_marker() {
        assert!(transferred_files("<f+++++++++ x.bin\n").contains("x.bin"));
    }

    #[test]
    fn ignores_summary_and_directory_lines() {
        let out = "cd+++++++++ sub/\ntotal size is 12  speedup is 0.10\n";
        assert!(transferred_files(out).is_empty());
    }

    #[test]
    fn seed_matches_source_length_and_retransfer_intent() {
        for f in FILES {
            assert_eq!(
                f.src.len(),
                f.seed.len(),
                "{} seed len must equal src",
                f.rel
            );
            if f.retransfer {
                assert_ne!(
                    f.src, f.seed,
                    "{} must differ to force -c retransfer",
                    f.rel
                );
            } else {
                assert_eq!(f.src, f.seed, "{} must be byte-identical", f.rel);
            }
        }
    }
}
