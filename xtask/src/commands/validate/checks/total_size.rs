//! `--stats` total-size parity between oc-rsync and upstream.
//!
//! Builds a fixture of regular files, two subdirectories, and a symlink, then
//! pulls it with each client over every transport and asserts oc-rsync's
//! reported `total size` equals upstream's AND equals the byte sum of regular
//! files plus symlinks only. Directory inode sizes must never be counted:
//! upstream `flist.c` sums `total_size` over `S_ISREG || S_ISLNK` entries alone,
//! so a run that folded directory sizes in would diverge from `expected_bytes`.

use std::path::Path;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The total-size parity check.
pub struct TotalSize;

/// Archive plus statistics; `--stats` is what prints the `total size` line.
const FLAGS: &[&str] = &["-a", "--stats"];

impl Check for TotalSize {
    fn name(&self) -> &'static str {
        "total-size"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("total-size");
        let src = root.join("src");
        let expected_bytes = match build_fixture(&src) {
            Ok(bytes) => bytes,
            Err(e) => return vec![CheckOutcome::skip(self.name(), "fixture", e)],
        };
        let expected_entries = support::entry_count(&src);
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        ctx.transports
            .iter()
            .map(|&t| {
                self.cell(
                    ctx,
                    t,
                    &root,
                    &src,
                    &flags,
                    expected_entries,
                    expected_bytes,
                )
            })
            .collect()
    }
}

impl TotalSize {
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        src: &Path,
        flags: &[String],
        expected_entries: usize,
        expected_bytes: u64,
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
        if support::entry_count(&up_dst) != expected_entries
            || support::entry_count(&oc_dst) != expected_entries
        {
            return CheckOutcome::fail(self.name(), label, "destination entry count != source");
        }

        let up_stdout = String::from_utf8_lossy(&up.stdout);
        let oc_stdout = String::from_utf8_lossy(&oc.stdout);
        let Some(up_n) = parse_total_size(&up_stdout) else {
            return CheckOutcome::fail(
                self.name(),
                label,
                "no `total size` line in upstream output",
            );
        };
        let Some(oc_n) = parse_total_size(&oc_stdout) else {
            return CheckOutcome::fail(self.name(), label, "no `total size` line in oc output");
        };

        if ctx.verbose {
            eprintln!("[total-size/{label}] oc={oc_n} upstream={up_n} expected={expected_bytes}");
        }

        if oc_n != up_n || oc_n != expected_bytes {
            return CheckOutcome::fail(
                self.name(),
                label,
                format!("oc={oc_n} upstream={up_n} expected={expected_bytes}"),
            );
        }
        CheckOutcome::pass(self.name(), label)
    }
}

/// Extract `N` from rsync's `total size is N  speedup is M` line.
///
/// rsync always prints this under `--stats`; the integer may carry grouping
/// commas (locale-dependent), which are stripped before parsing.
fn parse_total_size(stdout: &str) -> Option<u64> {
    let line = stdout.lines().find(|l| l.contains("total size is"))?;
    let after = line.split("total size is").nth(1)?;
    let digits: String = after
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .filter(|c| *c != ',')
        .collect();
    digits.parse().ok()
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

/// Build the fixture and return the expected `total size` in bytes.
///
/// The tree has regular files, two subdirectories (whose inode sizes must be
/// excluded), and one symlink. `expected_bytes` sums `symlink_metadata().len()`
/// over regular-file and symlink entries only. A symlink's `len()` is the byte
/// length of its target path, which is exactly what rsync counts for symlinks.
/// Idempotent: removes any prior tree first.
fn build_fixture(src: &Path) -> Result<u64, String> {
    if src.exists() {
        std::fs::remove_dir_all(src).map_err(|e| e.to_string())?;
    }
    let sub1 = src.join("sub1");
    let sub2 = src.join("sub2");
    std::fs::create_dir_all(&sub1).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&sub2).map_err(|e| e.to_string())?;

    std::fs::write(src.join("f1"), b"alpha-file-contents").map_err(|e| e.to_string())?;
    std::fs::write(src.join("f2"), b"bravo").map_err(|e| e.to_string())?;
    std::fs::write(sub1.join("f3"), b"charlie-in-sub1").map_err(|e| e.to_string())?;
    std::fs::write(sub2.join("f4"), b"delta-in-sub2-larger-payload").map_err(|e| e.to_string())?;

    std::os::unix::fs::symlink("../f1", sub1.join("link")).map_err(|e| e.to_string())?;

    // Sum regular-file and symlink bytes; directory inodes are excluded.
    let mut expected_bytes = 0u64;
    for rel in support::rel_entries(src) {
        let path = src.join(&rel);
        let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
        let ft = meta.file_type();
        if ft.is_file() || ft.is_symlink() {
            expected_bytes += meta.len();
        }
        // Backdate mtimes so the quick-check does not skip anything under test.
        support::capture(
            "touch",
            &["-h", "-d", "@1614830767", &path.to_string_lossy()],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(expected_bytes)
}

#[cfg(test)]
mod tests {
    use super::parse_total_size;

    #[test]
    fn parses_plain_total() {
        assert_eq!(
            parse_total_size("total size is 1374  speedup is 1.00\n"),
            Some(1374)
        );
    }

    #[test]
    fn strips_locale_grouping_commas() {
        assert_eq!(
            parse_total_size("total size is 1,234,567  speedup is 2.0\n"),
            Some(1_234_567)
        );
    }

    #[test]
    fn handles_zero_and_missing_line() {
        assert_eq!(
            parse_total_size("total size is 0  speedup is 0.00\n"),
            Some(0)
        );
        assert_eq!(parse_total_size("sent 10 bytes  received 5 bytes\n"), None);
    }

    #[test]
    fn finds_line_within_a_stats_block() {
        let out = "Number of files: 3\n\
                   Total file size: 1374 bytes\n\
                   \n\
                   sent 200 bytes  received 90 bytes\n\
                   total size is 1374  speedup is 4.6\n";
        assert_eq!(parse_total_size(out), Some(1374));
    }
}
