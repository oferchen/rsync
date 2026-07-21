//! `--mkpath` parity: oc-rsync creates the missing leading directory
//! components of the destination path exactly as upstream does.
//!
//! Ports upstream 3.5-dev tests `mkpath` and `file-to-file-mkpath-dry-run`.
//! `--mkpath` tells rsync to create the missing leading path elements of the
//! destination before transferring; without it, rsync errors when the dest
//! parent directories do not exist (`No such file or directory`). This check
//! aims each transfer at a DEEP destination (`<cell>/new/deep/path/`) whose
//! parent directories are deliberately never pre-created, then asserts:
//!
//! * with `--mkpath`, both oc and upstream exit 0, create the deep path, and
//!   land byte-identical `a.txt` + `sub/b.txt` trees there;
//! * without `--mkpath` (the control cell), both oc AND upstream FAIL, proving
//!   `--mkpath` is what enables the creation rather than some ambient default.
//!
//! The transfer command is built directly per transport (local / ssh / russh /
//! daemon) so the destination operand is the non-existent deep path. oc runs
//! over `transport`, upstream over `transport.for_upstream()`, each into its own
//! fresh cell dir. The ssh transports are skipped when no sshd answers on
//! localhost:22.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::commands::validate::support;
use crate::commands::validate::transport::{DaemonHandle, Transport};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The `--mkpath` destination-parent-creation parity check.
pub struct Mkpath;

/// Numeric-ids `--mkpath` over a full metadata-preserving transfer. The control
/// cell reuses these flags with `--mkpath` filtered out (see [`flags_for`]).
const FLAGS: &[&str] = &["-rlptgoD", "--mkpath", "--numeric-ids"];

impl Check for Mkpath {
    fn name(&self) -> &'static str {
        "mkpath"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("mkpath");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        // Two scenarios per transport: with `--mkpath` (the deep path must be
        // created) and the without-`--mkpath` control (both clients must fail).
        ctx.transports
            .iter()
            .flat_map(|&t| {
                [
                    self.cell_with(ctx, t, &root, &src),
                    self.cell_without(ctx, t, &root, &src),
                ]
            })
            .collect()
    }
}

impl Mkpath {
    /// The `--mkpath` cell: both clients must create the deep destination path
    /// and produce identical trees there.
    fn cell_with(
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

        let (oc_deep, up_deep) = match self.prepare(root, label) {
            Ok((oc_base, up_base)) => (deep_dst(&oc_base), deep_dst(&up_base)),
            Err(e) => return CheckOutcome::skip(self.name(), label, e),
        };

        // Non-vacuous guard: the deep path must be absent before the transfer,
        // otherwise `--mkpath` would have nothing to create and the cell would
        // pass trivially.
        if oc_deep.exists() || up_deep.exists() {
            return CheckOutcome::fail(self.name(), label, "deep path existed before transfer");
        }

        let daemon = match self.daemon_for(ctx, transport, src) {
            Ok(handle) => handle,
            Err(e) => return CheckOutcome::skip(self.name(), label, e),
        };
        let daemon_url = daemon.as_ref().map(|d| d.module_url());

        let up = run_client(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            src,
            &up_deep,
            true,
            daemon_url.as_deref(),
        );
        let up = match up {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), label, "upstream", other),
        };
        let _ = up;
        let oc = run_client(
            transport,
            ctx.oc,
            ctx.upstream,
            src,
            &oc_deep,
            true,
            daemon_url.as_deref(),
        );
        let oc = match oc {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), label, "oc", other),
        };
        let _ = oc;
        drop(daemon);

        // `--mkpath` must have created the deep path for both clients.
        if !oc_deep.exists() {
            return CheckOutcome::fail(self.name(), label, "oc did not create the deep path");
        }
        if !up_deep.exists() {
            return CheckOutcome::fail(self.name(), label, "upstream did not create the deep path");
        }
        // The transferred fixture must be present under the created path.
        if !has_fixture(&oc_deep) || !has_fixture(&up_deep) {
            return CheckOutcome::fail(self.name(), label, "deep path missing a.txt or sub/b.txt");
        }
        // And the two created trees must be byte-identical.
        match support::content_diff(&oc_deep, &up_deep) {
            Some(diff) => CheckOutcome::fail(
                self.name(),
                label,
                format!("oc and upstream deep trees differ: {diff}"),
            ),
            None => CheckOutcome::pass(self.name(), label),
        }
    }

    /// The control cell: without `--mkpath`, both clients must FAIL because the
    /// destination parent directories do not exist.
    fn cell_without(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        src: &Path,
    ) -> CheckOutcome {
        let label = format!("{} no-mkpath", transport.label());
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label.as_str(), "no sshd on localhost:22");
        }

        let bases = match self.prepare(root, &format!("{}-nomkpath", transport.label())) {
            Ok(bases) => bases,
            Err(e) => return CheckOutcome::skip(self.name(), label.as_str(), e),
        };
        let (oc_deep, up_deep) = (deep_dst(&bases.0), deep_dst(&bases.1));

        let daemon = match self.daemon_for(ctx, transport, src) {
            Ok(handle) => handle,
            Err(e) => return CheckOutcome::skip(self.name(), label.as_str(), e),
        };
        let daemon_url = daemon.as_ref().map(|d| d.module_url());

        let up = run_client(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            src,
            &up_deep,
            false,
            daemon_url.as_deref(),
        );
        let oc = run_client(
            transport,
            ctx.oc,
            ctx.upstream,
            src,
            &oc_deep,
            false,
            daemon_url.as_deref(),
        );
        drop(daemon);

        let (up, oc) = match (up, oc) {
            (Ok(up), Ok(oc)) => (up, oc),
            (Err(e), _) => {
                return CheckOutcome::skip(
                    self.name(),
                    label.as_str(),
                    format!("upstream could not run: {e}"),
                );
            }
            (_, Err(e)) => {
                return CheckOutcome::skip(
                    self.name(),
                    label.as_str(),
                    format!("oc could not run: {e}"),
                );
            }
        };
        if up.status.success() {
            return CheckOutcome::fail(
                self.name(),
                label.as_str(),
                "upstream succeeded without --mkpath (expected failure)",
            );
        }
        if oc.status.success() {
            return CheckOutcome::fail(
                self.name(),
                label.as_str(),
                "oc succeeded without --mkpath (expected failure)",
            );
        }
        CheckOutcome::pass(self.name(), label.as_str())
    }

    /// Recreate the two per-cell base directories (empty, WITHOUT the deep
    /// suffix) and return their paths, keyed by `tag`.
    fn prepare(&self, root: &Path, tag: &str) -> Result<(PathBuf, PathBuf), String> {
        let oc_base = root.join(format!("oc-{tag}"));
        let up_base = root.join(format!("up-{tag}"));
        reset_dir(&oc_base)?;
        reset_dir(&up_base)?;
        Ok((oc_base, up_base))
    }

    /// Start one shared upstream daemon for a daemon cell, or `None` for the
    /// filesystem/ssh transports.
    fn daemon_for(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        src: &Path,
    ) -> Result<Option<DaemonHandle>, String> {
        if transport != Transport::Daemon {
            return Ok(None);
        }
        DaemonHandle::start(ctx.upstream, src, ctx.work)
            .map(Some)
            .map_err(|e| format!("daemon: {e}"))
    }
}

/// Build one client rsync `Command` for `transport` aimed at the non-existent
/// deep destination `deep_dst`, and run it. `mkpath` selects the flag set (the
/// control cell drops `--mkpath`). Operand forms mirror `delete::run_client`:
/// `local` is a filesystem copy, `ssh-subprocess` uses `localhost:<src>`,
/// `russh` an `ssh://` URL, and `daemon` the module URL in `daemon_url`. The
/// sender is always `upstream` for the network transports.
fn run_client(
    transport: Transport,
    client: &Path,
    upstream: &Path,
    src: &Path,
    deep_dst: &Path,
    mkpath: bool,
    daemon_url: Option<&str>,
) -> TaskResult<Output> {
    let dst_arg = format!("{}/", deep_dst.display());
    let mut cmd = Command::new(client);
    cmd.args(flags_for(mkpath));

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

/// The deep destination path under a cell base: `<base>/new/deep/path`, whose
/// leading `new/deep/path` components are never pre-created.
fn deep_dst(base: &Path) -> PathBuf {
    base.join("new").join("deep").join("path")
}

/// The flag set for a scenario: the full [`FLAGS`] with `--mkpath` when enabled,
/// or the same set minus `--mkpath` for the control cell.
fn flags_for(mkpath: bool) -> Vec<&'static str> {
    FLAGS
        .iter()
        .copied()
        .filter(|f| mkpath || *f != "--mkpath")
        .collect()
}

/// True when the transferred fixture (`a.txt` and `sub/b.txt`) is present under
/// the created deep path.
fn has_fixture(deep: &Path) -> bool {
    deep.join("a.txt").is_file() && deep.join("sub").join("b.txt").is_file()
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
                label,
                format!("{who} exited {code}: {}", stderr.trim()),
            )
        }
        Err(e) => CheckOutcome::skip(check, label, format!("{who} could not run: {e}")),
    }
}

/// Recreate `dir` as an empty directory.
fn reset_dir(dir: &Path) -> Result<(), String> {
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|e| format!("remove {}: {e}", dir.display()))?;
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))
}

/// Build the mkpath fixture: a top-level `a.txt` and a `sub/b.txt`. Idempotent:
/// removes any prior tree first. No mtime backdating is needed because every
/// destination is created fresh, so the quick-check never skips a file.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;
    std::fs::write(src.join("a.txt"), b"alpha\n").map_err(|e| e.to_string())?;
    std::fs::write(sub.join("b.txt"), b"bravo\n").map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{deep_dst, flags_for};
    use std::path::Path;

    #[test]
    fn deep_dst_appends_never_precreated_suffix() {
        // The suffix encodes the whole point of the check: three missing leading
        // components that only `--mkpath` can create.
        assert_eq!(
            deep_dst(Path::new("/base")),
            Path::new("/base/new/deep/path")
        );
    }

    #[test]
    fn control_cell_drops_only_mkpath() {
        // WHY: the without-`--mkpath` control proves `--mkpath` is what enables
        // creation, so it must differ from the real cell in exactly that flag.
        let with = flags_for(true);
        let without = flags_for(false);
        assert!(with.contains(&"--mkpath"));
        assert!(!without.contains(&"--mkpath"));
        for shared in ["-rlptgoD", "--numeric-ids"] {
            assert!(with.contains(&shared) && without.contains(&shared));
        }
    }
}
