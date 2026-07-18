//! Flist banner direction parity between oc-rsync and upstream.
//!
//! Upstream gates the `sending ... file list` banner on the sender side
//! (`flist.c`, printed when `!am_server`), while the receiver prints
//! `receiving ... file list`. So the banner's direction word encodes who built
//! the file list: a LOCAL copy and an ssh PUSH both send it, an ssh PULL
//! receives it. This check runs three fixed direction cells - `local`, `pull`,
//! `push` - and asserts oc-rsync prints the same direction word upstream does,
//! and that it matches the expected direction. It is direction-based, not
//! per-transport, so it ignores `ctx.transports`; the ssh cells are skipped when
//! no sshd answers on localhost:22.

use std::path::Path;
use std::process::{Command, Output};

use crate::commands::validate::support;
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};
use crate::error::{TaskError, TaskResult};

/// The flist banner direction check.
pub struct Banner;

/// Recursive + verbose: `-v` makes rsync print the incremental file-list banner.
const FLAGS: &[&str] = &["-rv"];

/// The three fixed direction cells and the banner word each must print.
const CELLS: [(&str, &str); 3] = [
    ("local", "sending"),
    ("pull", "receiving"),
    ("push", "sending"),
];

impl Check for Banner {
    fn name(&self) -> &'static str {
        "flist-banner"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("banner");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        CELLS
            .iter()
            .map(|&(cell, expected)| self.cell(ctx, &root, &src, cell, expected))
            .collect()
    }
}

impl Banner {
    /// Run one direction cell for both clients and compare their banner words.
    fn cell(
        &self,
        ctx: &ValidateCtx,
        root: &Path,
        src: &Path,
        cell: &'static str,
        expected: &'static str,
    ) -> CheckOutcome {
        if cell != "local" && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), cell, "no sshd on localhost:22");
        }

        let up_dst = root.join(format!("up-{cell}"));
        let up_dir = match run_cell(ctx.upstream, ctx.upstream, src, &up_dst, cell) {
            Ok(out) => banner_direction(&combined_output(&out)),
            Err(e) => {
                return CheckOutcome::skip(
                    self.name(),
                    cell,
                    format!("upstream could not run: {e}"),
                );
            }
        };

        let oc_dst = root.join(format!("oc-{cell}"));
        let oc_dir = match run_cell(ctx.oc, ctx.upstream, src, &oc_dst, cell) {
            Ok(out) => banner_direction(&combined_output(&out)),
            Err(e) => {
                return CheckOutcome::skip(self.name(), cell, format!("oc could not run: {e}"));
            }
        };

        if ctx.verbose {
            eprintln!("[flist-banner/{cell}] oc={oc_dir} upstream={up_dir}");
        }

        if oc_dir == up_dir && oc_dir == expected {
            CheckOutcome::pass(self.name(), cell)
        } else {
            CheckOutcome::fail(
                self.name(),
                cell,
                format!("oc={oc_dir} upstream={up_dir} expected={expected}"),
            )
        }
    }
}

/// Build and run one direction cell's rsync command, capturing its output.
///
/// The destination is (re)created empty first. `local` is a plain filesystem
/// copy; `pull` makes `client` the receiver of an ssh subprocess transfer whose
/// sender is `upstream` (via `--rsync-path`); `push` makes `client` the sender,
/// with the destination expressed as an absolute `localhost:<path>` remote.
fn run_cell(
    client: &Path,
    upstream: &Path,
    src: &Path,
    dst: &Path,
    cell: &str,
) -> TaskResult<Output> {
    reset_dir(dst)?;
    let src_arg = format!("{}/", src.display());
    let dst_local = format!("{}/", dst.display());
    let mut cmd = Command::new(client);
    cmd.args(FLAGS);

    match cell {
        "local" => {
            cmd.arg(&src_arg).arg(&dst_local);
        }
        "pull" => {
            cmd.arg(format!("--rsync-path={}", upstream.display()))
                .arg("-e")
                .arg("ssh -o BatchMode=yes -o StrictHostKeyChecking=no")
                .arg(format!("localhost:{}/", src.display()))
                .arg(&dst_local);
        }
        _ => {
            cmd.arg(format!("--rsync-path={}", upstream.display()))
                .arg("-e")
                .arg("ssh -o BatchMode=yes -o StrictHostKeyChecking=no")
                .arg(&src_arg)
                .arg(format!("localhost:{}/", dst.display()));
        }
    }

    cmd.output()
        .map_err(|e| TaskError::Validation(format!("failed to spawn {cmd:?}: {e}")))
}

/// Fold a run's stdout then stderr into one string; the banner can land on
/// either stream depending on the transport.
fn combined_output(out: &Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

/// Classify the first `file list` banner line as `sending`, `receiving`, or
/// `none` (no banner or an unrecognized one).
pub fn banner_direction(output: &str) -> &'static str {
    for line in output.lines() {
        if line.contains("file list") {
            if line.contains("sending") {
                return "sending";
            }
            if line.contains("receiving") {
                return "receiving";
            }
            return "none";
        }
    }
    "none"
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

/// Build the banner fixture: two top-level files and a subdirectory with one
/// file. Idempotent: removes any prior tree first. No metadata or backdating is
/// needed since only the banner line - not transfer decisions - is inspected.
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
    use super::banner_direction;

    #[test]
    fn classifies_sending_and_receiving() {
        assert_eq!(
            banner_direction("sending incremental file list\n"),
            "sending"
        );
        assert_eq!(
            banner_direction("receiving incremental file list\n"),
            "receiving"
        );
    }

    #[test]
    fn first_file_list_line_decides_direction() {
        let out = "opening connection\n\
                   receiving incremental file list\n\
                   some later line mentioning sending\n";
        assert_eq!(banner_direction(out), "receiving");
    }

    #[test]
    fn none_without_a_banner_line() {
        assert_eq!(banner_direction("a file\nanother file\n"), "none");
    }
}
