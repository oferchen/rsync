//! How a local `--write-batch` builds its file lists, decided once per run.
//!
//! Upstream's sender streams an incremental file list: the operand's top
//! level first, then one sub-list per directory as it reads it. Some shapes
//! need the whole list, or a whole level of it, before the first sub-list can
//! go out; [`batch_list_mode`] names which.

use super::LocalCopyOptions;

/// Why the whole file list is built before any transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ListFirstReason {
    /// The batch records no `CF_INC_RECURSE`: one flat list is sent before any
    /// file data.
    ///
    /// upstream: compat.c:173-180 set_allow_inc_recurse() clears it for
    /// `--no-inc-recursive`, `--qsort`, a receiver deleting before or after
    /// the transfer, `--delay-updates` and `--prune-empty-dirs`; the reasons
    /// are decided where the batch header is built.
    IncRecurseOff,
    /// Several source operands: their top levels together form the initial
    /// list.
    ///
    /// upstream: flist.c:send_file_list() - the argv loop sends every
    /// operand's top-level entry before send_extra_file_list() sends the
    /// first sub-list.
    MultipleOperands,
}

/// Why the lists are planned only once the whole walk is over, although
/// upstream streams them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WholeRunReason {
    /// `--relative` (and `--files-from`, which implies it) lists implied
    /// parent directories that the per-directory planner cannot place yet.
    ///
    /// upstream: flist.c:send_implied_dirs() puts the implied directories in
    /// the initial list and replays the deeper levels per directory.
    RelativeImpliedDirs,
    /// `-H`: a group's data goes under its sorted-last member, which the
    /// walk knows only once the whole group is listed.
    ///
    /// upstream: hlink.c:113-208 match_gnums()
    HardLinkGroups,
}

/// How the batch file lists are built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BatchListMode {
    /// Planned one directory scan at a time, as the walk reads each directory.
    ///
    /// upstream: flist.c:send_directory() and send_extra_file_list()
    Stream,
    /// The whole list is known before any transfer.
    ListFirst(ListFirstReason),
    /// Planned once the walk is over.
    WholeRun(WholeRunReason),
}

/// Classifies a run that writes a batch with `operands` source operands.
pub(crate) fn batch_list_mode(options: &LocalCopyOptions, operands: usize) -> BatchListMode {
    let inc_recurse = options.get_batch_writer().is_some_and(|writer| {
        writer
            .lock()
            .expect("batch writer mutex poisoned")
            .config()
            .compat_flags
            .is_some_and(|cf| {
                protocol::CompatibilityFlags::from_bits(cf as u32)
                    .contains(protocol::CompatibilityFlags::INC_RECURSE)
            })
    });
    classify(
        inc_recurse,
        operands,
        options.relative_paths_enabled(),
        options.hard_links_enabled(),
    )
}

const fn classify(
    inc_recurse: bool,
    operands: usize,
    relative: bool,
    hard_links: bool,
) -> BatchListMode {
    if !inc_recurse {
        BatchListMode::ListFirst(ListFirstReason::IncRecurseOff)
    } else if operands > 1 {
        BatchListMode::ListFirst(ListFirstReason::MultipleOperands)
    } else if relative {
        BatchListMode::WholeRun(WholeRunReason::RelativeImpliedDirs)
    } else if hard_links {
        BatchListMode::WholeRun(WholeRunReason::HardLinkGroups)
    } else {
        BatchListMode::Stream
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat batch carries one list ahead of the data, whatever the operands.
    #[test]
    fn a_batch_without_inc_recurse_lists_first() {
        for (operands, relative) in [(1, false), (2, false), (1, true)] {
            assert_eq!(
                classify(false, operands, relative, false),
                BatchListMode::ListFirst(ListFirstReason::IncRecurseOff)
            );
        }
    }

    /// Several operands share the initial list, so it is complete only when
    /// every operand's top level is known.
    #[test]
    fn several_operands_list_first() {
        for relative in [false, true] {
            assert_eq!(
                classify(true, 2, relative, false),
                BatchListMode::ListFirst(ListFirstReason::MultipleOperands)
            );
        }
    }

    /// `--relative` streams in upstream, but its implied directories wait for
    /// the whole run here.
    #[test]
    fn a_relative_operand_waits_for_the_whole_run() {
        assert_eq!(
            classify(true, 1, true, false),
            BatchListMode::WholeRun(WholeRunReason::RelativeImpliedDirs)
        );
    }

    /// One operand, with or without a trailing slash (the slash changes only
    /// whether `.` heads the initial list), streams.
    #[test]
    fn a_single_operand_streams() {
        assert_eq!(classify(true, 1, false, false), BatchListMode::Stream);
    }

    /// `-H` waits for the whole run: a group's data goes under its last
    /// member.
    #[test]
    fn hard_links_wait_for_the_whole_run() {
        assert_eq!(
            classify(true, 1, false, true),
            BatchListMode::WholeRun(WholeRunReason::HardLinkGroups)
        );
    }

    /// Classified from the options' batch header: a writer that records
    /// `CF_INC_RECURSE` streams, and no batch writer reads as no bit.
    #[test]
    fn the_mode_follows_the_batch_header() {
        let options = LocalCopyOptions::new();
        assert_eq!(
            batch_list_mode(&options, 1),
            BatchListMode::ListFirst(ListFirstReason::IncRecurseOff)
        );
    }
}
