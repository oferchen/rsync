//! crates/logging/src/levels.rs
//! Flag enums and level structures for info and debug verbosity.

/// Info flags for diagnostic categories.
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

/// Debug flags for diagnostic categories.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DebugFlag {
    /// ACL processing.
    Acl,
    /// Backup file creation.
    Backup,
    /// Socket binding.
    Bind,
    /// Directory changes.
    Chdir,
    /// Connection establishment.
    Connect,
    /// Command execution.
    Cmd,
    /// Deletion operations.
    Del,
    /// Delta sum calculations.
    Deltasum,
    /// Duplicate detection.
    Dup,
    /// Exit status and cleanup.
    Exit,
    /// Filter rule processing.
    Filter,
    /// File list operations.
    Flist,
    /// Fuzzy basis file matching.
    Fuzzy,
    /// Generator operations.
    Genr,
    /// Hash calculations.
    Hash,
    /// Hard link detection.
    Hlink,
    /// Character encoding conversion.
    Iconv,
    /// I/O operations.
    Io,
    /// Namespace operations.
    Nstr,
    /// Ownership changes.
    Own,
    /// Protocol negotiation.
    Proto,
    /// Receiver operations.
    Recv,
    /// Sender operations.
    Send,
    /// Timing information.
    Time,
}

/// Info verbosity levels for each flag.
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
    pub fn get(&self, flag: InfoFlag) -> u8 {
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
    pub fn set(&mut self, flag: InfoFlag, level: u8) {
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
    pub fn set_all(&mut self, level: u8) {
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

/// Debug verbosity levels for each flag.
#[derive(Clone, Default, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DebugLevels {
    /// ACL processing level.
    pub acl: u8,
    /// Backup file creation level.
    pub backup: u8,
    /// Socket binding level.
    pub bind: u8,
    /// Directory changes level.
    pub chdir: u8,
    /// Connection establishment level.
    pub connect: u8,
    /// Command execution level.
    pub cmd: u8,
    /// Deletion operations level.
    pub del: u8,
    /// Delta sum calculations level.
    pub deltasum: u8,
    /// Duplicate detection level.
    pub dup: u8,
    /// Exit status level.
    pub exit: u8,
    /// Filter rule processing level.
    pub filter: u8,
    /// File list operations level.
    pub flist: u8,
    /// Fuzzy basis matching level.
    pub fuzzy: u8,
    /// Generator operations level.
    pub genr: u8,
    /// Hash calculations level.
    pub hash: u8,
    /// Hard link detection level.
    pub hlink: u8,
    /// Character encoding level.
    pub iconv: u8,
    /// I/O operations level.
    pub io: u8,
    /// Namespace operations level.
    pub nstr: u8,
    /// Ownership changes level.
    pub own: u8,
    /// Protocol negotiation level.
    pub proto: u8,
    /// Receiver operations level.
    pub recv: u8,
    /// Sender operations level.
    pub send: u8,
    /// Timing information level.
    pub time: u8,
}

impl DebugLevels {
    /// Get the level for a specific flag.
    pub fn get(&self, flag: DebugFlag) -> u8 {
        match flag {
            DebugFlag::Acl => self.acl,
            DebugFlag::Backup => self.backup,
            DebugFlag::Bind => self.bind,
            DebugFlag::Chdir => self.chdir,
            DebugFlag::Connect => self.connect,
            DebugFlag::Cmd => self.cmd,
            DebugFlag::Del => self.del,
            DebugFlag::Deltasum => self.deltasum,
            DebugFlag::Dup => self.dup,
            DebugFlag::Exit => self.exit,
            DebugFlag::Filter => self.filter,
            DebugFlag::Flist => self.flist,
            DebugFlag::Fuzzy => self.fuzzy,
            DebugFlag::Genr => self.genr,
            DebugFlag::Hash => self.hash,
            DebugFlag::Hlink => self.hlink,
            DebugFlag::Iconv => self.iconv,
            DebugFlag::Io => self.io,
            DebugFlag::Nstr => self.nstr,
            DebugFlag::Own => self.own,
            DebugFlag::Proto => self.proto,
            DebugFlag::Recv => self.recv,
            DebugFlag::Send => self.send,
            DebugFlag::Time => self.time,
        }
    }

    /// Set the level for a specific flag.
    pub fn set(&mut self, flag: DebugFlag, level: u8) {
        match flag {
            DebugFlag::Acl => self.acl = level,
            DebugFlag::Backup => self.backup = level,
            DebugFlag::Bind => self.bind = level,
            DebugFlag::Chdir => self.chdir = level,
            DebugFlag::Connect => self.connect = level,
            DebugFlag::Cmd => self.cmd = level,
            DebugFlag::Del => self.del = level,
            DebugFlag::Deltasum => self.deltasum = level,
            DebugFlag::Dup => self.dup = level,
            DebugFlag::Exit => self.exit = level,
            DebugFlag::Filter => self.filter = level,
            DebugFlag::Flist => self.flist = level,
            DebugFlag::Fuzzy => self.fuzzy = level,
            DebugFlag::Genr => self.genr = level,
            DebugFlag::Hash => self.hash = level,
            DebugFlag::Hlink => self.hlink = level,
            DebugFlag::Iconv => self.iconv = level,
            DebugFlag::Io => self.io = level,
            DebugFlag::Nstr => self.nstr = level,
            DebugFlag::Own => self.own = level,
            DebugFlag::Proto => self.proto = level,
            DebugFlag::Recv => self.recv = level,
            DebugFlag::Send => self.send = level,
            DebugFlag::Time => self.time = level,
        }
    }

    /// Set all flags to the specified level.
    pub fn set_all(&mut self, level: u8) {
        self.acl = level;
        self.backup = level;
        self.bind = level;
        self.chdir = level;
        self.connect = level;
        self.cmd = level;
        self.del = level;
        self.deltasum = level;
        self.dup = level;
        self.exit = level;
        self.filter = level;
        self.flist = level;
        self.fuzzy = level;
        self.genr = level;
        self.hash = level;
        self.hlink = level;
        self.iconv = level;
        self.io = level;
        self.nstr = level;
        self.own = level;
        self.proto = level;
        self.recv = level;
        self.send = level;
        self.time = level;
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

    mod debug_flag_tests {
        use super::*;

        #[test]
        fn debug_flag_clone_and_copy() {
            let flag = DebugFlag::Acl;
            let cloned = flag;
            let copied = flag;
            assert_eq!(flag, cloned);
            assert_eq!(flag, copied);
        }

        #[test]
        fn debug_flag_debug_format() {
            assert_eq!(format!("{:?}", DebugFlag::Acl), "Acl");
            assert_eq!(format!("{:?}", DebugFlag::Backup), "Backup");
            assert_eq!(format!("{:?}", DebugFlag::Bind), "Bind");
            assert_eq!(format!("{:?}", DebugFlag::Chdir), "Chdir");
            assert_eq!(format!("{:?}", DebugFlag::Connect), "Connect");
            assert_eq!(format!("{:?}", DebugFlag::Cmd), "Cmd");
            assert_eq!(format!("{:?}", DebugFlag::Del), "Del");
            assert_eq!(format!("{:?}", DebugFlag::Deltasum), "Deltasum");
            assert_eq!(format!("{:?}", DebugFlag::Dup), "Dup");
            assert_eq!(format!("{:?}", DebugFlag::Exit), "Exit");
            assert_eq!(format!("{:?}", DebugFlag::Filter), "Filter");
            assert_eq!(format!("{:?}", DebugFlag::Flist), "Flist");
            assert_eq!(format!("{:?}", DebugFlag::Fuzzy), "Fuzzy");
            assert_eq!(format!("{:?}", DebugFlag::Genr), "Genr");
            assert_eq!(format!("{:?}", DebugFlag::Hash), "Hash");
            assert_eq!(format!("{:?}", DebugFlag::Hlink), "Hlink");
            assert_eq!(format!("{:?}", DebugFlag::Iconv), "Iconv");
            assert_eq!(format!("{:?}", DebugFlag::Io), "Io");
            assert_eq!(format!("{:?}", DebugFlag::Nstr), "Nstr");
            assert_eq!(format!("{:?}", DebugFlag::Own), "Own");
            assert_eq!(format!("{:?}", DebugFlag::Proto), "Proto");
            assert_eq!(format!("{:?}", DebugFlag::Recv), "Recv");
            assert_eq!(format!("{:?}", DebugFlag::Send), "Send");
            assert_eq!(format!("{:?}", DebugFlag::Time), "Time");
        }

        #[test]
        fn debug_flag_equality() {
            assert_eq!(DebugFlag::Acl, DebugFlag::Acl);
            assert_ne!(DebugFlag::Acl, DebugFlag::Backup);
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

            let cloned = levels.clone();
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

    mod debug_levels_tests {
        use super::*;

        #[test]
        fn default_debug_levels_are_zero() {
            let levels = DebugLevels::default();
            assert_eq!(levels.acl, 0);
            assert_eq!(levels.backup, 0);
            assert_eq!(levels.bind, 0);
            assert_eq!(levels.chdir, 0);
            assert_eq!(levels.connect, 0);
            assert_eq!(levels.cmd, 0);
            assert_eq!(levels.del, 0);
            assert_eq!(levels.deltasum, 0);
            assert_eq!(levels.dup, 0);
            assert_eq!(levels.exit, 0);
            assert_eq!(levels.filter, 0);
            assert_eq!(levels.flist, 0);
            assert_eq!(levels.fuzzy, 0);
            assert_eq!(levels.genr, 0);
            assert_eq!(levels.hash, 0);
            assert_eq!(levels.hlink, 0);
            assert_eq!(levels.iconv, 0);
            assert_eq!(levels.io, 0);
            assert_eq!(levels.nstr, 0);
            assert_eq!(levels.own, 0);
            assert_eq!(levels.proto, 0);
            assert_eq!(levels.recv, 0);
            assert_eq!(levels.send, 0);
            assert_eq!(levels.time, 0);
        }

        #[test]
        fn get_returns_correct_level_for_each_flag() {
            let levels = DebugLevels {
                acl: 1,
                backup: 2,
                bind: 3,
                chdir: 4,
                connect: 5,
                cmd: 6,
                del: 7,
                deltasum: 8,
                dup: 9,
                exit: 10,
                filter: 11,
                flist: 12,
                fuzzy: 13,
                genr: 14,
                hash: 15,
                hlink: 16,
                iconv: 17,
                io: 18,
                nstr: 19,
                own: 20,
                proto: 21,
                recv: 22,
                send: 23,
                time: 24,
            };

            assert_eq!(levels.get(DebugFlag::Acl), 1);
            assert_eq!(levels.get(DebugFlag::Backup), 2);
            assert_eq!(levels.get(DebugFlag::Bind), 3);
            assert_eq!(levels.get(DebugFlag::Chdir), 4);
            assert_eq!(levels.get(DebugFlag::Connect), 5);
            assert_eq!(levels.get(DebugFlag::Cmd), 6);
            assert_eq!(levels.get(DebugFlag::Del), 7);
            assert_eq!(levels.get(DebugFlag::Deltasum), 8);
            assert_eq!(levels.get(DebugFlag::Dup), 9);
            assert_eq!(levels.get(DebugFlag::Exit), 10);
            assert_eq!(levels.get(DebugFlag::Filter), 11);
            assert_eq!(levels.get(DebugFlag::Flist), 12);
            assert_eq!(levels.get(DebugFlag::Fuzzy), 13);
            assert_eq!(levels.get(DebugFlag::Genr), 14);
            assert_eq!(levels.get(DebugFlag::Hash), 15);
            assert_eq!(levels.get(DebugFlag::Hlink), 16);
            assert_eq!(levels.get(DebugFlag::Iconv), 17);
            assert_eq!(levels.get(DebugFlag::Io), 18);
            assert_eq!(levels.get(DebugFlag::Nstr), 19);
            assert_eq!(levels.get(DebugFlag::Own), 20);
            assert_eq!(levels.get(DebugFlag::Proto), 21);
            assert_eq!(levels.get(DebugFlag::Recv), 22);
            assert_eq!(levels.get(DebugFlag::Send), 23);
            assert_eq!(levels.get(DebugFlag::Time), 24);
        }

        #[test]
        fn set_updates_correct_level_for_each_flag() {
            let mut levels = DebugLevels::default();

            levels.set(DebugFlag::Acl, 1);
            assert_eq!(levels.acl, 1);

            levels.set(DebugFlag::Backup, 2);
            assert_eq!(levels.backup, 2);

            levels.set(DebugFlag::Bind, 3);
            assert_eq!(levels.bind, 3);

            levels.set(DebugFlag::Chdir, 4);
            assert_eq!(levels.chdir, 4);

            levels.set(DebugFlag::Connect, 5);
            assert_eq!(levels.connect, 5);

            levels.set(DebugFlag::Cmd, 6);
            assert_eq!(levels.cmd, 6);

            levels.set(DebugFlag::Del, 7);
            assert_eq!(levels.del, 7);

            levels.set(DebugFlag::Deltasum, 8);
            assert_eq!(levels.deltasum, 8);

            levels.set(DebugFlag::Dup, 9);
            assert_eq!(levels.dup, 9);

            levels.set(DebugFlag::Exit, 10);
            assert_eq!(levels.exit, 10);

            levels.set(DebugFlag::Filter, 11);
            assert_eq!(levels.filter, 11);

            levels.set(DebugFlag::Flist, 12);
            assert_eq!(levels.flist, 12);

            levels.set(DebugFlag::Fuzzy, 13);
            assert_eq!(levels.fuzzy, 13);

            levels.set(DebugFlag::Genr, 14);
            assert_eq!(levels.genr, 14);

            levels.set(DebugFlag::Hash, 15);
            assert_eq!(levels.hash, 15);

            levels.set(DebugFlag::Hlink, 16);
            assert_eq!(levels.hlink, 16);

            levels.set(DebugFlag::Iconv, 17);
            assert_eq!(levels.iconv, 17);

            levels.set(DebugFlag::Io, 18);
            assert_eq!(levels.io, 18);

            levels.set(DebugFlag::Nstr, 19);
            assert_eq!(levels.nstr, 19);

            levels.set(DebugFlag::Own, 20);
            assert_eq!(levels.own, 20);

            levels.set(DebugFlag::Proto, 21);
            assert_eq!(levels.proto, 21);

            levels.set(DebugFlag::Recv, 22);
            assert_eq!(levels.recv, 22);

            levels.set(DebugFlag::Send, 23);
            assert_eq!(levels.send, 23);

            levels.set(DebugFlag::Time, 24);
            assert_eq!(levels.time, 24);
        }

        #[test]
        fn set_all_updates_all_levels() {
            let mut levels = DebugLevels::default();
            levels.set_all(7);

            assert_eq!(levels.acl, 7);
            assert_eq!(levels.backup, 7);
            assert_eq!(levels.bind, 7);
            assert_eq!(levels.chdir, 7);
            assert_eq!(levels.connect, 7);
            assert_eq!(levels.cmd, 7);
            assert_eq!(levels.del, 7);
            assert_eq!(levels.deltasum, 7);
            assert_eq!(levels.dup, 7);
            assert_eq!(levels.exit, 7);
            assert_eq!(levels.filter, 7);
            assert_eq!(levels.flist, 7);
            assert_eq!(levels.fuzzy, 7);
            assert_eq!(levels.genr, 7);
            assert_eq!(levels.hash, 7);
            assert_eq!(levels.hlink, 7);
            assert_eq!(levels.iconv, 7);
            assert_eq!(levels.io, 7);
            assert_eq!(levels.nstr, 7);
            assert_eq!(levels.own, 7);
            assert_eq!(levels.proto, 7);
            assert_eq!(levels.recv, 7);
            assert_eq!(levels.send, 7);
            assert_eq!(levels.time, 7);
        }

        #[test]
        fn debug_levels_clone() {
            let levels = DebugLevels {
                acl: 3,
                bind: 7,
                ..Default::default()
            };

            let cloned = levels.clone();
            assert_eq!(cloned.acl, 3);
            assert_eq!(cloned.bind, 7);
        }

        #[test]
        fn debug_levels_debug_format() {
            let levels = DebugLevels::default();
            let debug_str = format!("{levels:?}");
            assert!(debug_str.contains("DebugLevels"));
            assert!(debug_str.contains("acl"));
            assert!(debug_str.contains("bind"));
        }
    }
}
