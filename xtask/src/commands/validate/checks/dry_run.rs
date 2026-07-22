//! `-n -i` dry-run parity: itemized plan matches upstream, nothing is written.
//!
//! Builds a tiny tree and pulls it with each client over every transport using
//! `--dry-run --itemize-changes`. Two invariants are asserted per cell: the
//! ordered list of itemize rows oc-rsync prints must equal upstream rsync's (the
//! plan is identical), and - because it is a dry run - the destination must stay
//! empty (nothing is actually transferred). Upstream rsync is the ground truth
//! for the plan; the empty destination is checked directly on disk.

use std::path::Path;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The dry-run plan-parity / no-write check.
pub struct DryRun;

/// Dry run, itemized, numeric ids, standard archive-ish attribute set.
const FLAGS: &[&str] = &["-rlptgoD", "-n", "-i", "--numeric-ids"];

impl Check for DryRun {
    fn name(&self) -> &'static str {
        "dry-run"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("dry-run");
        let src = root.join("src");
        if let Err(e) = support::build_backdated_tree(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        ctx.transports
            .iter()
            .map(|&t| self.cell(ctx, t, &root, &src, &flags))
            .collect()
    }
}

impl DryRun {
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        src: &Path,
        flags: &[String],
    ) -> CheckOutcome {
        let label = transport.label();
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }
        let oc_dst = root.join(format!("oc-{label}"));
        let up_dst = root.join(format!("up-{label}"));

        let up = match pull_into(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            src,
            &up_dst,
            flags,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), label, "upstream", other),
        };
        let oc = match pull_into(
            transport,
            ctx.oc,
            ctx.upstream,
            src,
            &oc_dst,
            flags,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), label, "oc", other),
        };

        // No-write invariant: a dry run must transfer nothing, so both
        // destinations stay empty. (The normal populated-dest guard is
        // deliberately not applied here - an empty dest is the pass condition.)
        if support::entry_count(&oc_dst) != 0 {
            return CheckOutcome::fail(
                self.name(),
                label,
                "oc wrote into destination on --dry-run",
            );
        }
        if support::entry_count(&up_dst) != 0 {
            return CheckOutcome::fail(
                self.name(),
                label,
                "upstream wrote into destination on --dry-run",
            );
        }

        let up_rows = itemize_rows(&String::from_utf8_lossy(&up.stdout));
        let oc_rows = itemize_rows(&String::from_utf8_lossy(&oc.stdout));

        // Genuine-result guard: an empty plan proves nothing, so require a
        // non-trivial plan on both sides.
        if up_rows.is_empty() || oc_rows.is_empty() {
            return CheckOutcome::fail(
                self.name(),
                label,
                "empty itemize plan (nothing to compare)",
            );
        }

        if let Some((idx, oc_row, up_row)) = first_diff(&oc_rows, &up_rows) {
            if ctx.verbose {
                eprintln!("[dry-run/{label}] oc plan:\n  {}", oc_rows.join("\n  "));
                eprintln!(
                    "[dry-run/{label}] upstream plan:\n  {}",
                    up_rows.join("\n  ")
                );
            }
            return CheckOutcome::fail(
                self.name(),
                label,
                format!("itemize row {idx} differs: oc `{oc_row}` vs upstream `{up_row}`"),
            );
        }
        CheckOutcome::pass(self.name(), label)
    }
}

/// Collect the ordered itemize rows from one client's stdout.
///
/// An itemize row starts with rsync's 11-char change string, so its first
/// character is a transfer/change/hardlink/dir/no-change marker (`<>ch.*`), it
/// contains a space before the path, and it is at least 12 characters long.
/// Summary lines (`sent`, `total size`, file-list notices) match none of these
/// and are dropped, leaving just the plan in transfer order.
fn itemize_rows(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| is_itemize_row(line))
        .map(str::to_string)
        .collect()
}

/// Heuristically detect an itemize row by its leading change string.
fn is_itemize_row(line: &str) -> bool {
    let Some(first) = line.chars().next() else {
        return false;
    };
    matches!(first, '<' | '>' | 'c' | 'h' | '.' | '*') && line.len() >= 12 && line.contains(' ')
}

/// First index at which two plans differ, with both rows (a missing row on
/// either side is reported as an empty string). `None` when the plans are equal.
fn first_diff(oc: &[String], up: &[String]) -> Option<(usize, String, String)> {
    for idx in 0..oc.len().max(up.len()) {
        let oc_row = oc.get(idx).map(String::as_str).unwrap_or("");
        let up_row = up.get(idx).map(String::as_str).unwrap_or("");
        if oc_row != up_row {
            return Some((idx, oc_row.to_string(), up_row.to_string()));
        }
    }
    None
}

/// Distinguish a genuine divergence from an unrunnable cell (e.g. ssh refused).
fn skip_or_fail(
    check: &'static str,
    label: &str,
    who: &str,
    result: Result<std::process::Output, crate::error::TaskError>,
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
    use super::{first_diff, is_itemize_row, itemize_rows};

    #[test]
    fn extracts_only_itemize_rows_in_order() {
        let out = ">f+++++++++ a.txt\n\
                   >f+++++++++ b.txt\n\
                   cd+++++++++ sub/\n\
                   >f+++++++++ sub/c.txt\n\
                   \n\
                   sent 100 bytes  received 20 bytes\n\
                   total size is 12  speedup is 0.10\n";
        assert_eq!(
            itemize_rows(out),
            vec![
                ">f+++++++++ a.txt",
                ">f+++++++++ b.txt",
                "cd+++++++++ sub/",
                ">f+++++++++ sub/c.txt",
            ]
        );
    }

    #[test]
    fn summary_lines_are_not_itemize_rows() {
        assert!(!is_itemize_row("sent 100 bytes  received 20 bytes"));
        assert!(!is_itemize_row(""));
        assert!(is_itemize_row(">f+++++++++ a.txt"));
    }

    #[test]
    fn first_diff_finds_earliest_divergence() {
        let oc = vec![
            ">f+++++++++ a.txt".to_string(),
            "cd+++++++++ x/".to_string(),
        ];
        let up = vec![
            ">f+++++++++ a.txt".to_string(),
            "cd+++++++++ y/".to_string(),
        ];
        assert_eq!(
            first_diff(&oc, &up),
            Some((
                1,
                "cd+++++++++ x/".to_string(),
                "cd+++++++++ y/".to_string()
            ))
        );
    }

    #[test]
    fn first_diff_reports_missing_trailing_row() {
        let oc = vec![">f+++++++++ a.txt".to_string()];
        let up = vec![
            ">f+++++++++ a.txt".to_string(),
            ">f+++++++++ b.txt".to_string(),
        ];
        assert_eq!(
            first_diff(&oc, &up),
            Some((1, String::new(), ">f+++++++++ b.txt".to_string()))
        );
    }

    #[test]
    fn first_diff_none_for_equal_plans() {
        let rows = vec![">f+++++++++ a.txt".to_string()];
        assert_eq!(first_diff(&rows, &rows), None);
    }
}
