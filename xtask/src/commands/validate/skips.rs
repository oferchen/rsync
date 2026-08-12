//! Skip accounting for the fidelity matrix.
//!
//! A skipped cell is not a passing cell. Before this module the matrix counted
//! skips but let them pass silently, so a run that exercised 5 of 239 cells
//! printed `5 passed, 0 failed` and exited 0 - indistinguishable from success.
//!
//! The model mirrors upstream's, which does not ban skipping but makes a skip
//! impossible to mistake for a pass. `testsuite/rsync.fns:448`:
//!
//! ```text
//! test_skipped() {
//!     echo "$@" >&2
//!     echo "$@" > "$tmpdir/whyskipped"
//!     exit 77
//! }
//! ```
//!
//! Three properties carry over: the reason reaches the operator (stderr), the
//! reason is left behind in a machine-readable artifact (`whyskipped`), and the
//! outcome is distinct from both pass and fail (exit 77).
//!
//! What upstream leaves to the caller - noticing that the skip count has grown -
//! is here made a hard contract: [`ExpectedSkips`] is a declared number, and
//! exceeding it fails the run. The declared number is the contract, not a
//! tolerance; drifting past it is a defect.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::unix_impl::{CheckOutcome, Status};

/// Basename of the machine-readable skip artifact, mirroring the file upstream's
/// `test_skipped()` leaves in its scratch directory.
pub const WHYSKIPPED: &str = "whyskipped";

/// The declared number of cells this matrix is allowed to skip.
///
/// A skip is legitimate when the cell is genuinely inapplicable to the host -
/// there is no sshd to talk to, the work filesystem has no ACL support. It is
/// not legitimate when the harness itself could not set up, which is a defect
/// wearing a skip's clothing. Only the first kind belongs in this budget.
///
/// Raising this number is a deliberate act that must be justified in review, in
/// the same spirit as `tools/ci/known_failures.conf`: a blanket allowance can
/// never report a regression, so it hides the day the limitation is fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedSkips(pub usize);

impl ExpectedSkips {
    /// Cells legitimately inapplicable on a host with no sshd reachable.
    ///
    /// The ssh-subprocess and russh transports contribute two cells per check
    /// that exercises them. Every other skip is a harness or environment defect
    /// and must be fixed rather than budgeted.
    pub const DEFAULT: Self = Self(10);
}

/// Cells the default full selection accounts for - passed, failed, or skipped.
///
/// A skip budget alone cannot catch the worst shape of this defect. When a
/// check's fixture setup fails, some checks collapse the transports they would
/// have run into a single aggregate skip, so the cells do not become skipped -
/// they stop existing. Measured on macOS, where BSD `touch` rejects the
/// backdate helper: the matrix reports 55 cells (5 passed, 0 failed, 50
/// skipped) instead of 239. The missing 184 are counted nowhere, and the
/// totals line simply gets smaller - which reads exactly like a smaller run.
///
/// So the accounting contract is a floor on cells accounted for, checked
/// alongside the skip budget. A floor rather than an equality because
/// `--flags` and `--edge-cases` legitimately add cells; nothing legitimately
/// removes them.
///
/// Measured on macOS/aarch64 with a working backdate helper. It must be
/// re-measured on Linux before the floor is relied on there: if the platforms
/// legitimately differ, this becomes a per-platform floor rather than one
/// constant.
pub const EXPECTED_CELLS: usize = 239;

/// Diagnostic when a run accounted for fewer cells than the floor, or `None`.
///
/// `full_selection` is false when the caller narrowed the run (`--transport`,
/// a category subset), which legitimately produces fewer cells; the floor is
/// meaningless then and is not applied.
pub fn cell_shortfall(total: usize, floor: usize, full_selection: bool) -> Option<String> {
    if !full_selection || total >= floor {
        return None;
    }
    Some(format!(
        "matrix accounted for {total} cell(s), but the full selection declares {floor} - \
         {} cell(s) produced no outcome at all.\n\
         A cell that is neither passed, failed, nor skipped has vanished: its check \
         collapsed or aborted before reporting. Fix the check, or re-measure and update \
         EXPECTED_CELLS if the matrix legitimately changed size.",
        floor - total
    ))
}

/// One skipped cell and the reason it was skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skip {
    /// Check the cell belongs to.
    pub check: String,
    /// Cell label within the check.
    pub cell: String,
    /// Human-readable reason, as passed to `CheckOutcome::skip`.
    pub reason: String,
}

impl Skip {
    /// One record of the machine-readable artifact: `check\tcell\treason`.
    ///
    /// Tab-separated because a reason may contain spaces but never a tab, and
    /// because it keeps the file greppable by check name.
    pub fn record(&self) -> String {
        format!("{}\t{}\t{}", self.check, self.cell, self.reason)
    }
}

/// Collected skips for one run, and the verdict against the declared budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkipLedger {
    skips: Vec<Skip>,
    expected: ExpectedSkips,
}

impl SkipLedger {
    /// Collect every skipped cell from a finished matrix run.
    pub fn from_outcomes(outcomes: &[CheckOutcome], expected: ExpectedSkips) -> Self {
        let skips = outcomes
            .iter()
            .filter(|o| o.status == Status::Skip)
            .map(|o| Skip {
                check: o.check.to_owned(),
                cell: o.cell.clone(),
                reason: o.detail.clone(),
            })
            .collect();
        Self { skips, expected }
    }

    /// Number of skipped cells.
    pub fn count(&self) -> usize {
        self.skips.len()
    }

    /// Skip counts grouped by reason, most frequent first, then alphabetical.
    ///
    /// Grouping is what makes a large skip set readable: 127 cells lost to one
    /// broken fixture helper is one line, not 127.
    pub fn by_reason(&self) -> Vec<(&str, usize)> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for skip in &self.skips {
            *counts.entry(skip.reason.as_str()).or_default() += 1;
        }
        let mut grouped: Vec<(&str, usize)> = counts.into_iter().collect();
        grouped.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        grouped
    }

    /// The machine-readable artifact body, one record per line.
    pub fn artifact(&self) -> String {
        self.skips.iter().map(|s| s.record() + "\n").collect()
    }

    /// Write the artifact under `dir`, returning its path.
    ///
    /// Written unconditionally, including when nothing skipped: an empty
    /// `whyskipped` is evidence the run accounted for skips, whereas a missing
    /// one is ambiguous between "none" and "not checked".
    pub fn write_artifact(&self, dir: &Path) -> std::io::Result<PathBuf> {
        let path = dir.join(WHYSKIPPED);
        std::fs::write(&path, self.artifact())?;
        Ok(path)
    }

    /// `true` when the run skipped more cells than declared.
    pub fn exceeds_budget(&self) -> bool {
        self.count() > self.expected.0
    }

    /// The failure diagnostic when the budget is exceeded, naming the count, the
    /// budget, and the reasons that account for the overage.
    pub fn budget_diagnostic(&self) -> String {
        let mut msg = format!(
            "{} cell(s) skipped, but only {} are declared expected - \
             a skipped cell is not a passing cell.\nSkipped by reason:",
            self.count(),
            self.expected.0
        );
        for (reason, n) in self.by_reason() {
            msg.push_str(&format!("\n  {n:>4}  {reason}"));
        }
        msg.push_str(&format!(
            "\nFix the cause, or raise ExpectedSkips::DEFAULT with justification. \
             Full list: <work>/{WHYSKIPPED}"
        ));
        msg
    }

    /// The prominent per-run summary, printed whether or not the budget holds.
    pub fn summary(&self) -> String {
        if self.skips.is_empty() {
            return format!("skipped: 0 (budget {})", self.expected.0);
        }
        let mut out = format!(
            "skipped: {} of budget {}{}",
            self.count(),
            self.expected.0,
            if self.exceeds_budget() {
                "  << OVER BUDGET"
            } else {
                ""
            }
        );
        for (reason, n) in self.by_reason() {
            out.push_str(&format!("\n  {n:>4}  {reason}"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(status: Status, check: &'static str, cell: &str, detail: &str) -> CheckOutcome {
        CheckOutcome {
            check,
            cell: cell.to_owned(),
            status,
            detail: detail.to_owned(),
        }
    }

    /// Proof (a): a check whose oracle is missing must surface as a COUNTED skip
    /// carrying its reason - not as a pass, and not as silence.
    #[test]
    fn missing_oracle_is_a_counted_skip_carrying_its_reason() {
        let outcomes = vec![outcome(
            Status::Skip,
            "acl_xattr",
            "local",
            "setfacl/getfacl/setfattr/getfattr missing",
        )];
        let ledger = SkipLedger::from_outcomes(&outcomes, ExpectedSkips::DEFAULT);
        assert_eq!(ledger.count(), 1);
        assert_eq!(
            ledger.by_reason(),
            vec![("setfacl/getfacl/setfattr/getfattr missing", 1)]
        );
        assert_eq!(
            ledger.artifact(),
            "acl_xattr\tlocal\tsetfacl/getfacl/setfattr/getfattr missing\n"
        );
    }

    /// Proof (b): a missing fixture is the #371 shape - the harness could not set
    /// the cell up. It must be counted and reason-carrying like any other skip.
    #[test]
    fn missing_fixture_is_a_counted_skip_carrying_its_reason() {
        let outcomes = vec![outcome(
            Status::Skip,
            "verbosity",
            "local -v",
            "touch [\"-h\", \"-d\", \"@1614830767\"] failed: illegal time format",
        )];
        let ledger = SkipLedger::from_outcomes(&outcomes, ExpectedSkips::DEFAULT);
        assert_eq!(ledger.count(), 1);
        assert!(ledger.by_reason()[0].0.contains("illegal time format"));
        assert!(ledger.artifact().starts_with("verbosity\tlocal -v\t"));
    }

    /// Proof (c): an absent peer is the one legitimately-inapplicable class, so
    /// it is still counted and still named - it just fits inside the budget.
    #[test]
    fn absent_peer_is_counted_and_named_but_fits_the_budget() {
        let outcomes: Vec<CheckOutcome> = (0..10)
            .map(|i| {
                outcome(
                    Status::Skip,
                    "metadata",
                    &format!("ssh-subprocess {i}"),
                    "no sshd on localhost:22",
                )
            })
            .collect();
        let ledger = SkipLedger::from_outcomes(&outcomes, ExpectedSkips::DEFAULT);
        assert_eq!(ledger.count(), 10);
        assert!(!ledger.exceeds_budget(), "10 == budget must not trip");
        assert_eq!(ledger.by_reason(), vec![("no sshd on localhost:22", 10)]);
    }

    /// The gate itself: one cell past the declared count fails the run. This is
    /// the assertion that turns a silent 127-of-239 run red.
    #[test]
    fn exceeding_the_declared_count_trips_the_gate() {
        let outcomes: Vec<CheckOutcome> = (0..11)
            .map(|i| outcome(Status::Skip, "metadata", &format!("cell {i}"), "no sshd"))
            .collect();
        let ledger = SkipLedger::from_outcomes(&outcomes, ExpectedSkips::DEFAULT);
        assert!(ledger.exceeds_budget());
        let msg = ledger.budget_diagnostic();
        assert!(msg.contains("11 cell(s) skipped"), "{msg}");
        assert!(msg.contains("only 10 are declared expected"), "{msg}");
        assert!(msg.contains("not a passing cell"), "{msg}");
    }

    /// The macOS shape that motivated this: a broken fixture helper takes out
    /// most of the matrix. The diagnostic must name the dominant reason so the
    /// operator sees the cause, not just a number.
    #[test]
    fn the_macos_shape_is_over_budget_and_names_its_dominant_cause() {
        let mut outcomes: Vec<CheckOutcome> = (0..127)
            .map(|i| {
                outcome(
                    Status::Skip,
                    "verbosity",
                    &format!("cell {i}"),
                    "touch -d @epoch rejected by BSD touch",
                )
            })
            .collect();
        outcomes.extend((0..5).map(|i| outcome(Status::Pass, "dirs", &format!("p{i}"), "")));
        let ledger = SkipLedger::from_outcomes(&outcomes, ExpectedSkips::DEFAULT);

        assert_eq!(ledger.count(), 127, "passes must not be counted as skips");
        assert!(ledger.exceeds_budget());
        let msg = ledger.budget_diagnostic();
        assert!(msg.contains("127 cell(s) skipped"), "{msg}");
        assert!(msg.contains("BSD touch"), "{msg}");
    }

    /// Reasons are grouped so a large skip set stays readable, ordered by
    /// frequency so the dominant cause leads.
    #[test]
    fn reasons_group_by_frequency_then_alphabetically() {
        let mut outcomes = vec![outcome(Status::Skip, "a", "c1", "rare")];
        outcomes.extend((0..3).map(|i| outcome(Status::Skip, "b", &format!("c{i}"), "common")));
        outcomes.push(outcome(Status::Skip, "c", "c9", "also-rare"));
        let ledger = SkipLedger::from_outcomes(&outcomes, ExpectedSkips::DEFAULT);
        assert_eq!(
            ledger.by_reason(),
            vec![("common", 3), ("also-rare", 1), ("rare", 1)]
        );
    }

    /// A clean run still writes the artifact: an empty `whyskipped` says "we
    /// accounted for skips and there were none", which a missing file does not.
    #[test]
    fn artifact_is_written_even_when_nothing_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = SkipLedger::from_outcomes(&[], ExpectedSkips::DEFAULT);
        let path = ledger.write_artifact(dir.path()).unwrap();
        assert_eq!(path.file_name().unwrap(), WHYSKIPPED);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        assert!(!ledger.exceeds_budget());
        assert_eq!(ledger.summary(), "skipped: 0 (budget 10)");
    }

    /// The artifact is one record per skip, tab-separated, so it can be diffed
    /// and grepped by check the way upstream's `whyskipped` can be read.
    #[test]
    fn artifact_holds_one_tab_separated_record_per_skip() {
        let dir = tempfile::tempdir().unwrap();
        let outcomes = vec![
            outcome(Status::Skip, "acl", "local", "no acl support"),
            outcome(Status::Skip, "xattr", "daemon", "no xattr support"),
        ];
        let ledger = SkipLedger::from_outcomes(&outcomes, ExpectedSkips::DEFAULT);
        let path = ledger.write_artifact(dir.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            body,
            "acl\tlocal\tno acl support\nxattr\tdaemon\tno xattr support\n"
        );
    }

    /// The macOS shape a skip budget alone cannot see: cells that never became
    /// outcomes. 55 accounted of 239 declared must fail, and must say so in the
    /// language of vanished cells rather than of skips.
    #[test]
    fn vanished_cells_are_caught_by_the_floor_not_the_skip_budget() {
        // The BSD-touch run: 5 passed, 0 failed, 50 skipped = 55 accounted.
        let mut outcomes: Vec<CheckOutcome> = (0..5)
            .map(|i| outcome(Status::Pass, "dirs", &format!("p{i}"), ""))
            .collect();
        outcomes.extend(
            (0..50).map(|i| outcome(Status::Skip, "verbosity", &format!("s{i}"), "no sshd")),
        );

        // The skip budget alone does not catch this: 50 skips is over budget
        // here, but on a host with sshd the same collapse would leave far fewer
        // skips and slip through entirely.
        let msg = cell_shortfall(outcomes.len(), EXPECTED_CELLS, true).expect("must fail");
        assert!(msg.contains("55 cell(s)"), "{msg}");
        assert!(msg.contains("239"), "{msg}");
        assert!(msg.contains("184 cell(s) produced no outcome"), "{msg}");
        assert!(msg.contains("vanished"), "{msg}");
    }

    /// A full run that accounts for every cell clears the floor.
    #[test]
    fn a_complete_run_clears_the_cell_floor() {
        assert_eq!(cell_shortfall(EXPECTED_CELLS, EXPECTED_CELLS, true), None);
        assert_eq!(
            cell_shortfall(EXPECTED_CELLS + 12, EXPECTED_CELLS, true),
            None
        );
    }

    /// A narrowed run legitimately produces fewer cells, so the floor must not
    /// apply - otherwise `--transport local` could never pass.
    #[test]
    fn a_narrowed_run_is_exempt_from_the_cell_floor() {
        assert_eq!(cell_shortfall(3, EXPECTED_CELLS, false), None);
    }

    /// Passing and failing cells never enter the ledger.
    #[test]
    fn only_skipped_cells_are_collected() {
        let outcomes = vec![
            outcome(Status::Pass, "a", "c", ""),
            outcome(Status::Fail, "b", "c", "diverged"),
            outcome(Status::Skip, "c", "c", "why"),
        ];
        let ledger = SkipLedger::from_outcomes(&outcomes, ExpectedSkips::DEFAULT);
        assert_eq!(ledger.count(), 1);
        assert_eq!(ledger.artifact(), "c\tc\twhy\n");
    }
}
