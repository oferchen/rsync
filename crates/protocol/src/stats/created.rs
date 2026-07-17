//! Client-local "created files" statistics accumulator.
//!
//! Unlike [`DeleteStats`](super::DeleteStats), these counters are NEVER sent
//! over the wire. Upstream rsync reconstructs the "Number of created files"
//! breakdown entirely on the client from its own itemize pass: every entry
//! whose iflags carry `ITEM_IS_NEW` bumps `stats.created_files` plus the
//! per-type counter for the entry's mode. A client sender does this in
//! `sender.c:295-308`; a client receiver in `receiver.c:733-746`. Only
//! `total_read`/`total_written`/`total_size` (and the `--delete` counters)
//! cross the wire (main.c `handle_stats`), so the client must tally these
//! itself while it processes the per-file `NDX + iflags` stream.

use crate::flist::FileType;

/// Per-type tally of entries created at the destination (destination absent
/// before the transfer), reconstructed on the client from the `ITEM_IS_NEW`
/// itemize flags.
///
/// `files` is the running total across all types; `regular()` derives the
/// implicit regular-file count exactly as upstream's `output_itemized_counts`
/// does (`counts[0] -= counts[1..4]`). Feeds the `--stats`
/// `Number of created files: N (reg: X, dir: Y, link: Z, dev: W, special: V)`
/// line.
///
/// # Upstream Reference
///
/// - `receiver.c:733-746` / `sender.c:295-308` - `stats.created_*++` under the
///   `iflags & ITEM_IS_NEW` guard, keyed by the entry's mode.
/// - `main.c:387-416` - `output_itemized_counts()` renders the breakdown.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CreatedStats {
    /// Total number of entries created, across every type. Upstream's
    /// `stats.created_files`.
    pub files: u64,
    /// Number of directories created. Upstream's `stats.created_dirs`.
    pub dirs: u64,
    /// Number of symbolic links created. Upstream's `stats.created_symlinks`.
    pub symlinks: u64,
    /// Number of device nodes created. Upstream's `stats.created_devices`.
    pub devices: u64,
    /// Number of special files (FIFOs, sockets) created. Upstream's
    /// `stats.created_specials`.
    pub specials: u64,
}

impl CreatedStats {
    /// Creates an empty accumulator.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            files: 0,
            dirs: 0,
            symlinks: 0,
            devices: 0,
            specials: 0,
        }
    }

    /// Records one newly created entry, classifying it by its Unix `mode` bits.
    ///
    /// Bumps the running total (`files`) and the per-type counter, mirroring
    /// upstream's mode dispatch exactly: regular files add only to the total
    /// (`reg` is derived), directories to `dirs`, symlinks to `symlinks`, block
    /// and character devices to `devices`, and everything else (FIFOs, sockets,
    /// and unrecognised modes) to `specials`.
    ///
    /// Call once per `ITEM_IS_NEW` entry, whether or not any file data moved -
    /// a new empty file, directory, symlink, device, or FIFO all count.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:733-746` / `sender.c:296-309` - the `S_ISREG` / `S_ISDIR`
    ///   / `S_ISLNK` / `IS_DEVICE` / else cascade.
    pub const fn record(&mut self, mode: u32) {
        self.files = self.files.saturating_add(1);
        match FileType::from_mode(mode) {
            Some(FileType::Regular) => {}
            Some(FileType::Directory) => self.dirs = self.dirs.saturating_add(1),
            Some(FileType::Symlink) => self.symlinks = self.symlinks.saturating_add(1),
            Some(FileType::BlockDevice | FileType::CharDevice) => {
                self.devices = self.devices.saturating_add(1);
            }
            // upstream: the final `else` covers FIFOs, sockets, and any mode the
            // type mask does not recognise, all of which count as specials.
            Some(FileType::Fifo | FileType::Socket) | None => {
                self.specials = self.specials.saturating_add(1);
            }
        }
    }

    /// Returns the derived count of newly created regular files.
    ///
    /// Regular files are not tracked directly; upstream derives the `reg`
    /// sub-count as `total - (dirs + symlinks + devices + specials)`
    /// (`output_itemized_counts`). Mirrors that so a stale flat count can never
    /// disagree with the typed sub-counts.
    #[must_use]
    pub const fn regular(&self) -> u64 {
        self.files
            .saturating_sub(self.dirs)
            .saturating_sub(self.symlinks)
            .saturating_sub(self.devices)
            .saturating_sub(self.specials)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mode constants mirroring the S_IF* bits classified by `record`.
    const S_IFREG: u32 = 0o100000;
    const S_IFDIR: u32 = 0o040000;
    const S_IFLNK: u32 = 0o120000;
    const S_IFBLK: u32 = 0o060000;
    const S_IFCHR: u32 = 0o020000;
    const S_IFIFO: u32 = 0o010000;
    const S_IFSOCK: u32 = 0o140000;

    #[test]
    fn record_classifies_each_type_like_upstream() {
        let mut stats = CreatedStats::new();
        stats.record(S_IFREG | 0o644);
        stats.record(S_IFDIR | 0o755);
        stats.record(S_IFLNK | 0o777);
        stats.record(S_IFBLK | 0o600);
        stats.record(S_IFCHR | 0o600);
        stats.record(S_IFIFO | 0o644);
        stats.record(S_IFSOCK | 0o644);

        assert_eq!(stats.files, 7);
        assert_eq!(stats.dirs, 1);
        assert_eq!(stats.symlinks, 1);
        // Both block and char devices fold into `devices`, matching IS_DEVICE.
        assert_eq!(stats.devices, 2);
        // FIFO + socket both count as specials, matching upstream's else branch.
        assert_eq!(stats.specials, 2);
        // reg is derived: 7 total - (1 dir + 1 link + 2 dev + 2 special) = 1.
        assert_eq!(stats.regular(), 1);
    }

    #[test]
    fn record_unrecognised_mode_counts_as_special() {
        // An all-zero type mask does not match any S_IF* pattern; upstream's
        // final `else` treats it as a special, so `record` must too.
        let mut stats = CreatedStats::new();
        stats.record(0);
        assert_eq!(stats.files, 1);
        assert_eq!(stats.specials, 1);
        assert_eq!(stats.regular(), 0);
    }
}
