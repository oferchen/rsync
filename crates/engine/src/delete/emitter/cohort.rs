//! Cohort-aware delete record type.
//!
//! Emitted when a [`super::super::DeleteEntry`] carries a
//! [`super::super::HardlinkCohortId`] and a [`super::super::CohortIndex`]
//! is attached to the emitter, giving downstream itemize formatting the
//! information needed to tag a deletion with its leader cohort without
//! re-statting.

use std::path::PathBuf;

use super::super::DeleteEntryKind;
use super::super::HardlinkCohortId;

/// One cohort-aware delete dispatch record.
///
/// Produced by the emitter when a [`super::super::DeleteEntry`] carries
/// a [`HardlinkCohortId`] and a [`super::super::CohortIndex`] is
/// attached. The record pairs the destination path with the cohort tag
/// and the source-side ref count for the cohort at snapshot time,
/// giving callers enough information to format an upstream-style
/// itemize line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CohortDeleteRecord {
    /// Destination path the emitter dispatched against.
    pub path: PathBuf,
    /// Entry kind that drove the dispatch.
    pub kind: DeleteEntryKind,
    /// Cohort the entry belonged to (`None` if the destination was not
    /// part of any tracked cohort even though the index was attached).
    pub cohort: Option<HardlinkCohortId>,
    /// Source-side ref count for the cohort at snapshot time, or `0`
    /// when no cohort tag was present. Mirrors upstream's
    /// `match_hard_links` view of "how many links the upstream sent for
    /// this cohort".
    pub surviving_source_refs: u32,
}
