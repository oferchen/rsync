//! Access-time (`--atimes`, `-U`) parity between oc-rsync and upstream.
//!
//! Builds a small fixture whose files carry distinct, known past access times,
//! then pulls it with each client over every transport and asserts oc's
//! destination is byte-identical to upstream's *and* that the access time
//! (`stat -c %X`) of every file matches between the two destinations. A
//! non-vacuous guard also checks oc's dest access time equals the *source's*
//! known value, proving oc actually preserved the atime rather than stamping
//! "now".
//!
//! Access-time preservation is meaningless when the work filesystem is mounted
//! so that atime cannot be set (e.g. `noatime`), so the check probes the work
//! filesystem first - setting a known atime and reading it back - and skips the
//! whole matrix when the value does not stick. It also probes that the host's
//! upstream rsync was built with `--atimes` support.

use std::path::{Path, PathBuf};

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The access-time parity check.
pub struct Atimes;

/// Preserve metadata and request atime preservation. `--atimes` (`-U`) is the
/// attribute under test; `--numeric-ids` keeps owner comparison host-stable.
const FLAGS: &[&str] = &["-rlptgoD", "--atimes", "--numeric-ids"];

/// Epoch the support probe writes and reads back to decide whether the work
/// filesystem honors explicitly-set access times.
const PROBE_ATIME: i64 = 1_600_000_000;

impl Check for Atimes {
    fn name(&self) -> &'static str {
        "atimes"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        // Support probe: the host's upstream rsync may be built without
        // `--atimes` support, in which case it refuses the option at parse time
        // and exits non-zero. Nothing to compare against, so skip the matrix.
        if !upstream_supports_atimes(ctx.upstream) {
            return vec![CheckOutcome::skip(
                self.name(),
                "support",
                "upstream rsync lacks --atimes support",
            )];
        }

        // Support probe: the work filesystem must honor an explicitly-set atime,
        // or there is nothing meaningful to preserve or compare.
        if !work_honors_atimes(ctx.work) {
            return vec![CheckOutcome::skip(
                self.name(),
                "support",
                "filesystem does not honor atimes",
            )];
        }

        let root = ctx.work.join("atimes");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        // Snapshot the source atimes *before* any transfer reads (and possibly
        // bumps) them, so the non-vacuous guard compares against the known past
        // values rather than a moving target.
        let src_atimes = source_file_atimes(&src);
        let expected = support::entry_count(&src);
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        ctx.transports
            .iter()
            .map(|&t| self.cell(ctx, t, &root, &flags, expected, &src_atimes))
            .collect()
    }
}

impl Atimes {
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        flags: &[String],
        expected: usize,
        src_atimes: &[(PathBuf, String)],
    ) -> CheckOutcome {
        let label = transport.label();
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }
        let src = root.join("src");
        let oc_dst = root.join(format!("oc-{label}"));
        let up_dst = root.join(format!("up-{label}"));

        let up = match pull_into(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            &src,
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
            &src,
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
        // Parity: oc's per-file atime must match upstream's byte-for-byte.
        if let Some(diff) = atime_diff(&oc_dst, &up_dst) {
            if ctx.verbose {
                dump_atimes(&oc_dst, &up_dst);
            }
            return CheckOutcome::fail(self.name(), label, diff);
        }
        // Non-vacuous: oc's dest atime must equal the source's known past value,
        // proving it preserved the atime rather than stamping "now".
        match source_atime_diff(&oc_dst, src_atimes) {
            Some(diff) => {
                if ctx.verbose {
                    dump_atimes(&oc_dst, &up_dst);
                }
                CheckOutcome::fail(self.name(), label, diff)
            }
            None => CheckOutcome::pass(self.name(), label),
        }
    }
}

/// First access-time divergence between two trees (regular files only), or
/// `None` when every file's `stat -c %X` matches. Directory atimes are excluded:
/// merely reading a directory can bump its atime, making them unstable.
fn atime_diff(oc: &Path, up: &Path) -> Option<String> {
    for rel in support::rel_entries(oc) {
        let oc_path = oc.join(&rel);
        if !is_regular_file(&oc_path) {
            continue;
        }
        let oc_at = atime_of(&oc_path);
        let up_at = atime_of(&up.join(&rel));
        if oc_at != up_at {
            return Some(format!(
                "atime differs at {}: oc={} upstream={}",
                rel.display(),
                oc_at.as_deref().unwrap_or("?"),
                up_at.as_deref().unwrap_or("?"),
            ));
        }
    }
    None
}

/// First file whose oc destination atime differs from the source's snapshot
/// value, or `None` when oc preserved every known source atime.
fn source_atime_diff(oc: &Path, src_atimes: &[(PathBuf, String)]) -> Option<String> {
    for (rel, want) in src_atimes {
        let got = atime_of(&oc.join(rel));
        if got.as_deref() != Some(want.as_str()) {
            return Some(format!(
                "atime not preserved at {}: oc={} source={want}",
                rel.display(),
                got.as_deref().unwrap_or("?"),
            ));
        }
    }
    None
}

/// Access time of `path` as reported by `stat -c %X`, or `None` when the stat
/// call fails (e.g. the path is missing).
fn atime_of(path: &Path) -> Option<String> {
    support::capture("stat", &["-c", "%X", &path.to_string_lossy()]).ok()
}

/// Snapshot the `stat -c %X` access time of every regular file under `src`.
fn source_file_atimes(src: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for rel in support::rel_entries(src) {
        let path = src.join(&rel);
        if is_regular_file(&path) {
            if let Some(at) = atime_of(&path) {
                out.push((rel, at));
            }
        }
    }
    out
}

/// True when `path` is a regular file (not a directory or symlink).
fn is_regular_file(path: &Path) -> bool {
    path.symlink_metadata()
        .map(|m| m.file_type().is_file())
        .unwrap_or(false)
}

/// Print each file's oc/upstream access time for verbose diagnostics.
fn dump_atimes(oc: &Path, up: &Path) {
    for rel in support::rel_entries(oc) {
        let oc_path = oc.join(&rel);
        if !is_regular_file(&oc_path) {
            continue;
        }
        let oc_at = atime_of(&oc_path);
        let up_at = atime_of(&up.join(&rel));
        eprintln!(
            "    {}: oc={} upstream={}",
            rel.display(),
            oc_at.as_deref().unwrap_or("?"),
            up_at.as_deref().unwrap_or("?"),
        );
    }
}

/// Probe whether the host's upstream rsync was built with `--atimes` (`-U`)
/// support. A build without it refuses the option at parse time (printing
/// `... does not support --atimes` and exiting non-zero) before `--version`
/// short-circuits, so a clean `--atimes --version` run succeeds only when the
/// option is accepted. upstream: options.c `parse_one_refuse_match(..., "atimes")`
/// under `#ifndef SUPPORT_ATIMES`.
fn upstream_supports_atimes(upstream: &Path) -> bool {
    std::process::Command::new(upstream)
        .args(["--atimes", "--version"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Probe whether the work filesystem honors an explicitly-set access time.
/// Writes a throwaway file, sets its atime to a known epoch via
/// `touch -a -d @<epoch>`, then reads `stat -c %X` back; on a `noatime`-style
/// mount the value does not stick and the whole matrix is skipped.
fn work_honors_atimes(work: &Path) -> bool {
    let probe = work.join(".atimes-probe");
    if std::fs::write(&probe, b"probe").is_err() {
        return false;
    }
    let name = probe.to_string_lossy().into_owned();
    let set = support::capture("touch", &["-a", "-d", &format!("@{PROBE_ATIME}"), &name]).is_ok();
    let raw = support::capture("stat", &["-c", "%X", &name]).ok();
    let _ = std::fs::remove_file(&probe);
    set && raw.as_deref().and_then(parse_epoch) == Some(PROBE_ATIME)
}

/// Parse a `stat -c %X` (or `%Y`) value: a trimmed integer count of seconds
/// since the epoch. `None` for empty or non-numeric input.
fn parse_epoch(raw: &str) -> Option<i64> {
    raw.trim().parse::<i64>().ok()
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

/// Build the atimes fixture. Idempotent: removes any prior tree first. A couple
/// of files plus a subdirectory holding a file. Each file gets a distinct known
/// past access time (`touch -a`) and a backdated mtime (`touch -m`) so the
/// quick-check does not skip the transfer under test.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;

    // (relative path contents, distinct atime epoch) for each source file.
    let files: [(PathBuf, &[u8], i64); 3] = [
        (PathBuf::from("alpha"), b"alpha", 1_500_000_000),
        (PathBuf::from("bravo"), b"bravo", 1_500_001_000),
        (PathBuf::from("sub/charlie"), b"charlie", 1_500_002_000),
    ];
    for (rel, body, atime) in files {
        let path = src.join(&rel);
        std::fs::write(&path, body).map_err(|e| e.to_string())?;
        let name = path.to_string_lossy().into_owned();
        // Backdate mtime first, then stamp the distinct atime; separate `touch`
        // calls so neither clobbers the other's target field.
        support::capture("touch", &["-m", "-d", "@1614830767", &name])
            .map_err(|e| e.to_string())?;
        support::capture("touch", &["-a", "-d", &format!("@{atime}"), &name])
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_epoch;

    #[test]
    fn parse_epoch_reads_trimmed_integer_seconds() {
        // `stat -c %X` prints a bare integer, sometimes with trailing newline.
        assert_eq!(parse_epoch("1500000000"), Some(1_500_000_000));
        assert_eq!(parse_epoch("  1500000000\n"), Some(1_500_000_000));
        assert_eq!(parse_epoch("0"), Some(0));
    }

    #[test]
    fn parse_epoch_rejects_empty_and_non_numeric() {
        assert_eq!(parse_epoch(""), None);
        assert_eq!(parse_epoch("-"), None);
        assert_eq!(parse_epoch("1.5"), None);
        assert_eq!(parse_epoch("abc"), None);
    }
}
