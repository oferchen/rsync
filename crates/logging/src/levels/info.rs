//! Info verbosity flags and per-flag level storage.
//!
//! This module defines [`InfoFlag`] and [`InfoLevels`], mirroring upstream
//! rsync's `INFO_*` constants and `info_levels[]` array
//! (upstream: rsync.h, options.c:228).

/// Info flags for user-visible diagnostic categories.
///
/// These flags control output that end users see - file names, statistics,
/// skip/delete notifications. They are set by `-v` (level 1) and `-vv`
/// (level 2). Upstream defines these in `rsync.h` as `INFO_*` constants
/// and indexes into the `info_levels[]` array.
// upstream: rsync.h INFO_BACKUP..INFO_SYMSAFE, options.c:228 info_verbosity[]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum InfoFlag {
    /// Backup file operations.
    Backup,
    /// File copy operations.
    Copy,
    /// File deletion operations.
    Del,
    /// File list building and transmission.
    Flist,
    /// Miscellaneous operations.
    Misc,
    /// Mount point handling.
    Mount,
    /// File name processing.
    Name,
    /// Non-regular file handling.
    Nonreg,
    /// Progress reporting.
    Progress,
    /// File removal operations.
    Remove,
    /// Skipped files.
    Skip,
    /// Transfer statistics.
    Stats,
    /// Symlink safety checks.
    Symsafe,
}

/// Per-flag info verbosity levels.
///
/// Each field holds the current verbosity level for its corresponding
/// [`InfoFlag`]. A value of 0 means the flag is disabled. Upstream rsync
/// stores these in the global `info_levels[]` array (upstream: rsync.h).
// upstream: rsync.h info_levels[]
#[derive(Clone, Default, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct InfoLevels {
    /// Backup file operations level.
    pub backup: u8,
    /// File copy operations level.
    pub copy: u8,
    /// File deletion operations level.
    pub del: u8,
    /// File list building level.
    pub flist: u8,
    /// Miscellaneous operations level.
    pub misc: u8,
    /// Mount point handling level.
    pub mount: u8,
    /// File name processing level.
    pub name: u8,
    /// Non-regular file handling level.
    pub nonreg: u8,
    /// Progress reporting level.
    pub progress: u8,
    /// File removal operations level.
    pub remove: u8,
    /// Skipped files level.
    pub skip: u8,
    /// Transfer statistics level.
    pub stats: u8,
    /// Symlink safety checks level.
    pub symsafe: u8,
}

impl InfoLevels {
    /// Get the level for a specific flag.
    #[must_use]
    pub const fn get(&self, flag: InfoFlag) -> u8 {
        match flag {
            InfoFlag::Backup => self.backup,
            InfoFlag::Copy => self.copy,
            InfoFlag::Del => self.del,
            InfoFlag::Flist => self.flist,
            InfoFlag::Misc => self.misc,
            InfoFlag::Mount => self.mount,
            InfoFlag::Name => self.name,
            InfoFlag::Nonreg => self.nonreg,
            InfoFlag::Progress => self.progress,
            InfoFlag::Remove => self.remove,
            InfoFlag::Skip => self.skip,
            InfoFlag::Stats => self.stats,
            InfoFlag::Symsafe => self.symsafe,
        }
    }

    /// Set the level for a specific flag.
    pub const fn set(&mut self, flag: InfoFlag, level: u8) {
        match flag {
            InfoFlag::Backup => self.backup = level,
            InfoFlag::Copy => self.copy = level,
            InfoFlag::Del => self.del = level,
            InfoFlag::Flist => self.flist = level,
            InfoFlag::Misc => self.misc = level,
            InfoFlag::Mount => self.mount = level,
            InfoFlag::Name => self.name = level,
            InfoFlag::Nonreg => self.nonreg = level,
            InfoFlag::Progress => self.progress = level,
            InfoFlag::Remove => self.remove = level,
            InfoFlag::Skip => self.skip = level,
            InfoFlag::Stats => self.stats = level,
            InfoFlag::Symsafe => self.symsafe = level,
        }
    }

    /// Set all flags to the specified level.
    pub const fn set_all(&mut self, level: u8) {
        self.backup = level;
        self.copy = level;
        self.del = level;
        self.flist = level;
        self.misc = level;
        self.mount = level;
        self.name = level;
        self.nonreg = level;
        self.progress = level;
        self.remove = level;
        self.skip = level;
        self.stats = level;
        self.symsafe = level;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod info_flag_tests {
        use super::*;

        #[test]
        fn info_flag_clone_and_copy() {
            let flag = InfoFlag::Backup;
            let cloned = flag;
            let copied = flag;
            assert_eq!(flag, cloned);
            assert_eq!(flag, copied);
        }

        #[test]
        fn info_flag_debug_format() {
            assert_eq!(format!("{:?}", InfoFlag::Backup), "Backup");
            assert_eq!(format!("{:?}", InfoFlag::Copy), "Copy");
            assert_eq!(format!("{:?}", InfoFlag::Del), "Del");
            assert_eq!(format!("{:?}", InfoFlag::Flist), "Flist");
            assert_eq!(format!("{:?}", InfoFlag::Misc), "Misc");
            assert_eq!(format!("{:?}", InfoFlag::Mount), "Mount");
            assert_eq!(format!("{:?}", InfoFlag::Name), "Name");
            assert_eq!(format!("{:?}", InfoFlag::Nonreg), "Nonreg");
            assert_eq!(format!("{:?}", InfoFlag::Progress), "Progress");
            assert_eq!(format!("{:?}", InfoFlag::Remove), "Remove");
            assert_eq!(format!("{:?}", InfoFlag::Skip), "Skip");
            assert_eq!(format!("{:?}", InfoFlag::Stats), "Stats");
            assert_eq!(format!("{:?}", InfoFlag::Symsafe), "Symsafe");
        }

        #[test]
        fn info_flag_equality() {
            assert_eq!(InfoFlag::Backup, InfoFlag::Backup);
            assert_ne!(InfoFlag::Backup, InfoFlag::Copy);
        }
    }

    mod info_levels_tests {
        use super::*;

        #[test]
        fn default_info_levels_are_zero() {
            let levels = InfoLevels::default();
            assert_eq!(levels.backup, 0);
            assert_eq!(levels.copy, 0);
            assert_eq!(levels.del, 0);
            assert_eq!(levels.flist, 0);
            assert_eq!(levels.misc, 0);
            assert_eq!(levels.mount, 0);
            assert_eq!(levels.name, 0);
            assert_eq!(levels.nonreg, 0);
            assert_eq!(levels.progress, 0);
            assert_eq!(levels.remove, 0);
            assert_eq!(levels.skip, 0);
            assert_eq!(levels.stats, 0);
            assert_eq!(levels.symsafe, 0);
        }

        #[test]
        fn get_returns_correct_level_for_each_flag() {
            let levels = InfoLevels {
                backup: 1,
                copy: 2,
                del: 3,
                flist: 4,
                misc: 5,
                mount: 6,
                name: 7,
                nonreg: 8,
                progress: 9,
                remove: 10,
                skip: 11,
                stats: 12,
                symsafe: 13,
            };

            assert_eq!(levels.get(InfoFlag::Backup), 1);
            assert_eq!(levels.get(InfoFlag::Copy), 2);
            assert_eq!(levels.get(InfoFlag::Del), 3);
            assert_eq!(levels.get(InfoFlag::Flist), 4);
            assert_eq!(levels.get(InfoFlag::Misc), 5);
            assert_eq!(levels.get(InfoFlag::Mount), 6);
            assert_eq!(levels.get(InfoFlag::Name), 7);
            assert_eq!(levels.get(InfoFlag::Nonreg), 8);
            assert_eq!(levels.get(InfoFlag::Progress), 9);
            assert_eq!(levels.get(InfoFlag::Remove), 10);
            assert_eq!(levels.get(InfoFlag::Skip), 11);
            assert_eq!(levels.get(InfoFlag::Stats), 12);
            assert_eq!(levels.get(InfoFlag::Symsafe), 13);
        }

        #[test]
        fn set_updates_correct_level_for_each_flag() {
            let mut levels = InfoLevels::default();

            levels.set(InfoFlag::Backup, 1);
            assert_eq!(levels.backup, 1);

            levels.set(InfoFlag::Copy, 2);
            assert_eq!(levels.copy, 2);

            levels.set(InfoFlag::Del, 3);
            assert_eq!(levels.del, 3);

            levels.set(InfoFlag::Flist, 4);
            assert_eq!(levels.flist, 4);

            levels.set(InfoFlag::Misc, 5);
            assert_eq!(levels.misc, 5);

            levels.set(InfoFlag::Mount, 6);
            assert_eq!(levels.mount, 6);

            levels.set(InfoFlag::Name, 7);
            assert_eq!(levels.name, 7);

            levels.set(InfoFlag::Nonreg, 8);
            assert_eq!(levels.nonreg, 8);

            levels.set(InfoFlag::Progress, 9);
            assert_eq!(levels.progress, 9);

            levels.set(InfoFlag::Remove, 10);
            assert_eq!(levels.remove, 10);

            levels.set(InfoFlag::Skip, 11);
            assert_eq!(levels.skip, 11);

            levels.set(InfoFlag::Stats, 12);
            assert_eq!(levels.stats, 12);

            levels.set(InfoFlag::Symsafe, 13);
            assert_eq!(levels.symsafe, 13);
        }

        #[test]
        fn set_all_updates_all_levels() {
            let mut levels = InfoLevels::default();
            levels.set_all(5);

            assert_eq!(levels.backup, 5);
            assert_eq!(levels.copy, 5);
            assert_eq!(levels.del, 5);
            assert_eq!(levels.flist, 5);
            assert_eq!(levels.misc, 5);
            assert_eq!(levels.mount, 5);
            assert_eq!(levels.name, 5);
            assert_eq!(levels.nonreg, 5);
            assert_eq!(levels.progress, 5);
            assert_eq!(levels.remove, 5);
            assert_eq!(levels.skip, 5);
            assert_eq!(levels.stats, 5);
            assert_eq!(levels.symsafe, 5);
        }

        #[test]
        fn info_levels_clone() {
            let levels = InfoLevels {
                backup: 3,
                copy: 7,
                ..Default::default()
            };

            let cloned = levels;
            assert_eq!(cloned.backup, 3);
            assert_eq!(cloned.copy, 7);
        }

        #[test]
        fn info_levels_debug_format() {
            let levels = InfoLevels::default();
            let debug_str = format!("{levels:?}");
            assert!(debug_str.contains("InfoLevels"));
            assert!(debug_str.contains("backup"));
            assert!(debug_str.contains("copy"));
        }
    }
}
