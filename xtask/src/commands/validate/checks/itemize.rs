//! `-vi` itemize parity: one itemize row per entry, no duplicate bare name.
//!
//! Builds a tiny tree, pulls it with each client over every transport with
//! `-vi`, and asserts oc-rsync's itemized stdout carries exactly one change
//! string per transferred entry and never a second, bare filename line for the
//! same entry - the divergence this check guards against. Upstream rsync is the
//! ground truth for both the itemize-row count and the (zero) bare-name count.

use std::collections::HashSet;
use std::path::Path;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The itemize no-double-name check.
pub struct Itemize;

/// Itemized verbose transfer, numeric ids, no metadata surprises.
const FLAGS: &[&str] = &["-rlptgoD", "-vi", "--numeric-ids"];

impl Check for Itemize {
    fn name(&self) -> &'static str {
        "itemize"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("itemize");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let names = basenames(&src);
        let expected = support::entry_count(&src);
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        ctx.transports
            .iter()
            .map(|&t| self.cell(ctx, t, &root, &flags, &names, expected))
            .collect()
    }
}

impl Itemize {
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        flags: &[String],
        names: &HashSet<String>,
        expected: usize,
    ) -> CheckOutcome {
        let label = transport.label();
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), label, "no sshd on localhost:22");
        }
        let src = root.join("src");
        let src = src.as_path();
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

        // Genuine-result guard: both trees must be fully populated.
        if support::entry_count(&up_dst) != expected || support::entry_count(&oc_dst) != expected {
            return CheckOutcome::fail(self.name(), label, "destination entry count != source");
        }

        let (up_bare, up_rows) = classify(&String::from_utf8_lossy(&up.stdout), names);
        let (oc_bare, oc_rows) = classify(&String::from_utf8_lossy(&oc.stdout), names);

        if ctx.verbose {
            eprintln!(
                "[itemize/{label}] oc: {oc_rows} rows / {oc_bare} bare, upstream: {up_rows} rows / {up_bare} bare"
            );
        }

        if oc_bare > up_bare {
            let n = oc_bare - up_bare;
            return CheckOutcome::fail(
                self.name(),
                label,
                format!("oc emitted {n} duplicate bare-name line(s)"),
            );
        }
        if oc_rows != up_rows {
            return CheckOutcome::fail(
                self.name(),
                label,
                format!("itemize row count differs (oc {oc_rows} vs upstream {up_rows})"),
            );
        }
        CheckOutcome::pass(self.name(), label)
    }
}

/// Count itemize rows and forbidden bare-name lines in one client's stdout.
///
/// An itemize row starts with rsync's 11-char change string, so its first
/// character is a transfer/change/hardlink/dir/no-change marker (`<>ch.*`), it
/// contains a space before the path, and it is at least 12 characters long. A
/// bare-name line is a trimmed line equal to one source basename - the
/// duplicate this check forbids. Summary lines (`sent`, `total size`, file-list
/// notices) match neither and are ignored.
fn classify(stdout: &str, names: &HashSet<String>) -> (usize, usize) {
    let (mut bare, mut rows) = (0usize, 0usize);
    for raw in stdout.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if names.contains(line) {
            bare += 1;
        } else if is_itemize_row(line) {
            rows += 1;
        }
    }
    (bare, rows)
}

/// Heuristically detect an itemize row by its leading change string.
fn is_itemize_row(line: &str) -> bool {
    let Some(first) = line.chars().next() else {
        return false;
    };
    matches!(first, '<' | '>' | 'c' | 'h' | '.' | '*') && line.len() >= 12 && line.contains(' ')
}

/// Collect every entry's basename (files and dirs) into a set for bare-name
/// detection.
fn basenames(src: &Path) -> HashSet<String> {
    support::rel_entries(src)
        .iter()
        .filter_map(|rel| rel.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect()
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

/// Build the itemize fixture. Idempotent: removes any prior tree first.
fn build_fixture(src: &Path) -> Result<(), String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub = src.join("sub");
    std::fs::create_dir_all(&sub).map_err(|e| e.to_string())?;

    std::fs::write(src.join("a.txt"), b"alpha").map_err(|e| e.to_string())?;
    std::fs::write(src.join("b.txt"), b"bravo").map_err(|e| e.to_string())?;
    std::fs::write(sub.join("c.txt"), b"charlie").map_err(|e| e.to_string())?;

    // Backdate mtimes so the quick-check does not skip anything under test.
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
    use super::classify;
    use std::collections::HashSet;

    fn names() -> HashSet<String> {
        ["a.txt", "b.txt", "sub"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn counts_rows_and_ignores_summary_lines() {
        let out = ">f+++++++++ a.txt\n\
                   >f+++++++++ b.txt\n\
                   cd+++++++++ sub/\n\
                   \n\
                   sent 100 bytes  received 20 bytes\n\
                   total size is 12  speedup is 0.10\n";
        assert_eq!(classify(out, &names()), (0, 3));
    }

    #[test]
    fn detects_forbidden_duplicate_bare_name() {
        // An itemize row PLUS a bare filename for the same entry is the bug.
        assert_eq!(classify(">f+++++++++ a.txt\na.txt\n", &names()), (1, 1));
    }

    #[test]
    fn row_containing_basename_substring_is_not_bare() {
        // The path inside a change row must not be miscounted as a bare name.
        assert_eq!(classify(">f+++++++++ a.txt\n", &names()), (0, 1));
    }
}
