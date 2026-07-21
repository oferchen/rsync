//! Reusable comparison facets for validate checks.
//!
//! Every fidelity check ends the same way: it runs oc-rsync and upstream rsync
//! and decides whether the two results agree. The comparison itself splits into
//! a handful of independent *facets* - the destination tree's content, its POSIX
//! metadata, the client's stdout/stderr, the process exit status, and (in a
//! future PR) the captured wire frames. Each facet is deterministic and answers
//! one question, so this module keeps them apart (Interface Segregation) rather
//! than behind one monolithic "compare everything" call:
//!
//! - [`content_diff`] - destination tree structure + file bytes + symlink targets.
//! - [`metadata_diff`] - per-entry mode/mtime/uid/gid/nlink.
//! - [`VolatileNormalizer`] + [`first_line_diff`] - stdout/stderr equality after
//!   the volatile numeric fields (byte counts, offsets, rates, timings) are
//!   masked, so timing and size jitter carry no weight.
//! - [`exit_code_diff`] and [`classify_failure`] - exit-status parity, and the
//!   Skip-vs-Fail decision when a run does not exit cleanly.
//! - [`WireFrameDiffer`] / [`OrderedFrameDiffer`] - the seam a later PR will use
//!   to compare captured protocol frames in order. Frame *capture* is not
//!   implemented here; the ordered diff over already-captured frames is.
//!
//! The volatile-field masking, the exit classifier, and the tree/attr diffs used
//! to be re-implemented inline in each check; this module is the single home for
//! them so a fix lands in one place.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use crate::commands::validate::CheckOutcome;
use crate::commands::validate::support::rel_entries;
use crate::error::TaskError;

/// Compare two destination trees for entry set, type, file bytes, and symlink
/// targets. Returns the first divergence, or `None` when identical.
///
/// This is the content facet: it ignores ownership and timestamps (that is the
/// metadata facet's job) and asserts only that the same paths exist with the
/// same kind and, for regular files and symlinks, the same payload.
pub fn content_diff(a: &Path, b: &Path) -> Option<String> {
    let (ea, eb) = (rel_entries(a), rel_entries(b));
    if ea != eb {
        return Some(format!(
            "entry set differs ({} vs {} entries)",
            ea.len(),
            eb.len()
        ));
    }
    for rel in &ea {
        let (pa, pb) = (a.join(rel), b.join(rel));
        let (ma, mb) = match (pa.symlink_metadata(), pb.symlink_metadata()) {
            (Ok(ma), Ok(mb)) => (ma, mb),
            _ => return Some(format!("cannot stat {}", rel.display())),
        };
        let (ta, tb) = (ma.file_type(), mb.file_type());
        if ta.is_symlink() != tb.is_symlink() || ta.is_dir() != tb.is_dir() {
            return Some(format!("type differs at {}", rel.display()));
        }
        if ta.is_symlink() {
            if std::fs::read_link(&pa).ok() != std::fs::read_link(&pb).ok() {
                return Some(format!("symlink target differs at {}", rel.display()));
            }
        } else if ta.is_file() && std::fs::read(&pa).ok() != std::fs::read(&pb).ok() {
            return Some(format!("file content differs at {}", rel.display()));
        }
    }
    None
}

/// Per-entry POSIX attributes compared by [`metadata_diff`], in field order.
type Attrs = (u32, i64, u32, u32, u64);

/// Map a tree to per-entry `(mode & 07777, mtime, uid, gid, nlink)`.
fn attr_map(root: &Path) -> BTreeMap<PathBuf, Attrs> {
    rel_entries(root)
        .into_iter()
        .filter_map(|rel| {
            let meta = root.join(&rel).symlink_metadata().ok()?;
            let attrs = (
                meta.mode() & 0o7777,
                meta.mtime(),
                meta.uid(),
                meta.gid(),
                meta.nlink(),
            );
            Some((rel, attrs))
        })
        .collect()
}

/// First per-attribute divergence between two trees, or `None` when identical.
///
/// The metadata facet: it assumes the content facet has already confirmed the
/// entry sets match, and reports the first entry whose permissions, mtime,
/// ownership, or link count differ.
pub fn metadata_diff(oc: &Path, up: &Path) -> Option<String> {
    let (a, b) = (attr_map(oc), attr_map(up));
    for (rel, oc_attrs) in &a {
        let Some(up_attrs) = b.get(rel) else {
            return Some(format!("missing {} in oc tree", rel.display()));
        };
        const FIELDS: [&str; 5] = ["perms", "mtime", "uid", "gid", "nlink"];
        let mismatches: Vec<&str> = FIELDS
            .iter()
            .enumerate()
            .filter(|(i, _)| !attr_eq(*i, oc_attrs, up_attrs))
            .map(|(_, name)| *name)
            .collect();
        if !mismatches.is_empty() {
            return Some(format!(
                "{} differs at {}",
                mismatches.join("/"),
                rel.display()
            ));
        }
    }
    None
}

/// Compare one attribute field (by index into the tuple) between two entries.
fn attr_eq(field: usize, a: &Attrs, b: &Attrs) -> bool {
    match field {
        0 => a.0 == b.0,
        1 => a.1 == b.1,
        2 => a.2 == b.2,
        3 => a.3 == b.3,
        _ => a.4 == b.4,
    }
}

/// Masks the volatile numeric fields in one line of rsync output so equality
/// comparison ignores run-to-run jitter (Strategy pattern: the placeholder and
/// the recognized units are the configurable policy).
///
/// rsync interleaves genuinely volatile numbers into otherwise stable output:
/// byte counts and offsets depend on delta/compression decisions, checksums are
/// content-derived, and rates and elapsed times depend on wall-clock timing.
/// None of those can match a second implementation byte-for-byte, so each run of
/// digits (with embedded `.`/`,` grouping) collapses to a single placeholder,
/// absorbing an optional trailing unit so the label around it is what survives.
pub struct VolatileNormalizer {
    /// Substituted for every collapsed numeric run.
    placeholder: &'static str,
    /// Units a numeric run may absorb, longest-first so `KB` wins over `B`.
    units: &'static [&'static str],
}

impl VolatileNormalizer {
    /// Normalizer for rsync verbose/stats stdout: collapses byte counts,
    /// offsets, checksums, transfer rates, and percentages to `#`.
    pub const fn rsync_verbose() -> Self {
        Self {
            placeholder: "#",
            units: &["bytes", "KB", "B", "/s", "%"],
        }
    }

    /// Collapse every volatile numeric run in one line to the placeholder.
    pub fn normalize_line(&self, line: &str) -> String {
        let mut out = String::with_capacity(line.len());
        let mut rest = line;
        while let Some(first) = rest.chars().next() {
            if first.is_ascii_digit() {
                rest = self.strip_unit(&rest[numeric_len(rest)..]);
                out.push_str(self.placeholder);
            } else {
                out.push(first);
                rest = &rest[first.len_utf8()..];
            }
        }
        out
    }

    /// Strip an optional trailing unit (after at most one space) following a
    /// number. Alphabetic units must end on a word boundary so a size like
    /// `100B` is absorbed while a name like `100Bravo` keeps its letters.
    fn strip_unit<'a>(&self, rest: &'a str) -> &'a str {
        let candidate = rest.strip_prefix(' ').unwrap_or(rest);
        for unit in self.units {
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
}

/// Byte length of the leading numeric run (digits plus embedded `.` and `,`).
fn numeric_len(rest: &str) -> usize {
    rest.char_indices()
        .take_while(|&(_, c)| c.is_ascii_digit() || c == '.' || c == ',')
        .map(|(i, c)| i + c.len_utf8())
        .last()
        .unwrap_or(0)
}

/// First index where two normalized line vectors differ, with each side's line
/// (or `<missing>` when one vector is shorter). `None` when they are equal.
///
/// The output facet's comparator: run both clients' kept-and-normalized lines
/// through this to get a precise, human-readable first divergence.
pub fn first_line_diff(oc: &[String], up: &[String]) -> Option<(usize, String, String)> {
    for i in 0..oc.len().max(up.len()) {
        let a = oc.get(i).map(String::as_str).unwrap_or("<missing>");
        let b = up.get(i).map(String::as_str).unwrap_or("<missing>");
        if a != b {
            return Some((i, a.to_string(), b.to_string()));
        }
    }
    None
}

/// Compare two successful runs' process exit codes for parity. `None` when they
/// agree, else a diagnostic. A drop-in must exit with the same code as upstream
/// for the same inputs, so this catches a silent status divergence even when the
/// destination trees happen to match.
pub fn exit_code_diff(oc: &Output, up: &Output) -> Option<String> {
    let (a, b) = (oc.status.code(), up.status.code());
    if a == b {
        return None;
    }
    Some(format!("exit code differs (oc {a:?} vs upstream {b:?})"))
}

/// Classify a transfer whose `Result<Output>` was not a clean success.
///
/// The exit facet's other half: an OS-level spawn error means the cell could not
/// run (e.g. ssh refused, tool missing) and is a [`CheckOutcome::skip`]; a
/// process that ran but exited non-zero is a real divergence and is a
/// [`CheckOutcome::fail`] tagged with the peer, its exit code, and its stderr.
pub fn classify_failure(
    check: &'static str,
    cell: &str,
    who: &str,
    result: Result<Output, TaskError>,
) -> CheckOutcome {
    match result {
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let code = out.status.code().unwrap_or(-1);
            CheckOutcome::fail(
                check,
                cell.to_string(),
                format!("{who} exited {code}: {}", stderr.trim()),
            )
        }
        Err(e) => CheckOutcome::skip(check, cell.to_string(), format!("{who} could not run: {e}")),
    }
}

/// One captured protocol frame as `(tag, payload)`. A tuple for now; a later
/// wire-capture PR promotes this to a struct as the fields it needs grow.
pub type WireFrame = (u8, Vec<u8>);

/// Compares two ordered streams of captured wire frames for byte-and-order
/// fidelity. This is the seam for a future wire-frame diff: the trait and its
/// ordered implementation exist and are exercised, but no transport captures
/// frames yet, so callers pass empty streams today.
pub trait WireFrameDiffer {
    /// First divergence between oc's and upstream's frame streams, or `None`
    /// when the two streams are byte-for-byte identical in order.
    fn diff(&self, oc: &[WireFrame], upstream: &[WireFrame]) -> Option<String>;
}

/// Wire-frame differ that compares frames position by position, first mismatch
/// wins - mirroring rsync's requirement that frames match in both bytes and
/// order.
pub struct OrderedFrameDiffer;

impl WireFrameDiffer for OrderedFrameDiffer {
    fn diff(&self, oc: &[WireFrame], upstream: &[WireFrame]) -> Option<String> {
        if oc.len() != upstream.len() {
            return Some(format!(
                "frame count differs ({} vs {})",
                oc.len(),
                upstream.len()
            ));
        }
        for (i, (a, b)) in oc.iter().zip(upstream).enumerate() {
            if a != b {
                return Some(format!(
                    "frame {i} differs (oc tag={} len={} vs upstream tag={} len={})",
                    a.0,
                    a.1.len(),
                    b.0,
                    b.1.len()
                ));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn content_diff_none_for_identical_trees() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for root in [a.path(), b.path()] {
            fs::write(root.join("f"), b"same").unwrap();
            symlink("f", root.join("l")).unwrap();
        }
        assert!(content_diff(a.path(), b.path()).is_none());
    }

    #[test]
    fn content_diff_reports_content_symlink_and_set_differences() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        fs::write(a.path().join("f"), b"alpha").unwrap();
        fs::write(b.path().join("f"), b"beta").unwrap();
        assert!(
            content_diff(a.path(), b.path())
                .unwrap()
                .contains("content differs")
        );

        let (c, d) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        symlink("x", c.path().join("l")).unwrap();
        symlink("y", d.path().join("l")).unwrap();
        assert!(
            content_diff(c.path(), d.path())
                .unwrap()
                .contains("symlink target")
        );

        let (e, f) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        fs::write(e.path().join("only"), b"").unwrap();
        assert!(
            content_diff(e.path(), f.path())
                .unwrap()
                .contains("entry set")
        );
    }

    #[test]
    fn metadata_diff_none_for_hard_linked_identical_entries() {
        // A hard link shares the inode, so mode/mtime/uid/gid/nlink all match
        // without depending on a GNU-only `touch -d @epoch` (portable to BSD).
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let source = a.path().join("f");
        fs::write(&source, b"x").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
        fs::hard_link(&source, b.path().join("f")).unwrap();
        assert!(metadata_diff(a.path(), b.path()).is_none());
    }

    #[test]
    fn metadata_diff_names_the_diverging_permission() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        fs::write(a.path().join("f"), b"x").unwrap();
        fs::write(b.path().join("f"), b"x").unwrap();
        fs::set_permissions(a.path().join("f"), fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(b.path().join("f"), fs::Permissions::from_mode(0o600)).unwrap();
        assert!(metadata_diff(a.path(), b.path()).unwrap().contains("perms"));
    }

    #[test]
    fn normalizer_collapses_digit_runs_leaving_structure() {
        // Offsets, counts and checksums are volatile; the labels around them are
        // the structural signal that must survive.
        let norm = VolatileNormalizer::rsync_verbose();
        assert_eq!(
            norm.normalize_line("total: matches=0 data=12 tag_hits=3"),
            "total: matches=# data=# tag_hits=#"
        );
    }

    #[test]
    fn normalizer_absorbs_trailing_units_but_not_following_words() {
        let norm = VolatileNormalizer::rsync_verbose();
        // A byte count, a rate, and a percentage all collapse with their unit.
        assert_eq!(norm.normalize_line("12.5KB 3/s 50%"), "# # #");
        // A size unit is absorbed on a word boundary; a name is left intact.
        assert_eq!(norm.normalize_line("100B"), "#");
        assert_eq!(norm.normalize_line("100Bravo"), "#Bravo");
        // Grouping commas inside a count are part of the run, not separators.
        assert_eq!(norm.normalize_line("sent 1,234 bytes"), "sent #");
    }

    #[test]
    fn first_line_diff_finds_index_and_reports_missing() {
        let a = vec!["x".to_string(), "y".to_string()];
        let b = vec!["x".to_string(), "z".to_string()];
        assert_eq!(
            first_line_diff(&a, &b),
            Some((1, "y".to_string(), "z".to_string()))
        );
        // Equal vectors have no divergence.
        assert_eq!(first_line_diff(&a, &a), None);
        // A shorter side reports `<missing>` rather than silently matching.
        let short = vec!["x".to_string()];
        assert_eq!(
            first_line_diff(&a, &short),
            Some((1, "y".to_string(), "<missing>".to_string()))
        );
    }

    #[test]
    fn exit_code_diff_ignores_equal_codes_and_flags_divergence() {
        use std::os::unix::process::ExitStatusExt;
        use std::process::ExitStatus;
        let out = |code: i32| Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert!(exit_code_diff(&out(0), &out(0)).is_none());
        assert!(
            exit_code_diff(&out(0), &out(24))
                .unwrap()
                .contains("exit code differs")
        );
    }

    #[test]
    fn ordered_frame_diff_matches_identical_and_catches_noise() {
        let differ = OrderedFrameDiffer;
        // Empty streams match: this is the state every cell is in today, before
        // any transport captures frames.
        assert!(differ.diff(&[], &[]).is_none());

        let a: Vec<WireFrame> = vec![(7, b"hello".to_vec()), (1, b"data".to_vec())];
        assert!(differ.diff(&a, &a).is_none());

        // A single flipped payload byte is a real divergence, located in order.
        let mut b = a.clone();
        b[1].1 = b"DATA".to_vec();
        assert!(differ.diff(&a, &b).unwrap().contains("frame 1"));

        // A missing trailing frame is caught by the count guard.
        assert!(
            differ
                .diff(&a, &a[..1])
                .unwrap()
                .contains("frame count differs")
        );
    }
}
