//! Verbose stdout parity at each `-v` level across every transport.
//!
//! Builds a tiny tree, pulls it with each client over every transport at
//! increasing verbosity (`-v`, `-vv`, `-vvv`), and asserts oc-rsync's verbose
//! stdout is structurally identical to upstream's for that level. Upstream rsync
//! is the ground truth. Volatile numerics (byte counts, offsets, checksums,
//! rates) and the trailing timing summary carry no structural meaning, so both
//! sides are normalized - digit runs collapsed to a placeholder, summary and
//! rate lines dropped - before the line vectors are compared for equality. What
//! survives is the structural shape of the output, which a drop-in must match.

use std::path::Path;
use std::process::Output;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The verbose-output parity check.
pub struct Verbosity;

/// Verbosity levels exercised, one matrix cell each.
const LEVELS: &[&str] = &["-v", "-vv", "-vvv"];

/// Base flags shared by every cell; the level is appended per run.
const BASE_FLAGS: &[&str] = &["-rlptgoD", "--numeric-ids"];

/// Placeholder substituted for every volatile numeric run.
const PLACEHOLDER: &str = "#";

/// Trailing units a numeric run may absorb, longest-first so `KB` wins over `B`.
const UNITS: &[&str] = &["bytes", "KB", "B", "/s", "%"];

impl Check for Verbosity {
    fn name(&self) -> &'static str {
        "verbosity"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("verbosity");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let expected = support::entry_count(&src);

        let mut outcomes = Vec::new();
        for &transport in ctx.transports {
            for level in LEVELS {
                outcomes.push(self.cell(ctx, transport, &root, &src, level, expected));
            }
        }
        outcomes
    }
}

impl Verbosity {
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        src: &Path,
        level: &str,
        expected: usize,
    ) -> CheckOutcome {
        let label = transport.label();
        let cell = format!("{label} {level}");
        if transport.needs_ssh() && !support::ssh_ready() {
            return CheckOutcome::skip(self.name(), cell, "no sshd on localhost:22");
        }

        let flags: Vec<String> = BASE_FLAGS
            .iter()
            .chain(std::iter::once(&level))
            .map(|s| s.to_string())
            .collect();
        let slug = level.trim_start_matches('-');
        let oc_dst = root.join(format!("oc-{label}-{slug}"));
        let up_dst = root.join(format!("up-{label}-{slug}"));

        let up = match pull_into(
            transport.for_upstream(),
            ctx.upstream,
            ctx.upstream,
            src,
            &up_dst,
            &flags,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), &cell, "upstream", other),
        };
        let oc = match pull_into(
            transport,
            ctx.oc,
            ctx.upstream,
            src,
            &oc_dst,
            &flags,
            ctx.work,
        ) {
            Ok(out) if out.status.success() => out,
            other => return skip_or_fail(self.name(), &cell, "oc", other),
        };

        // Genuine-result guard: both trees must be fully populated.
        if support::entry_count(&up_dst) != expected || support::entry_count(&oc_dst) != expected {
            return CheckOutcome::fail(self.name(), cell, "destination entry count != source");
        }

        let oc_norm = normalize(&String::from_utf8_lossy(&oc.stdout));
        let up_norm = normalize(&String::from_utf8_lossy(&up.stdout));

        if oc_norm != up_norm {
            if ctx.verbose {
                eprintln!(
                    "[verbosity/{cell}] oc={oc_norm:?}\n[verbosity/{cell}] upstream={up_norm:?}"
                );
            }
            if let Some((i, a, b)) = first_diff(&oc_norm, &up_norm) {
                return CheckOutcome::fail(
                    self.name(),
                    cell,
                    format!("line {i}: oc {a:?} vs upstream {b:?}"),
                );
            }
            return CheckOutcome::fail(self.name(), cell, "verbose stdout differs");
        }
        CheckOutcome::pass(self.name(), cell)
    }
}

/// Reduce verbose stdout to its structural lines for equality comparison.
///
/// Returns the non-empty, trimmed lines with volatile content neutralized:
/// every run of ASCII digits (with embedded `.`/`,` and an optional trailing
/// unit such as `bytes`, `KB`, `B`, `%`, `/s`) collapses to a fixed placeholder,
/// and the trailing `sent .../received .../total size .../speedup` summary plus
/// any `bytes/sec` rate line - all of which encode timing or byte counts - are
/// dropped. Only structural differences remain.
pub fn normalize(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !is_summary_line(line))
        .map(neutralize)
        .collect()
}

/// True for the trailing summary and rate lines that encode timing / counts.
fn is_summary_line(line: &str) -> bool {
    line.starts_with("sent ")
        || line.starts_with("total size is")
        || line.contains("bytes/sec")
        || line.contains("speedup is")
}

/// Collapse every volatile numeric run in one line to [`PLACEHOLDER`].
fn neutralize(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(first) = rest.chars().next() {
        if first.is_ascii_digit() {
            rest = strip_unit(&rest[numeric_len(rest)..]);
            out.push_str(PLACEHOLDER);
        } else {
            out.push(first);
            rest = &rest[first.len_utf8()..];
        }
    }
    out
}

/// Byte length of the leading numeric run (digits plus embedded `.` and `,`).
fn numeric_len(rest: &str) -> usize {
    rest.char_indices()
        .take_while(|&(_, c)| c.is_ascii_digit() || c == '.' || c == ',')
        .map(|(i, c)| i + c.len_utf8())
        .last()
        .unwrap_or(0)
}

/// Strip an optional trailing unit (after at most one space) following a number.
///
/// Alphabetic units must end on a word boundary so a size like `100B` is
/// absorbed while a name like `100Bravo` keeps its letters.
fn strip_unit(rest: &str) -> &str {
    let candidate = rest.strip_prefix(' ').unwrap_or(rest);
    for unit in UNITS {
        if let Some(after) = candidate.strip_prefix(unit) {
            let alpha = unit.chars().all(|c| c.is_ascii_alphabetic());
            let boundary = after
                .chars()
                .next()
                .map(|c| !c.is_ascii_alphanumeric())
                .unwrap_or(true);
            if !alpha || boundary {
                return after;
            }
        }
    }
    rest
}

/// First index where the two normalized vectors differ, with each side's line
/// (or `<missing>` when one vector is shorter).
fn first_diff(oc: &[String], up: &[String]) -> Option<(usize, String, String)> {
    for i in 0..oc.len().max(up.len()) {
        let a = oc.get(i).map(String::as_str).unwrap_or("<missing>");
        let b = up.get(i).map(String::as_str).unwrap_or("<missing>");
        if a != b {
            return Some((i, a.to_string(), b.to_string()));
        }
    }
    None
}

/// Distinguish a genuine divergence from an unrunnable cell (e.g. ssh refused).
fn skip_or_fail(
    check: &'static str,
    label: &str,
    who: &str,
    result: Result<Output, crate::error::TaskError>,
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

/// Build the verbosity fixture. Idempotent: removes any prior tree first.
///
/// A couple of top-level files plus a subdirectory holding one file. Mtimes are
/// backdated so the quick-check makes the same transfer decisions on every run.
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
    use super::{neutralize, normalize};

    #[test]
    fn keeps_structural_lines_and_drops_summary() {
        let out = "receiving incremental file list\n\
                   a.txt\n\
                   sub/\n\
                   \n\
                   sent 100 bytes  received 200 bytes  600.00 bytes/sec\n\
                   total size is 12  speedup is 0.04\n";
        assert_eq!(
            normalize(out),
            vec![
                "receiving incremental file list".to_string(),
                "a.txt".to_string(),
                "sub/".to_string(),
            ]
        );
    }

    #[test]
    fn neutralizes_digit_runs_leaving_structure() {
        // Offsets, counts and checksums are volatile; the labels around them are
        // the structural signal that must survive.
        assert_eq!(
            neutralize("total: matches=0 data=12 tag_hits=3"),
            "total: matches=# data=# tag_hits=#"
        );
    }

    #[test]
    fn absorbs_trailing_units_but_not_following_words() {
        assert_eq!(neutralize("12.5KB 3/s 50%"), "# # #");
        // A size unit is absorbed on a word boundary; a name is left intact.
        assert_eq!(neutralize("100B"), "#");
        assert_eq!(neutralize("100Bravo"), "#Bravo");
    }

    #[test]
    fn identical_verbose_output_normalizes_equal() {
        let oc = "a.txt\n  1,234 bytes\nsent 9 bytes  received 9 bytes  1.00 bytes/sec\n";
        let up = "a.txt\n  5,678 bytes\nsent 7 bytes  received 7 bytes  2.00 bytes/sec\n";
        assert_eq!(normalize(oc), normalize(up));
        // The number and its trailing `bytes` unit collapse to one placeholder.
        assert_eq!(normalize(oc), vec!["a.txt".to_string(), "#".to_string()]);
    }
}
