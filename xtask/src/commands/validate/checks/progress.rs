//! `-v --progress` name/progress interleaving parity on the receiver.
//!
//! Builds a fixture of a few large top-level files, pulls it with each client
//! over every transport, and asserts that oc-rsync interleaves each transferred
//! file's NAME line immediately before its PROGRESS/xfr line - the N,P,N,P,...
//! pattern - exactly as upstream does. Progress may land on stdout or stderr, so
//! both streams are folded together before the pattern is derived. Upstream is
//! the ground truth: a cell where upstream itself renders no progress (e.g. a
//! host whose transport buffers the transfer away) is skipped, not failed.

use std::collections::HashSet;
use std::path::Path;
use std::process::Output;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The progress-interleave check.
pub struct Progress;

/// Archive plus a live progress meter, whose per-file lines we inspect.
const FLAGS: &[&str] = &["-av", "--progress"];

/// Number of distinct top-level files in the fixture.
const FILE_COUNT: usize = 4;

/// Base filler size (~3 MB) so each file yields a progress + `(xfr#N,` line.
const BASE_SIZE: usize = 3 * 1024 * 1024;

impl Check for Progress {
    fn name(&self) -> &'static str {
        "progress-interleave"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("progress");
        let src = root.join("src");
        let names = match build_fixture(&src) {
            Ok(names) => names,
            Err(e) => return vec![CheckOutcome::skip(self.name(), "fixture", e)],
        };
        let expected = support::entry_count(&src);
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        ctx.transports
            .iter()
            .map(|&t| self.cell(ctx, t, &root, &src, &flags, &names, expected))
            .collect()
    }
}

impl Progress {
    #[allow(clippy::too_many_arguments)]
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        src: &Path,
        flags: &[String],
        names: &HashSet<String>,
        expected: usize,
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

        // Genuine-result guard: both trees must be fully populated.
        if support::entry_count(&up_dst) != expected || support::entry_count(&oc_dst) != expected {
            return CheckOutcome::fail(self.name(), label, "destination entry count != source");
        }

        let oc_pat = interleave_pattern(&combined_output(&oc), names);
        let up_pat = interleave_pattern(&combined_output(&up), names);
        if ctx.verbose {
            eprintln!("[progress/{label}] oc={oc_pat} upstream={up_pat}");
        }

        if !is_good(&oc_pat) {
            return CheckOutcome::fail(self.name(), label, format!("oc pattern={oc_pat}"));
        }
        if !is_good(&up_pat) {
            return CheckOutcome::skip(self.name(), label, "upstream did not render progress");
        }
        CheckOutcome::pass(self.name(), label)
    }
}

/// Fold a run's stdout then stderr into one string; progress can be on either.
fn combined_output(out: &Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

/// Derive the N/P interleave pattern from combined transfer output.
///
/// The progress meter overwrites its line with carriage returns, so only the
/// segment after the LAST `\r` on each line is meaningful. A segment holding an
/// `(xfr#` marker is a progress line (`P`); a segment that is exactly a source
/// basename is a name line (`N`); everything else (stats, headers) is ignored.
pub fn interleave_pattern(output: &str, names: &HashSet<String>) -> String {
    let mut pat = String::new();
    for line in output.lines() {
        let seg = line.rsplit('\r').next().unwrap_or(line).trim();
        if seg.contains("(xfr#") {
            pat.push('P');
        } else if names.contains(seg) {
            pat.push('N');
        }
    }
    pat
}

/// True when a pattern is a non-empty `NP` repeated (each name immediately
/// followed by its progress line, with at least one transferred file).
pub fn is_good(pat: &str) -> bool {
    if pat.is_empty() || pat.len() % 2 != 0 || !pat.contains('P') {
        return false;
    }
    pat.bytes().enumerate().all(|(i, b)| {
        let expected = if i % 2 == 0 { b'N' } else { b'P' };
        b == expected
    })
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

/// Build the progress fixture and return the set of top-level file basenames.
///
/// Idempotent: removes any prior tree first. Each file is ~3 MB plus a small
/// per-file delta so the sizes are distinct and large enough to emit a progress
/// line and an `(xfr#N,` marker.
fn build_fixture(src: &Path) -> Result<HashSet<String>, String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(src).map_err(|e| e.to_string())?;

    let mut names = HashSet::new();
    for i in 0..FILE_COUNT {
        let name = format!("file{i}.bin");
        let size = BASE_SIZE + i * 64 * 1024;
        std::fs::write(src.join(&name), vec![b'x'; size]).map_err(|e| e.to_string())?;
        names.insert(name);
    }

    // Backdate mtimes so the quick-check does not skip anything under test.
    for entry in support::rel_entries(src) {
        let path = src.join(&entry);
        support::capture("touch", &["-d", "@1614830767", &path.to_string_lossy()])
            .map_err(|e| e.to_string())?;
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::{interleave_pattern, is_good};
    use std::collections::HashSet;

    fn names() -> HashSet<String> {
        ["file0.bin", "file1.bin"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn pattern_uses_segment_after_last_carriage_return() {
        // The progress meter overwrites its line with \r; only the last segment,
        // which carries the (xfr#..) marker, is the real progress line.
        let out = "file0.bin\n 50%\r 100%  (xfr#1, to-chk=0/1)\n";
        assert_eq!(interleave_pattern(out, &names()), "NP");
    }

    #[test]
    fn pattern_interleaves_names_and_progress_ignoring_banners() {
        let out = "sending incremental file list\n\
                   file0.bin\n  1.00M 100%  (xfr#1, to-chk=1/2)\n\
                   file1.bin\n  2.00M 100%  (xfr#2, to-chk=0/2)\n";
        assert_eq!(interleave_pattern(out, &names()), "NPNP");
    }

    #[test]
    fn pattern_empty_without_names_or_markers() {
        assert_eq!(interleave_pattern("random\nlines\n", &names()), "");
    }

    #[test]
    fn is_good_requires_name_then_progress_pairs() {
        assert!(is_good("NP"));
        assert!(is_good("NPNP"));
        assert!(!is_good("")); // nothing transferred
        assert!(!is_good("N")); // name, no progress
        assert!(!is_good("PN")); // progress before name is the regression
        assert!(!is_good("NPN")); // odd length
        assert!(!is_good("NN")); // two names, no progress
    }
}
