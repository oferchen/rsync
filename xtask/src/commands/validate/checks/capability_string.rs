//! Server-args capability-string parity between oc-rsync and upstream.
//!
//! When rsync starts a remote-shell transfer it invokes `<rsh> <host> rsync
//! --server <compact-flags> . <path>`, where the compact-flags token carries the
//! protocol-32 capability suffix after a `.` - e.g. `-logDtpre.iLsfxCIvu`. The
//! suffix advertises the client's negotiable capabilities (checksum/compression
//! choice, incremental recursion, symlink-times, and so on) and must match
//! upstream byte-for-byte or a peer negotiates the wrong feature set.
//!
//! upstream: options.c:2216 `server_options()` builds `argstr` (the compact flag
//! string) and appends the capability letters; main.c:1187 `do_cmd()` execs the
//! remote shell with those args.
//!
//! The transfer is oriented as a PUSH (`src/ host:dst/`), so the client under
//! test is the sender and advertises the full capability set including the
//! inc-recurse `i` letter. This is the direction where the drop-in contract holds
//! byte-for-byte: on a PULL oc deliberately suppresses `i` because its receive
//! path clears CF_INC_RECURSE (compat.c:162 `set_allow_inc_recurse()`), so a pull
//! comparison would flag intended behaviour rather than a regression.
//!
//! This check observes the invocation directly: it runs each client with `-e`
//! pointed at a tiny capture shell that records the exact argv it is handed, then
//! extracts the compact-flags token and asserts oc's equals upstream's. It needs
//! no live peer - the capture shell exits immediately - so it is deterministic
//! and requires neither sshd nor a daemon.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use crate::commands::validate::support;
use crate::commands::validate::{Category, Check, CheckOutcome, ValidateCtx};

/// The server-args capability-string parity check.
pub struct CapabilityString;

/// Client flags whose compact encoding carries the capability suffix. `-rlptgoD`
/// (the `-a` expansion) is stable across versions and forces `--server --sender`
/// with the full capability string.
const CLIENT_FLAGS: &[&str] = &["-rlptgoD", "--numeric-ids"];

/// Substring every protocol-32 capability suffix contains. Guards against a
/// vacuous pass on a token that happens to hold a `.` but no capability letters.
///
/// upstream: compat.c - the negotiated suffix always includes the checksum
/// (`C`), compression (`f`/`x`), and inc-recurse (`i`) capability letters that
/// spell the `LsfxCIvu` family.
const CAPABILITY_MARKER: &str = "LsfxCIvu";

impl Check for CapabilityString {
    fn name(&self) -> &'static str {
        "capability-string"
    }

    fn categories(&self) -> &'static [Category] {
        &[Category::Wire]
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("capability-string");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "server-args", e)];
        }
        vec![self.cell(ctx, &root, &src)]
    }
}

impl CapabilityString {
    /// Capture each client's remote-shell argv, extract the compact-flags token,
    /// and compare oc's against upstream's.
    fn cell(&self, ctx: &ValidateCtx, root: &Path, src: &Path) -> CheckOutcome {
        let up = match capture_compact_flags(ctx.upstream, root, src, "up") {
            Ok(Some(t)) => t,
            Ok(None) => {
                return CheckOutcome::skip(
                    self.name(),
                    "server-args",
                    "upstream emitted no compact-flags token (rsh not invoked)",
                );
            }
            Err(e) => return CheckOutcome::skip(self.name(), "server-args", e),
        };
        let oc = match capture_compact_flags(ctx.oc, root, src, "oc") {
            Ok(Some(t)) => t,
            Ok(None) => {
                return CheckOutcome::fail(
                    self.name(),
                    "server-args",
                    "oc emitted no compact-flags token; expected the capability string",
                );
            }
            Err(e) => return CheckOutcome::skip(self.name(), "server-args", e),
        };

        // Non-vacuous: the token must actually carry the capability suffix.
        if !up.contains(CAPABILITY_MARKER) {
            return CheckOutcome::skip(
                self.name(),
                "server-args",
                format!("upstream token `{up}` lacks the `{CAPABILITY_MARKER}` capability suffix"),
            );
        }
        if oc != up {
            if ctx.verbose {
                eprintln!("[capability-string] oc=`{oc}` upstream=`{up}`");
            }
            return CheckOutcome::fail(
                self.name(),
                "server-args",
                format!("capability string differs: oc=`{oc}` upstream=`{up}`"),
            );
        }
        CheckOutcome::pass(self.name(), "server-args")
    }
}

/// Run `client` as an rsh-pulling client whose remote shell is a capture script,
/// returning the single compact-flags token it handed the shell (`None` if the
/// shell was never invoked, i.e. no capture was written).
fn capture_compact_flags(
    client: &Path,
    root: &Path,
    src: &Path,
    tag: &str,
) -> Result<Option<String>, String> {
    let capture = root.join(format!("argv-{tag}.txt"));
    let _ = std::fs::remove_file(&capture);
    let rsh = write_capture_rsh(root, &capture, tag)?;

    let mut cmd = Command::new(client);
    cmd.args(CLIENT_FLAGS)
        .arg("-e")
        .arg(&rsh)
        // Push: local source, remote destination. The client is the sender and
        // advertises the full capability suffix. A fake host forces the
        // remote-shell path; the capture script ignores it.
        .arg(format!("{}/", src.display()))
        .arg(format!(
            "oc-validate-host:{}/",
            root.join(format!("dst-{tag}")).display()
        ));
    // The capture shell exits immediately, so the client reports a closed
    // connection; its exit status is irrelevant - only the recorded argv matters.
    cmd.output().map_err(|e| format!("spawn {client:?}: {e}"))?;

    if !capture.exists() {
        return Ok(None);
    }
    let recorded = std::fs::read_to_string(&capture).map_err(|e| e.to_string())?;
    Ok(compact_flags_token(&recorded))
}

/// Write an executable rsh that records each argv entry (one per line) into
/// `capture` and exits, then does nothing else.
fn write_capture_rsh(root: &Path, capture: &Path, tag: &str) -> Result<std::path::PathBuf, String> {
    let script = root.join(format!("capture-rsh-{tag}.sh"));
    let body = format!(
        "#!/bin/sh\n{{ for a in \"$@\"; do printf '%s\\n' \"$a\"; done; }} > '{}'\nexit 0\n",
        capture.display()
    );
    std::fs::write(&script, body).map_err(|e| e.to_string())?;
    let mut perms = std::fs::metadata(&script)
        .map_err(|e| e.to_string())?
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).map_err(|e| e.to_string())?;
    Ok(script)
}

/// Extract the compact-flags token from captured rsh argv: the single-dash token
/// that carries the `.`-separated capability suffix (e.g. `-logDtpre.iLsfxCIvu`).
/// Long flags (`--server`), the `.` source marker, and paths are skipped.
fn compact_flags_token(recorded: &str) -> Option<String> {
    recorded
        .lines()
        .map(str::trim)
        .find(|line| {
            line.starts_with('-')
                && !line.starts_with("--")
                && line.contains('.')
                && line[1..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic())
        })
        .map(str::to_owned)
}

/// Build a one-file source fixture. Idempotent: removes any prior tree first.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(src).map_err(|e| e.to_string())?;
    std::fs::write(src.join("f.txt"), b"capability-fixture\n").map_err(|e| e.to_string())?;
    support::capture(
        "touch",
        &["-d", "@1614830767", &src.join("f.txt").to_string_lossy()],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{CAPABILITY_MARKER, compact_flags_token};

    #[test]
    fn extracts_compact_flags_token_with_capability_suffix() {
        // Push server invocation: the receiver `rsync --server . <dst>` (no
        // `--sender`) carries the sender's compact-flags token.
        let recorded = "oc-validate-host\nrsync\n--server\n-logDtpre.iLsfxCIvu\n.\n/tmp/dst/\n";
        let token = compact_flags_token(recorded).unwrap();
        assert_eq!(token, "-logDtpre.iLsfxCIvu");
        assert!(token.contains(CAPABILITY_MARKER));
    }

    #[test]
    fn ignores_long_flags_dot_marker_and_paths() {
        // No single-dash dotted token present: the `.` source marker and the
        // `--server` long flag must not be mistaken for the capability string.
        let recorded = "host\nrsync\n--server\n--sender\n.\n/tmp/src/\n";
        assert!(compact_flags_token(recorded).is_none());
    }

    #[test]
    fn ignores_block_size_token_without_capability_dot() {
        // `-B131072` is single-dash but carries no `.` suffix, so it is not the
        // capability token.
        let recorded = "host\nrsync\n--server\n-B131072\n-logDtpre.iLsfxCIvu\n";
        assert_eq!(
            compact_flags_token(recorded).unwrap(),
            "-logDtpre.iLsfxCIvu"
        );
    }
}
