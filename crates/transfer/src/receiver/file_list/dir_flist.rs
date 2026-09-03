//! The receiver's second, independent directory numbering.
//!
//! An INC_RECURSE sub-list header frames itself by `dir_ndx`, which indexes
//! `dir_flist` - NOT the transfer file list. The two numberings differ, so
//! conflating them silently re-points a sub-list at the wrong parent.

use std::path::{Path, PathBuf};

use protocol::flist::{FileEntry, compare_file_entries};

/// What a wire `dir_ndx` resolves to, mirroring upstream's three outcomes at
/// `flist.c:2906-2918`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::receiver) enum DirSlot {
    /// A live directory; a sub-list may name it as its parent.
    Active(PathBuf),
    /// The slot exists but `flist_sort_and_clean()` cleared its entry. Upstream
    /// refuses this rather than dereferencing the zeroed struct.
    Cleared,
}

/// Upstream's receiver-side `dir_flist`, expressed over owned entries.
///
/// # Why a cleared directory must keep its slot
///
/// Upstream appends every directory to `dir_flist` **in the read loop**
/// (`flist.c:2993-2999`), sorts just that appended range (`flist.c:3049`), and
/// only *then* cleans the transfer list (`flist.c:3065`). Each slot holds a
/// **pointer** to the same `file_struct` as the transfer list, so when the
/// clean `clear_file()`s a duplicate directory the shared struct is zeroed and
/// the slot survives, now **inactive**.
///
/// Both halves are load-bearing:
///
/// - **Retaining the slot** keeps every later `dir_ndx` aligned. A dense
///   numbering shifts each subsequent directory down by one, so a `dir_ndx`
///   upstream refuses as cleared resolves to a different, live directory here
///   and the sub-list is accepted under the wrong parent.
/// - **The inactive marker** is what `flist.c:2911-2918` refuses, because
///   `f_name()` on a cleared entry returns NULL into the dirname comparison.
///
/// # Why the slot is not a flat index into `file_list`
///
/// That would be the literal transcription of upstream's pointer, and it is
/// unsafe here. `sorted` is an **alias** of `files` in the default arm
/// (`flist.c:2460`, `:3046`), so upstream's sort is in-place too and its
/// pointers simply follow the struct - oc's owned entries move instead, and an
/// index recorded before the sort would address a different entry after it.
/// The separate `sorted[]` array exists only under `need_unsorted_flist`
/// (`--iconv`), for the reason `flist.c:3026-3030` gives.
///
/// So the slot records a **name**, captured in two phases that each read real
/// state rather than re-deriving the clean's tie-break:
/// [`record_pre_clean`](Self::record_pre_clean) then
/// [`resolve_survivors`](Self::resolve_survivors).
#[derive(Debug, Default, Clone)]
pub(in crate::receiver) struct DirFlist {
    slots: Vec<DirSlot>,
}

/// The directories of one just-read list, in the order upstream numbers them.
///
/// Held between the two phases so the caller cannot accidentally record without
/// resolving, which would leave every slot of that range provisionally cleared.
#[derive(Debug)]
pub(in crate::receiver) struct PendingDirs {
    /// Sorted directory names, duplicates adjacent.
    names: Vec<PathBuf>,
}

impl DirFlist {
    /// Upstream's `dir_flist->used`: the number of slots, cleared ones included.
    pub(in crate::receiver) fn used(&self) -> usize {
        self.slots.len()
    }

    /// Phase 1 - snapshot the directories of `file_list[from..]` **before** the
    /// sort/clean pass, in the sorted order upstream numbers them.
    ///
    /// This must run before the clean: afterwards a tombstoned directory has a
    /// zeroed mode, so `is_dir()` is false and the entry is indistinguishable
    /// from a tombstoned regular file - which is exactly the dense-numbering
    /// defect this type exists to prevent.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2993-2999` - the read-loop append.
    /// - `flist.c:3049` - `fsort(dir_flist->sorted + dstart, ...)` orders just
    ///   the newly appended range.
    pub(in crate::receiver) fn record_pre_clean(
        file_list: &[FileEntry],
        from: usize,
    ) -> PendingDirs {
        let mut dirs: Vec<&FileEntry> = file_list[from..].iter().filter(|e| e.is_dir()).collect();
        dirs.sort_by(|a, b| compare_file_entries(a, b));
        PendingDirs {
            names: dirs.into_iter().map(|e| e.path().clone()).collect(),
        }
    }

    /// Phase 2 - append the recorded directories, marking the ones the clean
    /// tombstoned.
    ///
    /// `post_clean` is the same range after `sort_and_clean_file_list`. Survival
    /// is read from it rather than re-derived, so this cannot drift from the
    /// clean's own duplicate tie-break (a directory outranks a same-named
    /// regular file, `flist.c:3355-3360`). Duplicates are adjacent after the
    /// phase-1 sort and the clean keeps at most one, so the first occurrence of
    /// a name claims the survivor and every later occurrence is a cleared slot.
    pub(in crate::receiver) fn append(&mut self, pending: PendingDirs, post_clean: &[FileEntry]) {
        let mut previous: Option<&Path> = None;
        for name in &pending.names {
            let is_repeat = previous == Some(name.as_path());
            previous = Some(name.as_path());
            let survives = !is_repeat
                && post_clean
                    .iter()
                    .any(|e| e.is_active() && e.is_dir() && e.path() == name);
            self.slots.push(if survives {
                DirSlot::Active(name.clone())
            } else {
                DirSlot::Cleared
            });
        }
    }

    /// Seeds active slots for a list the test did not actually receive, standing
    /// in for an initial flist that already carried those parent directories.
    #[cfg(test)]
    pub(in crate::receiver) fn with_active<I, P>(names: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        Self {
            slots: names
                .into_iter()
                .map(|n| DirSlot::Active(n.into()))
                .collect(),
        }
    }

    /// Resolves a wire `dir_ndx`. `None` is upstream's `dir_ndx >=
    /// dir_flist->used` refusal (`flist.c:2906-2909`); `Some(Cleared)` is the
    /// inactive-slot refusal (`flist.c:2910-2918`).
    pub(in crate::receiver) fn resolve(&self, dir_ndx: i32) -> Option<&DirSlot> {
        usize::try_from(dir_ndx)
            .ok()
            .and_then(|n| self.slots.get(n))
    }
}
