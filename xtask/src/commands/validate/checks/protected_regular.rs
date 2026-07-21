//! Receiver symlink-overwrite safety parity between oc-rsync and upstream.
//!
//! A destination that already holds a symlink pointing *outside* the tree must
//! not be followed when the receiver lands a regular file of the same name: the
//! outside target has to stay untouched and the destination entry must end up a
//! plain regular file. Following the link would let a pre-planted symlink redirect
//! a write anywhere the receiver can reach - the receiver-side symlink-escape
//! attack. Upstream defeats it by writing a temp file in the destination
//! directory and atomically renaming over the symlink, never opening the link's
//! target.
//!
//! This check seeds each fresh destination with `payload -> <outside file>`,
//! transfers a regular `payload` from the source, and asserts for both clients
//! that (1) the destination `payload` is a regular file carrying the source bytes
//! and (2) the outside target still holds its original bytes. A crash, a hang, or
//! a modified outside target is a failure.
//!
//! upstream: receiver.c `recv_files()` opens `get_tmpname()` in the destination
//! dir and finishes via `finish_transfer()` -> `robust_rename()`, so the symlink
//! at the final name is replaced, never dereferenced.

use std::path::Path;
use std::process::{Command, Output};

use crate::commands::validate::support;
use crate::commands::validate::transport::{DaemonHandle, Transport};
use crate::commands::validate::{Category, Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The receiver symlink-overwrite safety parity check.
pub struct ProtectedRegular;

/// Source bytes for the regular `payload` that lands at the destination.
const SRC_BODY: &[u8] = b"source-payload-bytes\n";
/// Bytes the outside target must still hold after the transfer (proof it was
/// never written through).
const OUTSIDE_BODY: &[u8] = b"outside-must-not-change\n";
/// Backdated mtime so the quick-check never skips the transfer.
const EPOCH: &str = "@1614830767";

impl Check for ProtectedRegular {
    fn name(&self) -> &'static str {
        "protected-regular"
    }

    fn categories(&self) -> &'static [Category] {
        &[Category::Security]
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("protected-regular");
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

impl ProtectedRegular {
    /// Run one transport cell: seed the destination with a pre-existing symlink
    /// pointing outside the tree, transfer the regular file, and assert neither
    /// client followed the link.
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

        let daemon = if transport == Transport::Daemon {
            match DaemonHandle::start(ctx.upstream, src, ctx.work) {
                Ok(handle) => Some(handle),
                Err(e) => return CheckOutcome::skip(self.name(), label, format!("daemon: {e}")),
            }
        } else {
            None
        };
        let daemon_url = daemon.as_ref().map(|d| d.module_url());

        for who in ["up", "oc"] {
            let client = if who == "up" { ctx.upstream } else { ctx.oc };
            let effective = if who == "up" {
                transport.for_upstream()
            } else {
                transport
            };
            let dst = root.join(format!("{who}-{label}"));
            let outside = root.join(format!("{who}-{label}-outside"));

            if let Err(e) = seed_target(&dst, &outside) {
                return CheckOutcome::skip(self.name(), label, format!("{who} seed: {e}"));
            }
            match run_client(
                effective,
                client,
                ctx.upstream,
                src,
                &dst,
                daemon_url.as_deref(),
            ) {
                Ok(out) if out.status.success() => {}
                other => return skip_or_fail(self.name(), label, who, other),
            }
            if let Some(msg) = follow_violation(&dst, &outside) {
                if ctx.verbose {
                    eprintln!("[protected-regular/{label}] {who}: {msg}");
                }
                return CheckOutcome::fail(self.name(), label, format!("{who}: {msg}"));
            }
        }
        drop(daemon);
        CheckOutcome::pass(self.name(), label)
    }
}

/// Non-vacuous guard: the destination `payload` must be a regular file carrying
/// the source bytes, and the outside target must be unchanged.
fn follow_violation(dst: &Path, outside: &Path) -> Option<String> {
    match dst.join("payload").symlink_metadata() {
        Ok(m) if m.file_type().is_file() => match std::fs::read(dst.join("payload")) {
            Ok(bytes) if bytes == SRC_BODY => {}
            Ok(_) => return Some("dest payload content != source".to_string()),
            Err(e) => return Some(format!("cannot read dest payload: {e}")),
        },
        Ok(m) if m.file_type().is_symlink() => {
            return Some("dest payload is still a symlink (not replaced)".to_string());
        }
        Ok(_) => return Some("dest payload is not a regular file".to_string()),
        Err(e) => return Some(format!("cannot stat dest payload: {e}")),
    }
    match std::fs::read(outside) {
        Ok(bytes) if bytes == OUTSIDE_BODY => None,
        Ok(_) => Some("outside target was overwritten through the symlink".to_string()),
        Err(e) => Some(format!("cannot read outside target: {e}")),
    }
}

/// Build one client rsync `Command` for `transport` and pull `src/` into `dst/`
/// (already seeded). Flags are `-rlptgoD --numeric-ids`; operand forms mirror
/// `transport::pull_into`. The destination is NOT reset here - the caller seeded
/// the adversarial symlink and it must persist into the transfer.
fn run_client(
    transport: Transport,
    client: &Path,
    upstream: &Path,
    src: &Path,
    dst: &Path,
    daemon_url: Option<&str>,
) -> TaskResult<Output> {
    let dst_arg = format!("{}/", dst.display());
    let mut cmd = Command::new(client);
    cmd.arg("-rlptgoD").arg("--numeric-ids");

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

/// Distinguish a genuine divergence from an unrunnable cell (e.g. ssh refused).
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
                label.to_string(),
                format!("{who} exited {code}: {}", stderr.trim()),
            )
        }
        Err(e) => CheckOutcome::skip(
            check,
            label.to_string(),
            format!("{who} could not run: {e}"),
        ),
    }
}

/// Recreate `dst` empty, write the outside target, and plant `dst/payload` as an
/// absolute symlink pointing at that outside target.
fn seed_target(dst: &Path, outside: &Path) -> Result<(), String> {
    if dst.exists() {
        std::fs::remove_dir_all(dst).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    std::fs::write(outside, OUTSIDE_BODY).map_err(|e| e.to_string())?;
    let outside_abs = outside.canonicalize().map_err(|e| e.to_string())?;
    std::os::unix::fs::symlink(&outside_abs, dst.join("payload")).map_err(|e| e.to_string())?;
    Ok(())
}

/// Build the source fixture: a single regular `payload`. Idempotent.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(src).map_err(|e| e.to_string())?;
    std::fs::write(src.join("payload"), SRC_BODY).map_err(|e| e.to_string())?;
    support::capture(
        "touch",
        &["-d", EPOCH, &src.join("payload").to_string_lossy()],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{OUTSIDE_BODY, SRC_BODY, follow_violation};
    use std::fs;
    use std::os::unix::fs::symlink;

    #[test]
    fn violation_none_when_link_replaced_and_outside_untouched() {
        let dst = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let outside = out.path().join("target");
        fs::write(&outside, OUTSIDE_BODY).unwrap();
        // Correct receiver outcome: payload is a regular file with source bytes.
        fs::write(dst.path().join("payload"), SRC_BODY).unwrap();
        assert!(follow_violation(dst.path(), &outside).is_none());
    }

    #[test]
    fn violation_flags_an_unfollowed_symlink_still_present() {
        let dst = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let outside = out.path().join("target");
        fs::write(&outside, OUTSIDE_BODY).unwrap();
        symlink(&outside, dst.path().join("payload")).unwrap();
        assert!(
            follow_violation(dst.path(), &outside)
                .unwrap()
                .contains("still a symlink")
        );
    }

    #[test]
    fn violation_flags_an_overwritten_outside_target() {
        let dst = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let outside = out.path().join("target");
        // The link was followed: outside now holds the source bytes.
        fs::write(&outside, SRC_BODY).unwrap();
        fs::write(dst.path().join("payload"), SRC_BODY).unwrap();
        assert!(
            follow_violation(dst.path(), &outside)
                .unwrap()
                .contains("overwritten")
        );
    }
}
