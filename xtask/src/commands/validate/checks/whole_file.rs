//! Whole-file vs delta transfer parity between oc-rsync and upstream.
//!
//! The choice between rsync's delta algorithm (`--no-whole-file`) and a plain
//! whole-file copy (`-W`) affects only what crosses the wire: the delivered
//! destination bytes must be identical either way, and identical to upstream's.
//! This check exercises oc-rsync's delta path against upstream by pre-seeding
//! each destination with a *basis* `data.bin` that shares long common runs with
//! the source but differs in a few scattered regions - so `--no-whole-file`
//! forces a real delta transfer with genuine block matches and literal spans,
//! not a trivial no-op. `-I` (ignore-times) makes the transfer run over that
//! basis regardless of size/mtime.
//!
//! Because the destination is pre-seeded, this check cannot use
//! `transport::pull_into` (which wipes the destination first). It builds the
//! client `Command` directly - reusing `pull_into`'s per-transport operand
//! forms - and transfers into the seeded tree without resetting it. The oc and
//! upstream destinations are seeded with the same basis bytes, so only the
//! client under test varies. Both scenarios (`delta`, `whole`) run per transport;
//! the ssh transports are skipped when no sshd answers on localhost:22.

use std::path::Path;
use std::process::{Command, Output};

use crate::commands::validate::support;
use crate::commands::validate::transport::{DaemonHandle, Transport};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The whole-file vs delta delivered-bytes parity check.
pub struct WholeFile;

/// Length of the deterministic `data.bin` fixture (256 KiB): large enough to
/// span many delta blocks so the algorithm has real matches and literals.
const DATA_LEN: usize = 256 * 1024;

/// A fixed epoch (2021-03-04) applied to the source fixture.
const MTIME: &str = "@1614830767";

/// One transfer scenario: a human label and the rsync flag set that selects the
/// delta path or the whole-file path. `-I` forces the transfer to run over the
/// pre-seeded basis in both.
struct Scenario {
    /// Short label used in the cell name.
    label: &'static str,
    /// Complete rsync flag set for this scenario.
    flags: &'static [&'static str],
}

/// The two scenarios: force the delta algorithm, then force a whole-file copy.
/// Both must deliver destination bytes identical to the source and to upstream.
const SCENARIOS: &[Scenario] = &[
    Scenario {
        label: "delta",
        flags: &["-rlptgoD", "--no-whole-file", "-I", "--numeric-ids"],
    },
    Scenario {
        label: "whole",
        flags: &["-rlptgoD", "-W", "-I", "--numeric-ids"],
    },
];

/// Scattered byte windows flipped in the basis relative to the source. Each is
/// an `(offset, len)` region fully inside [`DATA_LEN`]; flipping a handful keeps
/// long common runs between them so the delta path finds matches *and* literals.
const FLIP_WINDOWS: &[(usize, usize)] = &[
    (1_024, 64),
    (40_960, 128),
    (131_072, 96),
    (200_000, 256),
    (262_000, 48),
];

impl Check for WholeFile {
    fn name(&self) -> &'static str {
        "whole-file"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("whole-file");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let mut outcomes = Vec::new();
        for &transport in ctx.transports {
            for scenario in SCENARIOS {
                outcomes.push(self.cell(ctx, transport, scenario, &root, &src));
            }
        }
        outcomes
    }
}

impl WholeFile {
    /// Run one `(transport, scenario)` cell: seed both destinations with the same
    /// basis, transfer with each client, and require both destinations end
    /// byte-identical to the source and to each other.
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        scenario: &Scenario,
        root: &Path,
        src: &Path,
    ) -> CheckOutcome {
        let label = format!("{} {}", transport.label(), scenario.label);
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }

        let basis = match basis_bytes(src) {
            Ok(bytes) => bytes,
            Err(e) => return CheckOutcome::skip(self.name(), label, e),
        };

        let up_dst = root.join(format!("up-{}-{}", transport.label(), scenario.label));
        let oc_dst = root.join(format!("oc-{}-{}", transport.label(), scenario.label));
        if let Err(e) = seed_dest(&basis, &up_dst) {
            return CheckOutcome::skip(self.name(), label, format!("seed upstream dest: {e}"));
        }
        if let Err(e) = seed_dest(&basis, &oc_dst) {
            return CheckOutcome::skip(self.name(), label, format!("seed oc dest: {e}"));
        }

        // Non-vacuous guard: the seeded basis must really differ from the source
        // before the transfer, or the delta path is not exercised over a basis.
        let source = std::fs::read(src.join("data.bin")).ok();
        let seeded = std::fs::read(oc_dst.join("data.bin")).ok();
        if seeded.is_none() || seeded == source {
            return CheckOutcome::fail(
                self.name(),
                label,
                "seed guard: dest data.bin did not differ from source basis",
            );
        }

        let _up = match run_transfer(
            ctx.upstream,
            ctx.upstream,
            transport.for_upstream(),
            scenario.flags,
            src,
            &up_dst,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), label, "upstream", other),
        };
        let _oc = match run_transfer(
            ctx.oc,
            ctx.upstream,
            transport,
            scenario.flags,
            src,
            &oc_dst,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), label, "oc", other),
        };

        // Delivered bytes must match the source and upstream regardless of
        // whether the delta or whole-file path moved them.
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

        CheckOutcome::pass(self.name(), label)
    }
}

/// Read the source `data.bin` and derive the basis by flipping [`FLIP_WINDOWS`].
fn basis_bytes(src: &Path) -> Result<Vec<u8>, String> {
    let source = std::fs::read(src.join("data.bin")).map_err(|e| e.to_string())?;
    Ok(mutate_basis(&source))
}

/// Build the transfer command for one cell without touching the destination.
///
/// Mirrors `transport::pull_into`'s operand forms - local copy, ssh subprocess,
/// russh `ssh://` URL, or an upstream `rsync://` daemon - but omits the
/// destination reset so the pre-seeded basis survives into the transfer.
fn run_transfer(
    client: &Path,
    upstream: &Path,
    transport: Transport,
    flags: &[&str],
    src: &Path,
    dst: &Path,
    work: &Path,
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
    label: String,
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

/// Deterministic byte generator: `data.bin`'s content derived purely from the
/// index, with no system randomness. Each byte mixes the index through a couple
/// of cheap operations so the stream is non-constant and non-periodic enough for
/// the rolling checksum to distinguish blocks. Reproducible across runs.
fn deterministic_bytes(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let i = i as u64;
        let mixed = i
            .wrapping_mul(2_654_435_761)
            .wrapping_add(i >> 3)
            .rotate_left((i & 31) as u32);
        out.push((mixed ^ (mixed >> 17)) as u8);
    }
    out
}

/// Derive a basis from the source bytes by flipping [`FLIP_WINDOWS`]. The result
/// has the same length and is byte-identical outside those windows, so it shares
/// long common runs with the source (delta matches) while differing inside them
/// (delta literals). Windows past the end are clamped and safely ignored.
fn mutate_basis(source: &[u8]) -> Vec<u8> {
    let mut basis = source.to_vec();
    let len = basis.len();
    for &(offset, window) in FLIP_WINDOWS {
        let start = offset.min(len);
        let end = offset.saturating_add(window).min(len);
        for byte in basis.iter_mut().take(end).skip(start) {
            *byte ^= 0xFF;
        }
    }
    basis
}

/// Build the source tree: a deterministic ~256 KiB `data.bin` plus a small
/// `sub/note.txt`, with mtimes backdated to [`MTIME`]. Idempotent: removes any
/// prior tree first.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(src.join("sub")).map_err(|e| e.to_string())?;
    std::fs::write(src.join("data.bin"), deterministic_bytes(DATA_LEN))
        .map_err(|e| e.to_string())?;
    std::fs::write(src.join("sub/note.txt"), b"whole-file fixture note\n")
        .map_err(|e| e.to_string())?;
    backdate(&src.join("data.bin"))?;
    backdate(&src.join("sub/note.txt"))
}

/// Pre-seed a destination with the basis `data.bin` (and nothing else, so the
/// transfer creates `sub/note.txt` fresh). Recreates `dst` empty first.
fn seed_dest(basis: &[u8], dst: &Path) -> Result<(), String> {
    if dst.exists() {
        std::fs::remove_dir_all(dst).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    std::fs::write(dst.join("data.bin"), basis).map_err(|e| e.to_string())
}

/// Set one path's mtime to [`MTIME`] via `touch`.
fn backdate(path: &Path) -> Result<(), String> {
    support::capture("touch", &["-h", "-d", MTIME, &path.to_string_lossy()])
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{DATA_LEN, FLIP_WINDOWS, deterministic_bytes, mutate_basis};

    #[test]
    fn generator_is_deterministic_and_non_constant() {
        let a = deterministic_bytes(4_096);
        let b = deterministic_bytes(4_096);
        assert_eq!(a, b, "generator must be reproducible");
        assert_eq!(a.len(), 4_096);
        assert!(
            a.iter().any(|&x| x != a[0]),
            "stream must not be a single repeated byte"
        );
    }

    #[test]
    fn basis_shares_long_runs_but_differs_in_flip_windows() {
        let source = deterministic_bytes(DATA_LEN);
        let basis = mutate_basis(&source);
        assert_eq!(basis.len(), source.len(), "basis keeps source length");
        assert_ne!(basis, source, "basis must differ from source");

        // Inside each in-range window every byte is flipped; outside all match.
        let mut flipped = vec![false; source.len()];
        for &(offset, len) in FLIP_WINDOWS {
            let end = (offset + len).min(source.len());
            for (i, f) in flipped.iter_mut().enumerate().take(end).skip(offset) {
                assert_ne!(basis[i], source[i], "byte {i} in a flip window must differ");
                *f = true;
            }
        }
        for (i, f) in flipped.iter().enumerate() {
            if !f {
                assert_eq!(basis[i], source[i], "byte {i} outside windows must match");
            }
        }
    }

    #[test]
    fn out_of_range_window_is_ignored_not_panicking() {
        // A short buffer smaller than every FLIP_WINDOW offset must not panic and
        // must come back unchanged (all windows clamp away).
        let short = deterministic_bytes(8);
        assert_eq!(mutate_basis(&short), short);
    }
}
