//! Phase-2 verification-redo fidelity for `--append-verify`.
//!
//! `--append-verify` re-checksums the bytes already present in the destination
//! after appending the tail. When that re-checksum fails the receiver keeps the
//! partial update, warns, and asks the generator to redo the file in phase 2
//! (upstream `receiver.c:1070` `case 0:` -> `send_msg_int(MSG_REDO, ndx)`). The
//! sibling `append-inplace` check seeds a *matching* prefix, so its re-checksum
//! succeeds and the redo path is never entered; this check seeds a destination
//! that is both shorter than the source and wrong, which forces the redo on
//! every transport.
//!
//! Like `append-inplace` this cannot use `transport::pull_into` /
//! `transport::push_into` (both recreate the destination empty) - the seed is
//! the whole point. It seeds each destination directly, builds the client
//! `Command` per transport and direction, and runs the transfer without wiping
//! the seed.
//!
//! Four things are asserted per cell, because the redo can diverge in more
//! places than the final bytes:
//!
//! 1. oc's exit code equals upstream's.
//! 2. the destination is byte-identical to the source *and* to upstream's.
//! 3. the `failed verification` warning lines on stderr are exactly upstream's,
//!    at each verbosity - upstream gates the line behind `INFO_GTE(NAME, 1)`
//!    (`receiver.c:1072`), so it is silent by default and prints the bare
//!    *relative* name under `-v`.
//! 4. the `--stats` literal/matched split matches, so a redo that re-sends the
//!    whole file as literal instead of re-deltaing against the retained basis is
//!    caught.
//!
//! The oc and upstream destinations are seeded identically, so only the client
//! under test varies. Upstream rsync is the ground truth. The ssh transports are
//! skipped when no sshd answers on localhost:22.

use std::path::Path;
use std::process::{Command, Output};

use crate::commands::validate::comparison;
use crate::commands::validate::support;
use crate::commands::validate::transport::{DaemonHandle, Transport};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The `--append-verify` phase-2 redo parity check.
pub struct VerifyRedo;

/// Length of the source `payload.bin`, in bytes (200 KiB).
const SOURCE_LEN: usize = 200 * 1024;

/// Length of the seeded destination `payload.bin`, in bytes (100 KiB). Shorter
/// than the source, so `--append-verify` appends a tail, and all zeros, so the
/// prefix is wrong and the re-checksum fails.
const SEED_LEN: usize = 100 * 1024;

/// Flags common to every cell. `-a` is the archive set the redo is normally hit
/// under; `--ignore-times` stops the quick-check from short-circuiting the
/// decision; `--numeric-ids` keeps id mapping out of the picture; `--stats`
/// prints the literal/matched split assertion 4 compares.
const BASE_FLAGS: &[&str] = &[
    "-a",
    "--append-verify",
    "--ignore-times",
    "--numeric-ids",
    "--stats",
];

/// The exact warning upstream rsync 3.4.4 writes to stderr when the
/// `--append-verify` re-checksum fails and the update is retained for a redo.
/// Measured identical on local, daemon pull, and daemon push. Note the *bare
/// relative* name: the receiver has already chdir'd into the destination, and
/// upstream renders `fname`, never an absolute path.
const WARNING: &str =
    "WARNING: payload.bin failed verification -- update retained (will try again).";

/// `--stats` label whose value proves the redo actually happened: the single
/// regular file is transferred twice, once per phase.
const TRANSFERRED_LABEL: &str = "Number of regular files transferred";

/// Value [`TRANSFERRED_LABEL`] must carry once the redo has run.
const REDO_TRANSFER_COUNT: &str = "2";

/// The delta-accounting fields assertion 4 compares. A redo that re-sends the
/// whole file as literal shows up here as a literal/matched split that differs
/// from upstream's re-delta against the retained basis.
const SPLIT_LABELS: &[&str] = &["Literal data", "Matched data"];

/// Transfer directions exercised, in report order.
const DIRECTIONS: &[Direction] = &[Direction::Pull, Direction::Push];

/// Verbosities exercised, in report order.
const VERBOSITIES: &[Verbosity] = &[Verbosity::Default, Verbosity::Verbose];

/// Which side of the transfer the client under test is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// Client is the receiver, and so the side that detects the failed
    /// verification and requests the redo.
    Pull,
    /// Client is the sender, and so the side that must re-send in phase 2.
    Push,
}

impl Direction {
    /// Stable direction label used in the cell name.
    const fn label(self) -> &'static str {
        match self {
            Direction::Pull => "pull",
            Direction::Push => "push",
        }
    }
}

/// Client verbosity, which is what gates upstream's warning line.
#[derive(Clone, Copy)]
enum Verbosity {
    /// No `-v`: upstream's `FWARNING` line is suppressed entirely.
    Default,
    /// `-v`: `INFO_GTE(NAME, 1)` holds, so upstream prints the warning.
    Verbose,
}

impl Verbosity {
    /// Stable verbosity label used in the cell name.
    const fn label(self) -> &'static str {
        match self {
            Verbosity::Default => "default",
            Verbosity::Verbose => "verbose",
        }
    }

    /// Extra flags this verbosity adds to [`BASE_FLAGS`].
    const fn flags(self) -> &'static [&'static str] {
        match self {
            Verbosity::Default => &[],
            Verbosity::Verbose => &["-v"],
        }
    }

    /// The `failed verification` lines upstream emits at this verbosity.
    ///
    /// Upstream prints the line only when `msgtype == FERROR_XFER ||
    /// INFO_GTE(NAME, 1) || stdout_format_has_i` (`receiver.c:1072`). The first
    /// failure is a `FWARNING` and the redo then succeeds, so nothing reaches
    /// the ungated `FERROR_XFER` form: without `-v` the transfer is silent, and
    /// with `-v` it emits exactly [`WARNING`].
    const fn expected_warnings(self) -> &'static [&'static str] {
        match self {
            Verbosity::Default => &[],
            Verbosity::Verbose => &[WARNING],
        }
    }
}

impl Check for VerifyRedo {
    fn name(&self) -> &'static str {
        "verify-redo"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("verify-redo");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }

        let mut outcomes = Vec::new();
        for &transport in ctx.transports {
            for &direction in DIRECTIONS {
                // A local push runs the same local-copy code path as a local
                // pull, so it would add a duplicate cell, not coverage.
                if transport == Transport::Local && direction == Direction::Push {
                    continue;
                }
                for &verbosity in VERBOSITIES {
                    outcomes.push(self.cell(ctx, transport, &root, &src, direction, verbosity));
                }
            }
        }
        outcomes
    }
}

impl VerifyRedo {
    /// Run one `(transport, direction, verbosity)` cell: seed both destinations,
    /// transfer with each client, and assert exit code, bytes, warning lines,
    /// and delta split all match upstream.
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        src: &Path,
        direction: Direction,
        verbosity: Verbosity,
    ) -> CheckOutcome {
        let label = format!(
            "{} {} {}",
            transport.label(),
            direction.label(),
            verbosity.label()
        );
        let stem = format!(
            "{}-{}-{}",
            transport.label(),
            direction.label(),
            verbosity.label()
        );
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }

        let oc_dst = root.join(format!("oc-{stem}"));
        let up_dst = root.join(format!("up-{stem}"));
        for dst in [&up_dst, &oc_dst] {
            if let Err(e) = seed_dest(dst) {
                return CheckOutcome::skip(
                    self.name(),
                    label.as_str(),
                    format!("seed {}: {e}", dst.display()),
                );
            }
        }

        // Non-vacuous guard: the seed must really be shorter than and different
        // from the source, or the re-checksum succeeds, the redo never runs, and
        // every assertion below passes on nothing.
        let source = source_bytes(SOURCE_LEN);
        match std::fs::read(oc_dst.join("payload.bin")) {
            Ok(seeded) if seeded.len() < source.len() && seeded != source => {}
            _ => {
                return CheckOutcome::fail(
                    self.name(),
                    label.as_str(),
                    "seed guard: dest payload.bin is not a shorter, differing file",
                );
            }
        }

        let flags: Vec<String> = BASE_FLAGS
            .iter()
            .chain(verbosity.flags())
            .map(|s| (*s).to_string())
            .collect();
        let run = |transport, client, dst: &Path| {
            Run {
                transport,
                direction,
                client,
                upstream: ctx.upstream,
                src,
                dst,
                work: ctx.work,
                flags: &flags,
            }
            .execute()
        };

        let up = match run(transport.for_upstream(), ctx.upstream, &up_dst) {
            Ok(out) if out.status.success() => out,
            other => return comparison::classify_failure(self.name(), &label, "upstream", other),
        };
        // oc's status is deliberately not required to succeed: a non-zero exit
        // is a real divergence, reported by the exit-code facet below rather
        // than swallowed as an unrunnable cell.
        let oc = match run(transport, ctx.oc, &oc_dst) {
            Ok(out) => out,
            Err(e) => {
                return CheckOutcome::skip(
                    self.name(),
                    label.as_str(),
                    format!("oc could not run: {e}"),
                );
            }
        };

        let up_out = String::from_utf8_lossy(&up.stdout);
        let oc_out = String::from_utf8_lossy(&oc.stdout);
        let up_err = String::from_utf8_lossy(&up.stderr);
        let oc_err = String::from_utf8_lossy(&oc.stderr);
        if ctx.verbose {
            eprintln!("[verify-redo/{label}] oc stderr={oc_err:?} upstream stderr={up_err:?}");
        }

        // Assertion 1: exit-code parity.
        if let Some(d) = comparison::exit_code_diff(&oc, &up) {
            return CheckOutcome::fail(self.name(), label.as_str(), d);
        }

        // Assertion 2: both destinations equal the source, and each other.
        if let Some(d) = support::content_diff(&up_dst, src) {
            return CheckOutcome::fail(
                self.name(),
                label.as_str(),
                format!("upstream dest != source: {d}"),
            );
        }
        if let Some(d) = support::content_diff(&oc_dst, src) {
            return CheckOutcome::fail(
                self.name(),
                label.as_str(),
                format!("oc dest != source: {d}"),
            );
        }
        if let Some(d) = support::content_diff(&oc_dst, &up_dst) {
            return CheckOutcome::fail(
                self.name(),
                label.as_str(),
                format!("oc dest != upstream dest: {d}"),
            );
        }

        // Non-vacuous guard: upstream must have transferred the one regular file
        // twice, which is the redo. If it did not, the fixture stopped forcing
        // the verification failure and assertions 3 and 4 are worthless.
        let up_transferred = stat_field(&up_out, TRANSFERRED_LABEL);
        if up_transferred.as_deref() != Some(REDO_TRANSFER_COUNT) {
            return CheckOutcome::fail(
                self.name(),
                label.as_str(),
                format!(
                    "redo guard: upstream {TRANSFERRED_LABEL} = {} (want {REDO_TRANSFER_COUNT})",
                    up_transferred.as_deref().unwrap_or("(absent)")
                ),
            );
        }

        // Assertion 3: the warning lines are exactly upstream's, and upstream's
        // are exactly the documented oracle for this verbosity.
        let up_warnings = warning_lines(&up_err);
        let oc_warnings = warning_lines(&oc_err);
        if up_warnings != verbosity.expected_warnings() {
            return CheckOutcome::fail(
                self.name(),
                label.as_str(),
                format!(
                    "oracle drift: upstream warnings {up_warnings:?} != {:?}",
                    verbosity.expected_warnings()
                ),
            );
        }
        if oc_warnings != up_warnings {
            return CheckOutcome::fail(
                self.name(),
                label.as_str(),
                format!("warning lines: oc {oc_warnings:?} != upstream {up_warnings:?}"),
            );
        }

        // Assertion 4: the literal/matched split of the redo.
        for &field in SPLIT_LABELS {
            let up_val = stat_field(&up_out, field);
            let oc_val = stat_field(&oc_out, field);
            if oc_val != up_val {
                return CheckOutcome::fail(
                    self.name(),
                    label.as_str(),
                    format!(
                        "{field}: oc={} upstream={}",
                        oc_val.as_deref().unwrap_or("(absent)"),
                        up_val.as_deref().unwrap_or("(absent)")
                    ),
                );
            }
        }

        CheckOutcome::pass(self.name(), label)
    }
}

/// One client invocation against an already-seeded destination.
///
/// Operand forms mirror `transport::pull_into` / `transport::push_into`, minus
/// their destination reset: `local` is a filesystem copy (identical either
/// direction, so `direction` is not consulted), `ssh-subprocess` uses
/// `-e ssh localhost:<path>`, `russh` uses an `ssh://` URL, and `daemon` uses a
/// module URL. The peer is always upstream, so only `client` varies between the
/// two runs of a cell.
struct Run<'a> {
    /// Transport the client speaks.
    transport: Transport,
    /// Whether the client is the receiver (pull) or the sender (push).
    direction: Direction,
    /// The rsync binary under test for this run.
    client: &'a Path,
    /// Upstream rsync, used as the peer on every network transport.
    upstream: &'a Path,
    /// Source tree.
    src: &'a Path,
    /// Pre-seeded destination tree; never reset here.
    dst: &'a Path,
    /// Scratch root, for the daemon config file.
    work: &'a Path,
    /// Complete client flag set.
    flags: &'a [String],
}

impl Run<'_> {
    /// Build and run the client command, returning its captured output.
    ///
    /// Unlike `append-inplace` the daemon is started per run rather than once
    /// per cell: a push must export the *client's own* destination, so the
    /// module differs between the upstream and the oc run and cannot be shared.
    /// The handle is a local, so it stays alive across `output()` and is killed
    /// on return.
    fn execute(&self) -> TaskResult<Output> {
        let src_arg = format!("{}/", self.src.display());
        let dst_arg = format!("{}/", self.dst.display());
        let mut cmd = Command::new(self.client);
        cmd.args(self.flags);

        let daemon = match (self.transport, self.direction) {
            (Transport::Daemon, Direction::Pull) => {
                Some(DaemonHandle::start(self.upstream, self.src, self.work)?)
            }
            (Transport::Daemon, Direction::Push) => Some(DaemonHandle::start_writable(
                self.upstream,
                self.dst,
                self.work,
            )?),
            _ => None,
        };

        match self.transport {
            Transport::Local => {
                cmd.arg(&src_arg).arg(&dst_arg);
            }
            Transport::SshSubprocess => {
                cmd.arg(format!("--rsync-path={}", self.upstream.display()))
                    .arg("-e")
                    .arg("ssh -o BatchMode=yes -o StrictHostKeyChecking=no");
                match self.direction {
                    Direction::Pull => cmd.arg(format!("localhost:{src_arg}")).arg(&dst_arg),
                    Direction::Push => cmd.arg(&src_arg).arg(format!("localhost:{dst_arg}")),
                };
            }
            Transport::Russh => {
                cmd.arg(format!("--rsync-path={}", self.upstream.display()));
                match self.direction {
                    Direction::Pull => cmd.arg(format!("ssh://localhost{src_arg}")).arg(&dst_arg),
                    Direction::Push => cmd.arg(&src_arg).arg(format!("ssh://localhost{dst_arg}")),
                };
            }
            Transport::Daemon => {
                let url = daemon
                    .as_ref()
                    .ok_or_else(|| {
                        TaskError::Validation("daemon transport without a daemon".into())
                    })?
                    .module_url();
                match self.direction {
                    Direction::Pull => cmd.arg(url).arg(&dst_arg),
                    Direction::Push => cmd.arg(&src_arg).arg(url),
                };
            }
        }

        cmd.output()
            .map_err(|e| TaskError::Validation(format!("failed to spawn {cmd:?}: {e}")))
    }
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

/// Build the source tree: a single 200 KiB deterministic `payload.bin`.
/// Idempotent: removes any prior tree first.
///
/// Unlike the sibling reuse checks this fixture is deliberately *not* backdated
/// with `touch -d @epoch`. The transfer decision here is forced by the differing
/// size and by `--ignore-times`, and every assertion (bytes, exit code, warning
/// lines, delta split) is mtime-independent - so stamping a fixed mtime would
/// buy nothing while making the whole check skip on hosts whose `touch` is the
/// BSD one and rejects `-d @seconds`.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(src).map_err(|e| e.to_string())?;
    std::fs::write(src.join("payload.bin"), source_bytes(SOURCE_LEN)).map_err(|e| e.to_string())
}

/// Recreate `dst` holding only the wrong, short `payload.bin` seed.
fn seed_dest(dst: &Path) -> TaskResult<()> {
    if dst.exists() {
        std::fs::remove_dir_all(dst)
            .map_err(|e| TaskError::Validation(format!("remove {}: {e}", dst.display())))?;
    }
    std::fs::create_dir_all(dst)
        .map_err(|e| TaskError::Validation(format!("create {}: {e}", dst.display())))?;
    std::fs::write(dst.join("payload.bin"), vec![0u8; SEED_LEN])
        .map_err(|e| TaskError::Validation(format!("seed payload.bin: {e}")))
}

/// The `failed verification` lines on `stderr`, in order, with trailing
/// whitespace trimmed. Unrelated stderr noise (ssh banners, chown warnings) is
/// filtered out so the comparison carries only the redo's own reporting.
fn warning_lines(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .map(str::trim_end)
        .filter(|line| line.contains("failed verification"))
        .collect()
}

/// Value of one `--stats` label, or `None` when the label is absent. Grouping
/// commas and a trailing ` bytes` unit are stripped, so `Literal data: 205,300
/// bytes` reads back as `205300`.
fn stat_field(stdout: &str, label: &str) -> Option<String> {
    stdout.lines().find_map(|line| {
        let (found, rest) = line.split_once(':')?;
        if found.trim() != label {
            return None;
        }
        let trimmed = rest.trim();
        let unitless = trimmed.strip_suffix("bytes").unwrap_or(trimmed).trim_end();
        Some(unitless.chars().filter(|c| *c != ',').collect())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The upstream `--stats` block measured for this fixture (rsync 3.4.4).
    const SAMPLE: &str = "\nNumber of files: 2 (reg: 1, dir: 1)\n\
        Number of created files: 0\n\
        Number of regular files transferred: 2\n\
        Total file size: 204,800 bytes\n\
        Literal data: 205,300 bytes\n\
        Matched data: 101,900 bytes\n\
        Total bytes received: 206,086\n";

    #[test]
    fn source_bytes_are_deterministic_and_index_derived() {
        assert_eq!(source_bytes(SOURCE_LEN), source_bytes(SOURCE_LEN));
        assert_eq!(source_bytes(SOURCE_LEN).len(), SOURCE_LEN);
        // Distinct indices are not all mapped to one constant byte.
        let head = source_bytes(256);
        assert!(head.iter().any(|&b| b != head[0]));
    }

    #[test]
    fn seed_is_shorter_than_and_differs_from_the_source() {
        // Both properties are load-bearing: shorter makes --append-verify append
        // a tail, differing makes the re-checksum fail and force the redo.
        let source = source_bytes(SOURCE_LEN);
        let seed = vec![0u8; SEED_LEN];
        assert!(seed.len() < source.len());
        assert_ne!(seed.as_slice(), &source[..SEED_LEN]);
    }

    #[test]
    fn stat_field_strips_commas_and_the_bytes_unit() {
        assert_eq!(
            stat_field(SAMPLE, "Literal data").as_deref(),
            Some("205300")
        );
        assert_eq!(
            stat_field(SAMPLE, "Matched data").as_deref(),
            Some("101900")
        );
        assert_eq!(stat_field(SAMPLE, "File list size"), None);
    }

    #[test]
    fn redo_guard_reads_two_transfers_from_the_measured_block() {
        // The guard is what keeps the check non-vacuous: one regular file
        // counted twice is the phase-2 redo.
        assert_eq!(
            stat_field(SAMPLE, TRANSFERRED_LABEL).as_deref(),
            Some(REDO_TRANSFER_COUNT),
            "fixture must transfer payload.bin twice"
        );
    }

    #[test]
    fn warning_lines_keep_only_the_verification_line() {
        let stderr = format!(
            "Warning: Permanently added 'localhost' to the list of known hosts.\n{WARNING}\n"
        );
        assert_eq!(warning_lines(&stderr), vec![WARNING]);
        assert!(warning_lines("").is_empty());
    }

    #[test]
    fn verbosity_oracle_matches_the_upstream_gate() {
        // upstream: receiver.c:1072 gates the FWARNING behind INFO_GTE(NAME, 1),
        // so the line exists only under -v; the redo then succeeds, so it never
        // escalates to the ungated FERROR_XFER form.
        assert!(Verbosity::Default.expected_warnings().is_empty());
        assert_eq!(Verbosity::Verbose.expected_warnings(), &[WARNING]);
        assert!(Verbosity::Default.flags().is_empty());
        assert_eq!(Verbosity::Verbose.flags(), &["-v"]);
    }

    #[test]
    fn warning_names_the_file_relatively() {
        // The divergence this check exists to catch is an absolute destination
        // path in place of upstream's bare `payload.bin`.
        assert!(WARNING.contains(" payload.bin failed verification"));
        assert!(!WARNING.contains('/'));
    }
}
