//! `--append`, `--append-verify`, and `--inplace` delivery parity.
//!
//! All three flags reuse bytes already present in the destination instead of
//! writing a fresh temp file: `--append` and `--append-verify` extend a
//! destination that is a prefix of the source, and `--inplace` overwrites a
//! same-length destination in place. Each mode therefore only exercises its
//! reuse path when the destination is pre-seeded, so this check cannot use
//! `transport::pull_into` (which recreates the destination empty). It seeds each
//! destination directly, builds the client `Command` per transport, and runs the
//! transfer without wiping the seed - then asserts oc-rsync ends byte-identical
//! to both upstream and the source across every transport in `ctx.transports`.
//!
//! The oc and upstream destinations are seeded identically, so only the client
//! under test varies. Upstream rsync is the ground truth. The ssh transports are
//! skipped when no sshd answers on localhost:22.

use std::path::Path;
use std::process::{Command, Output};

use crate::commands::validate::support;
use crate::commands::validate::transport::{DaemonHandle, Transport};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The `--append` / `--append-verify` / `--inplace` delivery parity check.
pub struct AppendInplace;

/// Length of the source `growing.bin`, in bytes (128 KiB).
const FILE_LEN: usize = 128 * 1024;

/// Prefix length pre-seeded for the append scenarios (64 KiB); `--append` must
/// deliver the remaining tail.
const PREFIX_LEN: usize = 64 * 1024;

/// Byte ranges the inplace seed flips relative to the source, so a same-length
/// destination differs and `--inplace` must rewrite exactly these spans. Head,
/// interior, and tail are covered; every range lies within [`FILE_LEN`].
const MUTATION_RANGES: &[(usize, usize)] = &[(0, 16), (40_000, 40_512), (131_056, FILE_LEN)];

/// Contents of the second fixture file, transferred fresh under every scenario.
const NOTE: &[u8] = b"append/inplace fixture note\n";

/// A fixed epoch (2021-03-04) applied to source and seeded destination alike, so
/// mtimes match and only the flag under test drives the reuse decision.
const MTIME: &str = "@1614830767";

/// The three reuse modes, in report order.
const SCENARIOS: &[Scenario] = &[Scenario::Append, Scenario::AppendVerify, Scenario::Inplace];

/// One reuse mode: its flags and how it seeds the destination `growing.bin`.
#[derive(Clone, Copy)]
enum Scenario {
    /// `--append`: seed a prefix; the tail is appended.
    Append,
    /// `--append-verify`: seed a prefix; the tail is appended after re-checksum.
    AppendVerify,
    /// `--inplace`: seed a same-length but mutated copy; rewritten in place.
    Inplace,
}

impl Scenario {
    /// Stable scenario label used in the cell name.
    fn label(self) -> &'static str {
        match self {
            Scenario::Append => "append",
            Scenario::AppendVerify => "append-verify",
            Scenario::Inplace => "inplace",
        }
    }

    /// Complete flag set for the mode, including `-I` for inplace so the
    /// same-length seed is not skipped by the quick-check.
    fn flags(self) -> &'static [&'static str] {
        match self {
            Scenario::Append => &["-rlptgoD", "--append", "--numeric-ids"],
            Scenario::AppendVerify => &["-rlptgoD", "--append-verify", "--numeric-ids"],
            Scenario::Inplace => &["-rlptgoD", "--inplace", "-I", "--numeric-ids"],
        }
    }

    /// Destination `growing.bin` bytes to pre-seed for this mode. The append
    /// modes seed a shorter prefix; inplace seeds a mutated same-length copy.
    /// Both differ from `source`, so the reuse path is genuinely exercised.
    fn seed(self, source: &[u8]) -> Vec<u8> {
        match self {
            Scenario::Append | Scenario::AppendVerify => prefix(source, PREFIX_LEN),
            Scenario::Inplace => mutate(source),
        }
    }
}

impl Check for AppendInplace {
    fn name(&self) -> &'static str {
        "append-inplace"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("append-inplace");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let mut outcomes = Vec::new();
        for &transport in ctx.transports {
            for &scenario in SCENARIOS {
                outcomes.push(self.cell(ctx, transport, &root, &src, scenario));
            }
        }
        outcomes
    }
}

impl AppendInplace {
    /// Run one `(transport, scenario)` cell: seed both destinations, transfer
    /// with each client, and assert oc ends byte-identical to upstream and to
    /// the source.
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        src: &Path,
        scenario: Scenario,
    ) -> CheckOutcome {
        let label = format!("{} {}", transport.label(), scenario.label());
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }

        let source = source_bytes(FILE_LEN);
        let seed = scenario.seed(&source);
        let stem = format!("{}-{}", transport.label(), scenario.label());
        let oc_dst = root.join(format!("oc-{stem}"));
        let up_dst = root.join(format!("up-{stem}"));

        if let Err(e) = seed_dest(&oc_dst, &seed) {
            return CheckOutcome::skip(self.name(), label, format!("seed oc dest: {e}"));
        }
        if let Err(e) = seed_dest(&up_dst, &seed) {
            return CheckOutcome::skip(self.name(), label, format!("seed upstream dest: {e}"));
        }

        // Non-vacuous guard: the seeded destination must really differ from the
        // source before the transfer, or the reuse path is never taken.
        let seeded = std::fs::read(oc_dst.join("growing.bin")).ok();
        if seeded.is_none() || seeded.as_deref() == Some(source.as_slice()) {
            return CheckOutcome::fail(
                self.name(),
                label,
                "seed guard: dest growing.bin did not differ from source",
            );
        }

        // The daemon transport needs one live upstream daemon shared by both
        // client runs; keep it alive for the whole cell.
        let daemon = if transport == Transport::Daemon {
            match DaemonHandle::start(ctx.upstream, src, ctx.work) {
                Ok(handle) => Some(handle),
                Err(e) => return CheckOutcome::skip(self.name(), label, format!("daemon: {e}")),
            }
        } else {
            None
        };
        let daemon_url = daemon.as_ref().map(|d| d.module_url());
        let flags = scenario.flags();

        let up = match run_client(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            src,
            &up_dst,
            daemon_url.as_deref(),
            flags,
        ) {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), &label, "upstream", other),
        };
        let _ = up;
        let oc = match run_client(
            transport,
            ctx.oc,
            ctx.upstream,
            src,
            &oc_dst,
            daemon_url.as_deref(),
            flags,
        ) {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), &label, "oc", other),
        };
        let _ = oc;
        drop(daemon);

        // Both destinations must now equal the source, and each other.
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

/// Build one client rsync `Command` for `transport` and run it into the
/// already-seeded destination, which is never reset here so the reuse flags
/// operate on the pre-seeded bytes.
///
/// Operand forms mirror `transport::pull_into`: `local` is a filesystem copy;
/// `ssh-subprocess` uses `-e ssh localhost:<src>`; `russh` uses an `ssh://` URL;
/// `daemon` uses the module URL passed in `daemon_url`. The sender is always
/// `upstream` for the network transports.
fn run_client(
    transport: Transport,
    client: &Path,
    upstream: &Path,
    src: &Path,
    dst: &Path,
    daemon_url: Option<&str>,
    flags: &[&str],
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

/// Deterministic source byte at `index`, derived purely from the index by a
/// fixed integer hash - no system randomness, so the fixture is reproducible.
fn source_byte(index: usize) -> u8 {
    let x = (index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    ((x >> 24) ^ x) as u8
}

/// The first `len` deterministic source bytes.
fn source_bytes(len: usize) -> Vec<u8> {
    (0..len).map(source_byte).collect()
}

/// A leading prefix of `bytes` of length `n` (clamped to the slice length).
fn prefix(bytes: &[u8], n: usize) -> Vec<u8> {
    bytes[..n.min(bytes.len())].to_vec()
}

/// A same-length copy of `bytes` with every byte in [`MUTATION_RANGES`] flipped,
/// so an inplace destination differs from the source only in those spans.
fn mutate(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    for &(start, end) in MUTATION_RANGES {
        for b in out.iter_mut().take(end.min(bytes.len())).skip(start) {
            *b = !*b;
        }
    }
    out
}

/// Seed `dst` with `growing_seed` and backdate it, so the reuse flags see a
/// destination whose mtime matches the source. Recreates `dst` empty first, so
/// it is idempotent; `sub/note.txt` is left absent and transferred fresh.
fn seed_dest(dst: &Path, growing_seed: &[u8]) -> TaskResult<()> {
    reset_dir(dst)?;
    let file = dst.join("growing.bin");
    std::fs::write(&file, growing_seed)
        .map_err(|e| TaskError::Validation(format!("seed growing.bin: {e}")))?;
    backdate(&file)
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

/// Set `path`'s mtime to [`MTIME`] via `touch`.
fn backdate(path: &Path) -> TaskResult<()> {
    support::capture("touch", &["-h", "-d", MTIME, &path.to_string_lossy()]).map(|_| ())
}

/// Build the source tree: a 128 KiB deterministic `growing.bin` plus
/// `sub/note.txt`, both backdated. Idempotent: removes any prior tree first.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;
    std::fs::write(src.join("growing.bin"), source_bytes(FILE_LEN)).map_err(|e| e.to_string())?;
    std::fs::write(sub.join("note.txt"), NOTE).map_err(|e| e.to_string())?;
    for rel in ["growing.bin", "sub/note.txt"] {
        backdate(&src.join(rel)).map_err(|e| e.to_string())?;
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_bytes_are_deterministic_and_index_derived() {
        // Same length twice yields identical bytes; the generator uses no
        // system randomness, only the index.
        assert_eq!(source_bytes(FILE_LEN), source_bytes(FILE_LEN));
        assert_eq!(source_bytes(4).len(), 4);
        // Distinct indices are not all mapped to one constant byte.
        let first = source_bytes(256);
        assert!(first.iter().any(|&b| b != first[0]));
    }

    #[test]
    fn prefix_is_a_strict_leading_slice() {
        let full = source_bytes(FILE_LEN);
        let head = prefix(&full, PREFIX_LEN);
        assert_eq!(head.len(), PREFIX_LEN);
        assert_eq!(head.as_slice(), &full[..PREFIX_LEN]);
        // A prefix of the source differs from the whole source (append path).
        assert_ne!(head, full);
        // Over-long request clamps to the slice length.
        assert_eq!(prefix(&full, FILE_LEN * 2).len(), FILE_LEN);
    }

    #[test]
    fn mutate_keeps_length_flips_only_named_ranges() {
        let full = source_bytes(FILE_LEN);
        let changed = mutate(&full);
        assert_eq!(changed.len(), full.len());
        assert_ne!(changed, full);
        for i in 0..full.len() {
            let inside = MUTATION_RANGES.iter().any(|&(s, e)| i >= s && i < e);
            if inside {
                assert_eq!(changed[i], !full[i], "byte {i} must be flipped");
            } else {
                assert_eq!(changed[i], full[i], "byte {i} must be untouched");
            }
        }
    }

    #[test]
    fn every_scenario_seed_differs_from_source() {
        let full = source_bytes(FILE_LEN);
        for &scenario in SCENARIOS {
            assert_ne!(
                scenario.seed(&full),
                full,
                "{} seed must differ to exercise reuse",
                scenario.label()
            );
        }
    }
}
