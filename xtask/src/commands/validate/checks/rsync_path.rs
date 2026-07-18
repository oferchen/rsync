//! `--rsync-path` parity between oc-rsync and upstream over ssh.
//!
//! The `--rsync-path` option names the rsync binary to launch on the remote
//! end, so it is only meaningful for the ssh transports (a local copy and the
//! daemon transport never spawn a remote rsync). This check exercises the
//! ssh-subprocess and russh transports with two scenarios per transport:
//!
//! - `honored`: point `--rsync-path` at a wrapper script that records a
//!   per-run sentinel and then execs the real upstream rsync. Both clients must
//!   exit 0, both sentinels must exist afterward (proving the named binary
//!   really ran), and the two destination trees must be identical. This proves
//!   oc invokes the `--rsync-path` binary exactly as upstream does.
//! - `missing`: point `--rsync-path` at a path that does not exist. Both clients
//!   must fail (non-zero exit) - oc must not silently succeed when the remote
//!   binary is absent. Exit codes and messages need not match, only that both
//!   are failures.
//!
//! Because upstream rsync has no embedded russh client, the reference (upstream)
//! run of the russh cell uses the ssh subprocess instead
//! ([`Transport::for_upstream`]); the client under test still exercises the
//! `ssh://` code path. Commands are built directly here rather than through
//! `pull_into`, which hard-codes its own `--rsync-path`.

use std::path::Path;
use std::process::{Command, Output};

use crate::commands::validate::support;
use crate::commands::validate::transport::Transport;
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The `--rsync-path` fidelity check.
pub struct RsyncPath;

/// Base flags shared by both scenarios (recursive, symlinks, perms, times,
/// group, owner, devices; numeric ids to avoid name-mapping differences).
const BASE_FLAGS: [&str; 2] = ["-rlptgoD", "--numeric-ids"];

/// A remote rsync path guaranteed not to exist, for the negative scenario.
const MISSING_RSYNC_PATH: &str = "/nonexistent/oc-rsync-does-not-exist";

impl Check for RsyncPath {
    fn name(&self) -> &'static str {
        "rsync-path"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let ssh_transports: Vec<Transport> = ctx
            .transports
            .iter()
            .copied()
            .filter(|t| t.needs_ssh())
            .collect();
        if ssh_transports.is_empty() {
            return vec![CheckOutcome::skip(
                self.name(),
                "transports",
                "no ssh transport selected",
            )];
        }

        let root = ctx.work.join("rsync-path");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let wrapper = root.join("wrapper.sh");

        let mut outcomes = Vec::new();
        for transport in ssh_transports {
            outcomes.push(self.honored(ctx, &root, &src, &wrapper, transport));
            outcomes.push(self.missing(ctx, &root, &src, transport));
        }
        outcomes
    }
}

impl RsyncPath {
    /// Positive scenario: `--rsync-path` names the sentinel wrapper. Both clients
    /// must exit 0, both sentinels must exist, and the trees must match.
    fn honored(
        &self,
        ctx: &ValidateCtx,
        root: &Path,
        src: &Path,
        wrapper: &Path,
        transport: Transport,
    ) -> CheckOutcome {
        let label = transport.label();
        let cell = format!("{label} honored");
        if !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), cell, "no sshd on localhost:22");
        }

        let flags = honored_flags(wrapper);
        let up_dst = root.join(format!("up-{label}"));
        let oc_dst = root.join(format!("oc-{label}"));
        let up_sentinel = root.join(format!("sentinel-{label}-up"));
        let oc_sentinel = root.join(format!("sentinel-{label}-oc"));

        if let Err(e) = arm_wrapper(wrapper, ctx.upstream, &up_sentinel) {
            return CheckOutcome::skip(self.name(), cell, e.to_string());
        }
        let up = match run_plain(ctx.upstream, transport.for_upstream(), &flags, src, &up_dst) {
            Ok(out) => out,
            Err(e) => return CheckOutcome::skip(self.name(), cell, e.to_string()),
        };
        if let Err(e) = arm_wrapper(wrapper, ctx.upstream, &oc_sentinel) {
            return CheckOutcome::skip(self.name(), cell, e.to_string());
        }
        let oc = match run_plain(ctx.oc, transport, &flags, src, &oc_dst) {
            Ok(out) => out,
            Err(e) => return CheckOutcome::skip(self.name(), cell, e.to_string()),
        };

        let (up_hit, oc_hit) = (up_sentinel.exists(), oc_sentinel.exists());
        if ctx.verbose {
            eprintln!(
                "[rsync-path/{cell}] oc exit={:?} sentinel={oc_hit}; upstream exit={:?} sentinel={up_hit}",
                oc.status.code(),
                up.status.code()
            );
        }

        if !oc.status.success() || !up.status.success() {
            return CheckOutcome::fail(
                self.name(),
                cell,
                format!(
                    "expected both to honor --rsync-path but oc exit={:?} upstream exit={:?}",
                    oc.status.code(),
                    up.status.code()
                ),
            );
        }
        if !oc_hit || !up_hit {
            return CheckOutcome::fail(
                self.name(),
                cell,
                format!(
                    "custom rsync-path binary did not run (oc sentinel={oc_hit}, upstream sentinel={up_hit})"
                ),
            );
        }
        match support::content_diff(&oc_dst, &up_dst) {
            Some(diff) => CheckOutcome::fail(self.name(), cell, diff),
            None => CheckOutcome::pass(self.name(), cell),
        }
    }

    /// Negative scenario: `--rsync-path` names a nonexistent binary. Both clients
    /// must fail; oc must not silently succeed.
    fn missing(
        &self,
        ctx: &ValidateCtx,
        root: &Path,
        src: &Path,
        transport: Transport,
    ) -> CheckOutcome {
        let label = transport.label();
        let cell = format!("{label} missing");
        if !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), cell, "no sshd on localhost:22");
        }

        let flags = missing_flags();
        let up_dst = root.join(format!("up-{label}-missing"));
        let oc_dst = root.join(format!("oc-{label}-missing"));

        let up = match run_plain(ctx.upstream, transport.for_upstream(), &flags, src, &up_dst) {
            Ok(out) => out,
            Err(e) => return CheckOutcome::skip(self.name(), cell, e.to_string()),
        };
        let oc = match run_plain(ctx.oc, transport, &flags, src, &oc_dst) {
            Ok(out) => out,
            Err(e) => return CheckOutcome::skip(self.name(), cell, e.to_string()),
        };

        if ctx.verbose {
            eprintln!(
                "[rsync-path/{cell}] oc exit={:?} upstream exit={:?}",
                oc.status.code(),
                up.status.code()
            );
        }

        if oc.status.success() || up.status.success() {
            return CheckOutcome::fail(
                self.name(),
                cell,
                format!(
                    "expected both to fail on missing --rsync-path but oc exit={:?} upstream exit={:?}",
                    oc.status.code(),
                    up.status.code()
                ),
            );
        }
        CheckOutcome::pass(self.name(), cell)
    }
}

/// Flags for the positive scenario: base flags plus `--rsync-path=<wrapper>`.
fn honored_flags(wrapper: &Path) -> Vec<String> {
    let mut flags: Vec<String> = BASE_FLAGS.iter().map(|s| s.to_string()).collect();
    flags.push(format!("--rsync-path={}", wrapper.display()));
    flags
}

/// Flags for the negative scenario: base flags plus a nonexistent `--rsync-path`.
fn missing_flags() -> Vec<String> {
    let mut flags: Vec<String> = BASE_FLAGS.iter().map(|s| s.to_string()).collect();
    flags.push(format!("--rsync-path={MISSING_RSYNC_PATH}"));
    flags
}

/// Rewrite the wrapper for this run's sentinel and clear that sentinel, so its
/// post-run existence proves the wrapper executed.
fn arm_wrapper(wrapper: &Path, upstream: &Path, sentinel: &Path) -> TaskResult<()> {
    write_wrapper(wrapper, upstream, sentinel)?;
    let _ = std::fs::remove_file(sentinel);
    Ok(())
}

/// Reset the destination and pull `src` over an ssh transport.
fn run_plain(
    client: &Path,
    transport: Transport,
    flags: &[String],
    src: &Path,
    dst: &Path,
) -> TaskResult<Output> {
    reset_dir(dst)?;
    run(ssh_command(client, transport, flags, src, dst))
}

/// Build a pull command for `client` over an ssh transport.
///
/// `flags` already carries `--rsync-path`; only the ssh operands are added here.
/// The ssh-subprocess form uses `-e ssh ... localhost:<src>/`; the russh form
/// uses an `ssh://localhost<src>/` URL with no `-e`.
fn ssh_command(
    client: &Path,
    transport: Transport,
    flags: &[String],
    src: &Path,
    dst: &Path,
) -> Command {
    let mut cmd = Command::new(client);
    cmd.args(flags);
    let dst_arg = format!("{}/", dst.display());
    match transport {
        Transport::Russh => {
            cmd.arg(format!("ssh://localhost{}/", src.display()))
                .arg(dst_arg);
        }
        _ => {
            cmd.arg("-e")
                .arg("ssh -o BatchMode=yes -o StrictHostKeyChecking=no")
                .arg(format!("localhost:{}/", src.display()))
                .arg(dst_arg);
        }
    }
    cmd
}

/// Run a prepared command, capturing combined output.
fn run(mut cmd: Command) -> TaskResult<Output> {
    cmd.output()
        .map_err(|e| TaskError::Validation(format!("failed to spawn {cmd:?}: {e}")))
}

/// The wrapper script body: record `sentinel`, then exec the real `upstream`.
fn wrapper_script(upstream: &Path, sentinel: &Path) -> String {
    format!(
        "#!/bin/sh\ntouch \"{}\"\nexec \"{}\" \"$@\"\n",
        sentinel.display(),
        upstream.display()
    )
}

/// Write the wrapper script and mark it executable (0o755).
fn write_wrapper(wrapper: &Path, upstream: &Path, sentinel: &Path) -> TaskResult<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(wrapper, wrapper_script(upstream, sentinel))
        .map_err(|e| TaskError::Validation(format!("write wrapper {}: {e}", wrapper.display())))?;
    std::fs::set_permissions(wrapper, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| TaskError::Validation(format!("chmod wrapper {}: {e}", wrapper.display())))
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

/// Build the fixture: two top-level files and a subdirectory with one file.
/// Idempotent: removes any prior tree first.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;

    std::fs::write(src.join("alpha.txt"), b"alpha\n").map_err(|e| e.to_string())?;
    std::fs::write(src.join("bravo.txt"), b"bravo\n").map_err(|e| e.to_string())?;
    std::fs::write(sub.join("charlie.txt"), b"charlie\n").map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{honored_flags, missing_flags, wrapper_script};
    use std::path::Path;

    #[test]
    fn wrapper_script_records_sentinel_then_execs_upstream() {
        let script = wrapper_script(Path::new("/usr/bin/rsync"), Path::new("/tmp/sentinel-x"));
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains("touch \"/tmp/sentinel-x\""));
        assert!(script.contains("exec \"/usr/bin/rsync\" \"$@\""));
    }

    #[test]
    fn honored_flags_point_rsync_path_at_the_wrapper() {
        let flags = honored_flags(Path::new("/work/wrapper.sh"));
        assert!(flags.contains(&"-rlptgoD".to_string()));
        assert!(flags.contains(&"--numeric-ids".to_string()));
        assert!(flags.contains(&"--rsync-path=/work/wrapper.sh".to_string()));
    }

    #[test]
    fn missing_flags_point_rsync_path_at_a_nonexistent_binary() {
        let flags = missing_flags();
        assert!(flags.contains(&"-rlptgoD".to_string()));
        assert!(
            flags
                .iter()
                .any(|f| f.starts_with("--rsync-path=/nonexistent/"))
        );
    }
}
