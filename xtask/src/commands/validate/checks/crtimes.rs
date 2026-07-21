//! Create/birth-time (`--crtimes`, `-N`) parity between oc-rsync and upstream.
//!
//! Builds a small fixture, then pulls it with each client over every transport
//! and asserts oc's destination is byte-identical to upstream's *and* that the
//! birth time (`stat -c %W`) of every entry matches between the two
//! destinations. This proves oc and upstream handle `--crtimes` identically on
//! this host - both preserving the crtime, or both leaving the fresh file's own
//! birth time - without assuming crtime is settable anywhere.
//!
//! Birth time is a fragile attribute: many Linux filesystems and kernels do not
//! expose it at all (`stat -c %W` reports `0`), and most cannot *set* it, so
//! `--crtimes` is frequently a no-op or unsupported. The check probes the work
//! filesystem first and skips the whole matrix when birth times are not exposed.

use std::path::Path;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The create/birth-time parity check.
pub struct Crtimes;

/// Preserve metadata and request crtime preservation. `--crtimes` (`-N`) is the
/// attribute under test; `--numeric-ids` keeps owner comparison host-stable.
const FLAGS: &[&str] = &["-rlptgoD", "--crtimes", "--numeric-ids"];

impl Check for Crtimes {
    fn name(&self) -> &'static str {
        "crtimes"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        // Support probe: the host's upstream rsync may be built without
        // `--crtimes` support, in which case it refuses the option at parse time
        // and exits non-zero. Nothing to compare against, so skip the matrix.
        if !upstream_supports_crtimes(ctx.upstream) {
            return vec![CheckOutcome::skip(
                self.name(),
                "support",
                "upstream rsync lacks --crtimes support",
            )];
        }

        // Support probe: birth times must be exposed by the work filesystem, or
        // there is nothing meaningful to compare.
        if !work_exposes_birth_times(ctx.work) {
            return vec![CheckOutcome::skip(
                self.name(),
                "support",
                "filesystem does not expose birth times",
            )];
        }

        let root = ctx.work.join("crtimes");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let expected = support::entry_count(&src);
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        ctx.transports
            .iter()
            .map(|&t| self.cell(ctx, t, &root, &flags, expected))
            .collect()
    }
}

impl Crtimes {
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        flags: &[String],
        expected: usize,
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
        match crtime_diff(&oc_dst, &up_dst) {
            Some(diff) => {
                if ctx.verbose {
                    dump_crtimes(&oc_dst, &up_dst);
                }
                CheckOutcome::fail(self.name(), label, diff)
            }
            None => CheckOutcome::pass(self.name(), label),
        }
    }
}

/// First birth-time divergence between two trees, or `None` when every entry's
/// `stat -c %W` matches. Missing values compare equal only when both are absent.
fn crtime_diff(oc: &Path, up: &Path) -> Option<String> {
    for rel in support::rel_entries(oc) {
        let oc_bt = crtime_of(&oc.join(&rel));
        let up_bt = crtime_of(&up.join(&rel));
        if oc_bt != up_bt {
            return Some(format!(
                "crtime differs at {}: oc={} upstream={}",
                rel.display(),
                oc_bt.as_deref().unwrap_or("?"),
                up_bt.as_deref().unwrap_or("?"),
            ));
        }
    }
    None
}

/// Birth time of `path` as reported by `stat -c %W`, or `None` when the stat
/// call fails (e.g. the path is missing).
fn crtime_of(path: &Path) -> Option<String> {
    support::capture("stat", &["-c", "%W", &path.to_string_lossy()]).ok()
}

/// Print each entry's oc/upstream birth time for verbose diagnostics.
fn dump_crtimes(oc: &Path, up: &Path) {
    for rel in support::rel_entries(oc) {
        let oc_bt = crtime_of(&oc.join(&rel));
        let up_bt = crtime_of(&up.join(&rel));
        eprintln!(
            "    {}: oc={} upstream={}",
            rel.display(),
            oc_bt.as_deref().unwrap_or("?"),
            up_bt.as_deref().unwrap_or("?"),
        );
    }
}

/// Probe whether the host's upstream rsync was built with `--crtimes` (`-N`)
/// support. A build without it refuses the option at parse time (printing
/// `This rsync does not support --crtimes (-N)` and exiting non-zero) before
/// `--version` short-circuits, so a clean `--crtimes --version` run succeeds
/// only when the option is accepted. upstream: options.c:1028
/// `parse_one_refuse_match(0, "crtimes", ...)` under `#ifndef SUPPORT_CRTIMES`.
fn upstream_supports_crtimes(upstream: &Path) -> bool {
    std::process::Command::new(upstream)
        .args(["--crtimes", "--version"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Probe whether the work filesystem exposes birth times. Writes a throwaway
/// file, reads its `stat -c %W`, and treats an empty, `-`, or `0` value as "not
/// exposed" - the same convention `stat` uses for filesystems without crtime.
fn work_exposes_birth_times(work: &Path) -> bool {
    let probe = work.join(".crtimes-probe");
    if std::fs::write(&probe, b"probe").is_err() {
        return false;
    }
    let raw = support::capture("stat", &["-c", "%W", &probe.to_string_lossy()]).ok();
    let _ = std::fs::remove_file(&probe);
    raw.map(|v| birth_exposed(&v)).unwrap_or(false)
}

/// True when a `stat -c %W` value denotes an exposed birth time. Empty, `-`, and
/// `0` all mean the filesystem or kernel does not report one.
fn birth_exposed(raw: &str) -> bool {
    let v = raw.trim();
    !(v.is_empty() || v == "-" || v == "0")
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

/// Build the crtimes fixture. Idempotent: removes any prior tree first. A couple
/// of files plus a subdirectory holding a file, with backdated mtimes so the
/// quick-check does not skip the transfer (mtime is orthogonal to crtime).
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;

    std::fs::write(src.join("alpha"), b"alpha").map_err(|e| e.to_string())?;
    std::fs::write(src.join("bravo"), b"bravo").map_err(|e| e.to_string())?;
    std::fs::write(sub.join("charlie"), b"charlie").map_err(|e| e.to_string())?;

    // Backdate mtimes so the quick-check does not skip anything under test.
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
    use super::birth_exposed;

    #[test]
    fn birth_exposed_treats_unset_values_as_not_exposed() {
        // `stat -c %W` reports 0, -, or nothing when the fs lacks birth times.
        assert!(!birth_exposed("0"));
        assert!(!birth_exposed("-"));
        assert!(!birth_exposed(""));
        assert!(!birth_exposed("  0\n"));
    }

    #[test]
    fn birth_exposed_true_for_real_epoch_value() {
        assert!(birth_exposed("1614830767"));
        assert!(birth_exposed("  1614830767\n"));
    }
}
