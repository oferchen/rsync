//! `--fuzzy` (`-y`) delivered-bytes parity between oc-rsync and upstream.
//!
//! `--fuzzy` lets rsync reuse a *similarly named* existing destination file as a
//! delta basis when the exact target is missing - e.g. transferring `data.txt`
//! into a directory that already holds `data.txt.bak`. Which basis rsync picks
//! only affects what crosses the wire; the delivered `data.txt` bytes must be
//! correct regardless, and oc's whole destination tree must match upstream's.
//! This check pre-seeds each destination with a fuzzy basis whose bytes share
//! long common runs with the source but differ in a few scattered regions - so
//! `--fuzzy` finds a real basis to delta against, not a trivial whole-file copy.
//! `-I` (ignore-times) forces the transfer to run rather than quick-check away.
//!
//! Because the destination is pre-seeded, this check cannot use
//! `transport::pull_into` (which wipes the destination first). It builds the
//! client `Command` directly - reusing `pull_into`'s per-transport operand
//! forms - and transfers into the seeded tree without resetting it. The oc and
//! upstream destinations are seeded with the same basis bytes, so only the
//! client under test varies. The ssh transports are skipped when no sshd answers
//! on localhost:22.
//!
//! Note: `--fuzzy` does not remove the basis, so both destinations keep
//! `data.txt.bak` after the transfer. The delivered `data.txt` is therefore
//! compared file-to-file against the source (a whole-tree compare would flag the
//! surviving basis), while the two destination trees - carrying the identical
//! basis - are compared to each other.

use std::path::Path;
use std::process::{Command, Output};

use crate::commands::validate::support;
use crate::commands::validate::transport::{DaemonHandle, Transport};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The `--fuzzy` delivered-bytes parity check.
pub struct Fuzzy;

/// Recursive, fuzzy-basis matching, ignore-times, numeric ids.
const FLAGS: &[&str] = &["-rlptgoD", "--fuzzy", "-I", "--numeric-ids"];

/// The transfer target whose bytes must be delivered correctly.
const TARGET: &str = "data.txt";

/// The fuzzy basis pre-seeded into each destination: `TARGET` with a `.bak`
/// suffix. It shares the target's stem, so rsync's name-similarity scoring picks
/// it as the delta basis for `TARGET`; it is *not* the exact target name, so the
/// target itself is still absent and must be transferred.
const BASIS: &str = "data.txt.bak";

/// Length of the deterministic `data.txt` fixture (128 KiB): large enough to
/// span many delta blocks so the fuzzy basis yields real matches and literals.
const DATA_LEN: usize = 128 * 1024;

/// A fixed epoch (2021-03-04) applied to the source fixture.
const MTIME: &str = "@1614830767";

/// Scattered byte windows flipped in the basis relative to the source. Each is
/// an `(offset, len)` region inside [`DATA_LEN`]; flipping a handful keeps long
/// common runs between them so the fuzzy basis produces matches *and* literals.
const FLIP_WINDOWS: &[(usize, usize)] = &[
    (1_024, 64),
    (20_480, 128),
    (65_536, 96),
    (100_000, 256),
    (130_000, 48),
];

impl Check for Fuzzy {
    fn name(&self) -> &'static str {
        "fuzzy"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("fuzzy");
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

impl Fuzzy {
    /// Run one transport cell: seed both destinations with the same fuzzy basis,
    /// transfer with each client, and require the delivered `TARGET` to match the
    /// source and oc's whole tree to match upstream's.
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

        let basis = match basis_bytes(src) {
            Ok(bytes) => bytes,
            Err(e) => return CheckOutcome::skip(self.name(), label, e),
        };

        let up_dst = root.join(format!("up-{label}"));
        let oc_dst = root.join(format!("oc-{label}"));
        if let Err(e) = seed_dest(&basis, &up_dst) {
            return CheckOutcome::skip(self.name(), label, format!("seed upstream dest: {e}"));
        }
        if let Err(e) = seed_dest(&basis, &oc_dst) {
            return CheckOutcome::skip(self.name(), label, format!("seed oc dest: {e}"));
        }

        // Non-vacuous guard: the fuzzy basis must exist under a non-target name
        // and really differ from the source target before the transfer, or
        // `--fuzzy` has no basis to match and the path is not exercised.
        let source = std::fs::read(src.join(TARGET)).ok();
        let seeded_basis = std::fs::read(oc_dst.join(BASIS)).ok();
        if oc_dst.join(TARGET).exists() {
            return CheckOutcome::fail(
                self.name(),
                label,
                "seed guard: target was pre-seeded, so no fuzzy basis is needed",
            );
        }
        if seeded_basis.is_none() || seeded_basis == source {
            return CheckOutcome::fail(
                self.name(),
                label,
                "seed guard: fuzzy basis missing or identical to source target",
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

        // The delivered target must be byte-identical to the source, proving the
        // fuzzy basis was applied correctly (a whole-tree compare would flag the
        // surviving basis file, which `--fuzzy` intentionally leaves in place).
        let src_data = std::fs::read(src.join(TARGET)).ok();
        let oc_data = std::fs::read(oc_dst.join(TARGET)).ok();
        if oc_data.is_none() {
            return CheckOutcome::fail(self.name(), label, "oc did not deliver the target file");
        }
        if oc_data != src_data {
            return CheckOutcome::fail(self.name(), label, "oc target bytes != source");
        }

        // oc's whole destination tree (delivered target + surviving basis + fresh
        // subtree) must match upstream's, which carried the identical basis.
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

/// Read the source `TARGET` and derive the fuzzy basis by flipping
/// [`FLIP_WINDOWS`].
fn basis_bytes(src: &Path) -> Result<Vec<u8>, String> {
    let source = std::fs::read(src.join(TARGET)).map_err(|e| e.to_string())?;
    Ok(mutate_basis(&source))
}

/// Build the transfer command for one cell without touching the destination.
///
/// Mirrors `transport::pull_into`'s operand forms - local copy, ssh subprocess,
/// russh `ssh://` URL, or an upstream `rsync://` daemon - but omits the
/// destination reset so the pre-seeded fuzzy basis survives into the transfer.
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

/// Deterministic byte generator: `TARGET`'s content derived purely from the
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

/// Derive a fuzzy basis from the source bytes by flipping [`FLIP_WINDOWS`]. The
/// result has the same length and is byte-identical outside those windows, so it
/// shares long common runs with the source (fuzzy matches) while differing inside
/// them (fuzzy literals). Windows past the end are clamped and safely ignored.
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

/// Build the source tree: a deterministic ~128 KiB `data.txt` plus a small
/// `sub/note.txt`, with mtimes backdated to [`MTIME`]. Idempotent: removes any
/// prior tree first.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(src.join("sub")).map_err(|e| e.to_string())?;
    std::fs::write(src.join(TARGET), deterministic_bytes(DATA_LEN)).map_err(|e| e.to_string())?;
    std::fs::write(src.join("sub/note.txt"), b"fuzzy fixture note\n").map_err(|e| e.to_string())?;
    backdate(&src.join(TARGET))?;
    backdate(&src.join("sub/note.txt"))
}

/// Pre-seed a destination with the fuzzy basis under [`BASIS`] (and nothing else,
/// so the target is absent and the subtree is created fresh). Recreates `dst`
/// empty first, so it is idempotent.
fn seed_dest(basis: &[u8], dst: &Path) -> Result<(), String> {
    if dst.exists() {
        std::fs::remove_dir_all(dst).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    std::fs::write(dst.join(BASIS), basis).map_err(|e| e.to_string())
}

/// Set one path's mtime to [`MTIME`] via `touch`.
fn backdate(path: &Path) -> Result<(), String> {
    support::capture("touch", &["-h", "-d", MTIME, &path.to_string_lossy()])
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{BASIS, DATA_LEN, FLIP_WINDOWS, TARGET, deterministic_bytes, mutate_basis};

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

    #[test]
    fn basis_name_shares_target_stem_but_is_not_the_target() {
        // The fuzzy basis carries the target's stem plus a `.bak` suffix, so
        // rsync's name-similarity scoring picks it while the target stays absent.
        assert_ne!(BASIS, TARGET, "basis must not be the exact target name");
        assert!(
            BASIS.starts_with(TARGET),
            "basis must share the target stem"
        );
        assert!(
            BASIS.ends_with(".bak"),
            "basis uses the `.bak` suffix scheme"
        );
    }
}
