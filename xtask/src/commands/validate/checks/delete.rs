//! `--delete` parity: oc-rsync removes exactly the extraneous destination
//! entries upstream removes.
//!
//! Unlike the other checks, `--delete` only does anything when the destination
//! already holds entries absent from the source, so this check cannot use
//! `transport::pull_into` (which recreates the destination empty). Instead it
//! pre-seeds each destination with a copy of the source *plus* extraneous
//! entries (`stale.txt`, `stale_dir/inner.txt`), builds the client `Command`
//! directly, and transfers into the seeded tree without wiping it. The oc and
//! upstream destinations are seeded identically, so only the client under test
//! varies. Upstream rsync is the ground truth for both the surviving tree and
//! the set of `*deleting` itemize lines. The ssh transports are skipped when no
//! sshd answers on localhost:22.

use std::path::Path;
use std::process::{Command, Output};

use crate::commands::validate::support;
use crate::commands::validate::transport::{DaemonHandle, Transport};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The `--delete` extraneous-removal parity check.
pub struct Delete;

/// Itemized, numeric-ids, `--delete`: match upstream's deletion semantics and
/// surface each removal as a `*deleting` itemize line.
const FLAGS: &[&str] = &["-rlptgoD", "--delete", "-i", "--numeric-ids"];

impl Check for Delete {
    fn name(&self) -> &'static str {
        "delete"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("delete");
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

impl Delete {
    /// Run one transport cell: seed both destinations, transfer each, and
    /// compare surviving trees and `*deleting` lines against upstream.
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

        let oc_dst = root.join(format!("oc-{label}"));
        let up_dst = root.join(format!("up-{label}"));

        // Seed both destinations identically before their respective transfers.
        if let Err(e) = seed_dest(src, &oc_dst) {
            return CheckOutcome::skip(self.name(), label, format!("seed oc dest: {e}"));
        }
        if let Err(e) = seed_dest(src, &up_dst) {
            return CheckOutcome::skip(self.name(), label, format!("seed upstream dest: {e}"));
        }

        // Non-vacuous guard: the stale entries must actually exist pre-transfer,
        // otherwise `--delete` would have nothing to remove and the cell would
        // pass trivially.
        if !stale_seeded(&oc_dst) || !stale_seeded(&up_dst) {
            return CheckOutcome::fail(self.name(), label, "seed did not place stale entries");
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

        let up = match run_client(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            src,
            &up_dst,
            daemon_url.as_deref(),
        ) {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), label, "upstream", other),
        };
        let oc = match run_client(
            transport,
            ctx.oc,
            ctx.upstream,
            src,
            &oc_dst,
            daemon_url.as_deref(),
        ) {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), label, "oc", other),
        };
        drop(daemon);

        let oc_del = deleting_paths(&combined(&oc));
        let up_del = deleting_paths(&combined(&up));

        // Both destinations must equal the source (extraneous entries gone).
        if let Some(diff) = support::content_diff(&oc_dst, src) {
            if ctx.verbose {
                dump(label, &oc_dst, &up_dst, &oc_del, &up_del);
            }
            return CheckOutcome::fail(self.name(), label, format!("oc dest != source: {diff}"));
        }
        if let Some(diff) = support::content_diff(&up_dst, src) {
            if ctx.verbose {
                dump(label, &oc_dst, &up_dst, &oc_del, &up_del);
            }
            return CheckOutcome::fail(
                self.name(),
                label,
                format!("upstream dest != source: {diff}"),
            );
        }
        // And the two destinations must equal each other.
        if let Some(diff) = support::content_diff(&oc_dst, &up_dst) {
            if ctx.verbose {
                dump(label, &oc_dst, &up_dst, &oc_del, &up_del);
            }
            return CheckOutcome::fail(
                self.name(),
                label,
                format!("oc and upstream dests differ: {diff}"),
            );
        }

        // The set of removed paths must match upstream's.
        if oc_del != up_del {
            if ctx.verbose {
                dump(label, &oc_dst, &up_dst, &oc_del, &up_del);
            }
            return CheckOutcome::fail(
                self.name(),
                label,
                format!("deleting sets differ (oc {oc_del:?} vs upstream {up_del:?})"),
            );
        }

        CheckOutcome::pass(self.name(), label)
    }
}

/// Build one client rsync `Command` for `transport` and run it into the
/// (already-seeded) destination. The destination is never reset here, so
/// `--delete` operates on the pre-seeded tree.
///
/// Operand forms mirror `transport::pull_into`: `local` is a filesystem copy;
/// `ssh-subprocess` uses `-e ssh localhost:<src>`; `russh` uses an `ssh://` URL;
/// `daemon` uses the module URL passed in `daemon_url`. The sender is always
/// `upstream` for the network transports (`--rsync-path` / upstream daemon).
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
            let url = daemon_url
                .ok_or_else(|| TaskError::Validation("daemon transport without url".into()))?;
            cmd.arg(url).arg(&dst_arg);
        }
    }

    cmd.output()
        .map_err(|e| TaskError::Validation(format!("failed to spawn {cmd:?}: {e}")))
}

/// Seed `dst` with a fresh copy of the source tree plus the extraneous entries
/// `--delete` must remove. Recreates `dst` empty first, so it is idempotent.
fn seed_dest(src: &Path, dst: &Path) -> TaskResult<()> {
    reset_dir(dst)?;
    copy_tree(src, dst)?;
    std::fs::write(dst.join("stale.txt"), b"stale\n")
        .map_err(|e| TaskError::Validation(format!("write stale.txt: {e}")))?;
    let stale_dir = dst.join("stale_dir");
    std::fs::create_dir_all(&stale_dir)
        .map_err(|e| TaskError::Validation(format!("create stale_dir: {e}")))?;
    std::fs::write(stale_dir.join("inner.txt"), b"inner\n")
        .map_err(|e| TaskError::Validation(format!("write stale_dir/inner.txt: {e}")))
}

/// True when both seeded stale entries are present in `dst`.
fn stale_seeded(dst: &Path) -> bool {
    dst.join("stale.txt").exists() && dst.join("stale_dir").join("inner.txt").exists()
}

/// Recursively copy `src`'s contents (files and directories) into `dst`.
fn copy_tree(src: &Path, dst: &Path) -> TaskResult<()> {
    let entries = std::fs::read_dir(src)
        .map_err(|e| TaskError::Validation(format!("read {}: {e}", src.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| TaskError::Validation(format!("dir entry: {e}")))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ty = entry
            .file_type()
            .map_err(|e| TaskError::Validation(format!("file type: {e}")))?;
        if ty.is_dir() {
            std::fs::create_dir_all(&to)
                .map_err(|e| TaskError::Validation(format!("create {}: {e}", to.display())))?;
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| {
                TaskError::Validation(format!("copy {} -> {}: {e}", from.display(), to.display()))
            })?;
        }
    }
    Ok(())
}

/// Fold a run's stdout then stderr into one string; deletion notices can land on
/// either stream depending on the transport.
fn combined(out: &Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

/// Extract the sorted set of removed paths from a client's output.
///
/// A deletion notice is any line containing `deleting ` - the itemized form
/// `*deleting   <path>` and the verbose form `deleting <path>` both qualify. The
/// path is everything after the keyword, trimmed. Sorting makes the set order-
/// independent so oc and upstream compare regardless of deletion sequence.
pub fn deleting_paths(output: &str) -> Vec<String> {
    const KEY: &str = "deleting ";
    let mut paths: Vec<String> = output
        .lines()
        .filter_map(|line| {
            let idx = line.find(KEY)?;
            let path = line[idx + KEY.len()..].trim();
            (!path.is_empty()).then(|| path.to_string())
        })
        .collect();
    paths.sort();
    paths
}

/// Print deleting sets and surviving trees for both clients (verbose failures).
fn dump(label: &str, oc_dst: &Path, up_dst: &Path, oc_del: &[String], up_del: &[String]) {
    eprintln!("[delete/{label}] oc deleting: {oc_del:?}");
    eprintln!("[delete/{label}] upstream deleting: {up_del:?}");
    eprintln!(
        "[delete/{label}] oc surviving: {:?}",
        support::rel_entries(oc_dst)
    );
    eprintln!(
        "[delete/{label}] upstream surviving: {:?}",
        support::rel_entries(up_dst)
    );
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
fn reset_dir(dir: &Path) -> TaskResult<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)
            .map_err(|e| TaskError::Validation(format!("remove {}: {e}", dir.display())))?;
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| TaskError::Validation(format!("create {}: {e}", dir.display())))
}

/// Build the delete fixture: one top-level file and a subdirectory with one
/// file. Idempotent: removes any prior tree first. Mtimes are backdated so the
/// quick-check does not re-transfer the kept files under test.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;

    std::fs::write(src.join("keep1.txt"), b"keep1\n").map_err(|e| e.to_string())?;
    std::fs::write(sub.join("keep2.txt"), b"keep2\n").map_err(|e| e.to_string())?;

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
    use super::deleting_paths;

    #[test]
    fn extracts_itemized_deleting_paths_sorted() {
        let out = ">f+++++++++ keep1.txt\n\
                   *deleting   stale_dir/inner.txt\n\
                   *deleting   stale_dir/\n\
                   *deleting   stale.txt\n\
                   sent 100 bytes  received 20 bytes\n";
        assert_eq!(
            deleting_paths(out),
            vec!["stale.txt", "stale_dir/", "stale_dir/inner.txt"]
        );
    }

    #[test]
    fn extracts_verbose_form_and_ignores_non_deleting_lines() {
        let out = "deleting stale.txt\nbuilding file list\n";
        assert_eq!(deleting_paths(out), vec!["stale.txt"]);
    }

    #[test]
    fn empty_without_any_deletion_notice() {
        assert!(deleting_paths(">f+++++++++ keep1.txt\n").is_empty());
    }
}
