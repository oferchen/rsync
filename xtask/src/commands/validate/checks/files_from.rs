//! `--files-from` parity: oc-rsync transfers exactly the listed subset,
//! identically to upstream, in both the newline- and NUL-separated forms.
//!
//! With `--files-from=<file>` the source operand is the base directory (no
//! trailing path component) and each list entry names a path relative to it, so
//! only the listed subset is transferred. This check builds a five-entry source
//! (`a.txt`, `b.txt`, `c.txt`, `sub/d.txt`, `sub/e.txt`) and a list selecting
//! just `a.txt` and `sub/d.txt`, in two encodings:
//!
//! * `newline` - entries separated by `\n`, passed with `--files-from`.
//! * `from0`   - the same entries separated by NUL (`\0`), passed with
//!   `--from0 --files-from`.
//!
//! Each (transport, scenario) cell runs upstream then oc into fresh empty
//! destinations and asserts both exit 0, the two trees are identical
//! (`support::content_diff`), and - as a non-vacuous guard - the oc tree holds
//! exactly the listed subset and none of the unlisted entries. Like the other
//! direct-`Command` checks, the daemon cell shares one upstream daemon across
//! both client runs, and the ssh transports are skipped when no sshd answers on
//! localhost:22.

use std::path::Path;
use std::process::{Command, Output};

use crate::commands::validate::support;
use crate::commands::validate::transport::{DaemonHandle, Transport};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The `--files-from` subset-selection parity check.
pub struct FilesFrom;

/// The listed subset (relative to the source base), in sorted destination form.
const REQUIRED: [&str; 2] = ["a.txt", "sub/d.txt"];

/// Source entries that are present but *not* listed, so must never transfer.
const FORBIDDEN: [&str; 3] = ["b.txt", "c.txt", "sub/e.txt"];

impl Check for FilesFrom {
    fn name(&self) -> &'static str {
        "files-from"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("files-from");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }

        let list = root.join("list.txt");
        let list0 = root.join("list0.txt");
        if let Err(e) = write_lists(&list, &list0) {
            return vec![CheckOutcome::skip(self.name(), "list", e)];
        }

        // The `--files-from` operand carries the list-file path directly, so each
        // scenario differs only in its flag set (built here) and encoding.
        let scenarios: [(&str, Vec<String>); 2] = [
            (
                "newline",
                vec![
                    "-rlptgoD".into(),
                    "--numeric-ids".into(),
                    format!("--files-from={}", list.display()),
                ],
            ),
            (
                "from0",
                vec![
                    "-rlptgoD".into(),
                    "--numeric-ids".into(),
                    "--from0".into(),
                    format!("--files-from={}", list0.display()),
                ],
            ),
        ];

        let mut outcomes = Vec::new();
        for &transport in ctx.transports {
            for (scenario, flags) in &scenarios {
                outcomes.push(self.cell(ctx, transport, &root, &src, scenario, flags));
            }
        }
        outcomes
    }
}

impl FilesFrom {
    /// Run one (transport, scenario) cell: transfer the listed subset with both
    /// clients into fresh destinations and compare the results.
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        src: &Path,
        scenario: &str,
        flags: &[String],
    ) -> CheckOutcome {
        let label = format!("{} {scenario}", transport.label());
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), &label, "no sshd on localhost:22");
        }

        let oc_dst = root.join(format!("oc-{}-{scenario}", transport.label()));
        let up_dst = root.join(format!("up-{}-{scenario}", transport.label()));
        if let Err(e) = reset_dir(&oc_dst).and_then(|()| reset_dir(&up_dst)) {
            return CheckOutcome::skip(self.name(), &label, format!("reset dest: {e}"));
        }

        // One upstream daemon serves both client runs for the daemon transport.
        let daemon = if transport == Transport::Daemon {
            match DaemonHandle::start(ctx.upstream, src, ctx.work) {
                Ok(handle) => Some(handle),
                Err(e) => return CheckOutcome::skip(self.name(), &label, format!("daemon: {e}")),
            }
        } else {
            None
        };
        let daemon_url = daemon.as_ref().map(|d| d.module_url());

        let up = run_client(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            src,
            &up_dst,
            flags,
            daemon_url.as_deref(),
        );
        match up {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), &label, "upstream", other),
        }
        let oc = run_client(
            transport,
            ctx.oc,
            ctx.upstream,
            src,
            &oc_dst,
            flags,
            daemon_url.as_deref(),
        );
        match oc {
            Ok(out) if out.status.success() => {}
            other => return skip_or_fail(self.name(), &label, "oc", other),
        }
        drop(daemon);

        // Non-vacuous guard: the oc tree must be exactly the listed subset,
        // otherwise a no-op (empty) or full-tree transfer would pass trivially.
        let oc_entries: Vec<String> = support::rel_entries(&oc_dst)
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        if let Some(problem) = subset_problem(&oc_entries) {
            return CheckOutcome::fail(self.name(), &label, problem);
        }

        match support::content_diff(&oc_dst, &up_dst) {
            Some(diff) => CheckOutcome::fail(self.name(), &label, diff),
            None => CheckOutcome::pass(self.name(), &label),
        }
    }
}

/// Build one client rsync `Command` for `transport` and run it into `dst`.
///
/// `flags` already carries the `--files-from` operand (and `--from0` for the
/// NUL scenario); only the source base and network plumbing are added per
/// transport. The base has no trailing path component so list entries resolve
/// relative to it: `<src>/` for local, `localhost:<src>/` for ssh,
/// `ssh://localhost<src>/` for russh, and the module root for the daemon. The
/// sender is always `upstream` on the network transports.
fn run_client(
    transport: Transport,
    client: &Path,
    upstream: &Path,
    src: &Path,
    dst: &Path,
    flags: &[String],
    daemon_url: Option<&str>,
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

/// Report the first way `entries` fails to be exactly the listed subset: a
/// missing required entry or a present forbidden one. `None` means the tree
/// holds every listed path and none of the unlisted ones.
fn subset_problem(entries: &[String]) -> Option<String> {
    for want in REQUIRED {
        if !entries.iter().any(|e| e == want) {
            return Some(format!("listed entry {want} missing from oc dest"));
        }
    }
    for deny in FORBIDDEN {
        if entries.iter().any(|e| e == deny) {
            return Some(format!("unlisted entry {deny} present in oc dest"));
        }
    }
    None
}

/// Write the newline- and NUL-separated list files selecting the same subset.
///
/// Both list the required entries in order. The newline file terminates each
/// entry with `\n`; the `--from0` file terminates each with a NUL byte (`\0`),
/// which is how rsync distinguishes entries when `--from0` is set.
fn write_lists(list: &Path, list0: &Path) -> Result<(), String> {
    let newline: String = REQUIRED.iter().map(|e| format!("{e}\n")).collect();
    let mut nul: Vec<u8> = Vec::new();
    for entry in REQUIRED {
        nul.extend_from_slice(entry.as_bytes());
        nul.push(0);
    }
    std::fs::write(list, newline.as_bytes()).map_err(|e| e.to_string())?;
    std::fs::write(list0, &nul).map_err(|e| e.to_string())
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

/// Build the files-from fixture: three top-level files and a subdirectory with
/// two files. Idempotent: removes any prior tree first. Mtimes are backdated so
/// the quick-check does not skip the listed files under test.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;

    std::fs::write(src.join("a.txt"), b"a\n").map_err(|e| e.to_string())?;
    std::fs::write(src.join("b.txt"), b"b\n").map_err(|e| e.to_string())?;
    std::fs::write(src.join("c.txt"), b"c\n").map_err(|e| e.to_string())?;
    std::fs::write(sub.join("d.txt"), b"d\n").map_err(|e| e.to_string())?;
    std::fs::write(sub.join("e.txt"), b"e\n").map_err(|e| e.to_string())?;

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
    use super::{subset_problem, write_lists};

    #[test]
    fn subset_problem_accepts_exactly_the_listed_entries() {
        let entries = vec![
            "a.txt".to_string(),
            "sub".to_string(),
            "sub/d.txt".to_string(),
        ];
        assert!(subset_problem(&entries).is_none());
    }

    #[test]
    fn subset_problem_reports_missing_required_entry() {
        let entries = vec!["a.txt".to_string()];
        assert!(subset_problem(&entries).unwrap().contains("sub/d.txt"));
    }

    #[test]
    fn subset_problem_reports_present_forbidden_entry() {
        let entries = vec![
            "a.txt".to_string(),
            "b.txt".to_string(),
            "sub/d.txt".to_string(),
        ];
        assert!(subset_problem(&entries).unwrap().contains("b.txt"));
    }

    #[test]
    fn write_lists_uses_newline_and_nul_separators() {
        let dir = tempfile::tempdir().unwrap();
        let (list, list0) = (dir.path().join("l"), dir.path().join("l0"));
        write_lists(&list, &list0).unwrap();
        assert_eq!(std::fs::read(&list).unwrap(), b"a.txt\nsub/d.txt\n");
        assert_eq!(std::fs::read(&list0).unwrap(), b"a.txt\0sub/d.txt\0");
    }
}
