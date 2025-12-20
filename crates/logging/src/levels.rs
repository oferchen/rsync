//! crates/logging/src/levels.rs
//! Flag enums and level structures for info and debug verbosity.

/// Info flags for diagnostic categories.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
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
