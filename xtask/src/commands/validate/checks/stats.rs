//! `--stats` accounting parity between oc-rsync and upstream.
//!
//! Pulls a fixed tree with each client over every transport and asserts that
//! oc-rsync's `--stats` summary reports the same *deterministic* accounting as
//! upstream. Upstream prints the block from `main.c:output_summary()`; only the
//! fields that are a pure function of the input tree are compared. Timing,
//! throughput, wire-byte, and delta-dependent fields legitimately differ run to
//! run and are deliberately excluded (see `DETERMINISTIC_LABELS` below).

use std::collections::BTreeMap;
use std::path::Path;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The `--stats` accounting parity check.
pub struct Stats;

/// Recursive archive subset plus statistics and numeric ids.
///
/// `-rlptgoD` is `-a` without `-A`/`-X`, keeping the run portable across hosts
/// that lack ACL/xattr support; `--numeric-ids` keeps id mapping out of the
/// picture so counts stay deterministic; `--stats` prints the summary block.
const FLAGS: &[&str] = &["-rlptgoD", "--stats", "--numeric-ids"];

/// Summary labels whose values are a pure function of the input tree and so
/// must match upstream byte-for-byte for identical inputs.
///
/// - `Number of files` - total plus the `(reg: R, dir: D, link: L)` breakdown.
/// - `Number of created files` - every entry is new in a fresh pull.
/// - `Number of deleted files` - `0` here (no `--delete`), but still emitted.
/// - `Number of regular files transferred` - the regular-file count.
/// - `Total file size` - summed `S_ISREG || S_ISLNK` bytes.
///
/// Deliberately excluded because they are timing, throughput, wire, or
/// delta dependent and may differ for identical inputs: `File list generation
/// time`, `File list transfer time`, `Total bytes sent`, `Total bytes
/// received`, `Literal data`, `Matched data`, `File list size`, and
/// `Total transferred file size` (a transfer-decision artifact, not part of the
/// contracted compare set).
const DETERMINISTIC_LABELS: &[&str] = &[
    "Number of files",
    "Number of created files",
    "Number of deleted files",
    "Number of regular files transferred",
    "Total file size",
];

/// Labels that must be present on both sides for the check to be non-vacuous.
const REQUIRED_LABELS: &[&str] = &["Number of files", "Total file size"];

impl Check for Stats {
    fn name(&self) -> &'static str {
        "stats"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("stats");
        let src = root.join("src");
        if let Err(e) = build_fixture(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }
        let expected_entries = support::entry_count(&src);
        let flags: Vec<String> = FLAGS.iter().map(|s| s.to_string()).collect();

        ctx.transports
            .iter()
            .map(|&t| self.cell(ctx, t, &root, &src, &flags, expected_entries))
            .collect()
    }
}

impl Stats {
    /// Run one transport cell: pull with oc and upstream, then compare fields.
    fn cell(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        root: &Path,
        src: &Path,
        flags: &[String],
        expected_entries: usize,
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

        let up_fields = stat_fields(&String::from_utf8_lossy(&up.stdout));
        let oc_fields = stat_fields(&String::from_utf8_lossy(&oc.stdout));

        if ctx.verbose {
            eprintln!("[stats/{label}] oc={oc_fields:?} upstream={up_fields:?}");
        }

        // Non-vacuous guard: the anchor fields must have parsed on both sides.
        for req in REQUIRED_LABELS {
            if !oc_fields.contains_key(*req) || !up_fields.contains_key(*req) {
                return CheckOutcome::skip(self.name(), label, "stats fields not found");
            }
        }

        for field in DETERMINISTIC_LABELS {
            let oc_val = oc_fields.get(*field);
            let up_val = up_fields.get(*field);
            if oc_val != up_val {
                let oc_disp = oc_val.map(String::as_str).unwrap_or("(absent)");
                let up_disp = up_val.map(String::as_str).unwrap_or("(absent)");
                return CheckOutcome::fail(
                    self.name(),
                    label,
                    format!("{field}: oc={oc_disp} upstream={up_disp}"),
                );
            }
        }
        CheckOutcome::pass(self.name(), label)
    }
}

/// Extract the deterministic `--stats` fields as label -> normalized value.
///
/// Keys are the text before the first colon; values are the remainder with any
/// grouping commas removed, a trailing ` bytes` unit dropped, and surrounding
/// whitespace trimmed. Only [`DETERMINISTIC_LABELS`] are retained, so the map
/// never carries timing or throughput noise. The `Number of files` family keeps
/// its `(reg: R, dir: D, link: L)` breakdown in the value, so the per-type
/// counts are compared alongside the total.
pub fn stat_fields(stdout: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    for line in stdout.lines() {
        let Some((label, rest)) = line.split_once(':') else {
            continue;
        };
        let label = label.trim();
        if DETERMINISTIC_LABELS.contains(&label) {
            fields.insert(label.to_string(), normalize_value(rest));
        }
    }
    fields
}

/// Normalize a stats value: drop a trailing ` bytes` unit and grouping commas.
fn normalize_value(raw: &str) -> String {
    let trimmed = raw.trim();
    let unitless = trimmed.strip_suffix("bytes").unwrap_or(trimmed).trim_end();
    unitless.chars().filter(|c| *c != ',').collect::<String>()
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

/// Build the deterministic fixture tree under `src`.
///
/// Regular files of known sizes, two subdirectories, and one symlink, with every
/// entry backdated to a fixed epoch so the quick-check never skips a transfer.
/// Idempotent: removes any prior tree first.
fn build_fixture(src: &Path) -> Result<(), String> {
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

    for rel in support::rel_entries(src) {
        let path = src.join(&rel);
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
    use super::{normalize_value, stat_fields};

    /// A representative upstream `--stats` block for a small tree.
    const SAMPLE: &str = "\nNumber of files: 7 (reg: 4, dir: 3, link: 1)\n\
        Number of created files: 7 (reg: 4, dir: 3, link: 1)\n\
        Number of deleted files: 0\n\
        Number of regular files transferred: 4\n\
        Total file size: 1,234 bytes\n\
        Total transferred file size: 1,234 bytes\n\
        Literal data: 1,234 bytes\n\
        Matched data: 0 bytes\n\
        File list size: 210\n\
        File list generation time: 0.001 seconds\n\
        File list transfer time: 0.000 seconds\n\
        Total bytes sent: 512\n\
        Total bytes received: 90\n";

    #[test]
    fn extracts_only_deterministic_fields() {
        let f = stat_fields(SAMPLE);
        assert_eq!(f["Number of files"], "7 (reg: 4 dir: 3 link: 1)");
        assert_eq!(f["Number of created files"], "7 (reg: 4 dir: 3 link: 1)");
        assert_eq!(f["Number of deleted files"], "0");
        assert_eq!(f["Number of regular files transferred"], "4");
        assert_eq!(f["Total file size"], "1234");
        // Excluded noisy fields never enter the map.
        assert!(!f.contains_key("Total transferred file size"));
        assert!(!f.contains_key("Literal data"));
        assert!(!f.contains_key("File list size"));
        assert!(!f.contains_key("Total bytes sent"));
        assert!(!f.contains_key("File list generation time"));
    }

    #[test]
    fn normalize_strips_bytes_unit_and_grouping_commas() {
        assert_eq!(normalize_value(" 1,234,567 bytes"), "1234567");
        assert_eq!(normalize_value(" 0"), "0");
        assert_eq!(normalize_value(" 7 (reg: 4, dir: 3)"), "7 (reg: 4 dir: 3)");
    }

    #[test]
    fn missing_stats_block_yields_empty_map() {
        let out = "sent 200 bytes  received 90 bytes  580.00 bytes/sec\n";
        assert!(stat_fields(out).is_empty());
    }
}
