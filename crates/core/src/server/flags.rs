#![deny(unsafe_code)]
//! Parser for the compact server flag string.
//!
//! When rsync invokes a remote server, it encodes the relevant transfer options
//! into a compact single-letter flag string like `-logDtpre.iLsfxC`. This module
//! decodes those flags into structured options.

use thiserror::Error;

/// Parsed server options decoded from the compact flag string.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct ParsedServerFlags {
    /// Preserve symbolic links (`l` flag, `--links`).
    pub links: bool,
    /// Preserve owner (`o` flag, `--owner`).
    pub owner: bool,
    /// Preserve group (`g` flag, `--group`).
    pub group: bool,
    /// Preserve device files (`D` flag, `--devices`).
    pub devices: bool,
    /// Preserve special files (included in `D` flag, `--specials`).
    pub specials: bool,
    /// Preserve modification times (`t` flag, `--times`).
    pub times: bool,
    /// Preserve permissions (`p` flag, `--perms`).
    pub perms: bool,
    /// Recursive transfer (`r` flag, `--recursive`).
    pub recursive: bool,
    /// Remote shell specified (`e` flag, `--rsh`).
    pub rsh: bool,
    /// Archive mode shorthand (`a` flag, `--archive`).
    pub archive: bool,
    /// Verbose output (`v` flag, `--verbose`).
    pub verbose: bool,
    /// Compress during transfer (`z` flag, `--compress`).
    pub compress: bool,
    /// Checksum-based transfer (`c` flag, `--checksum`).
    pub checksum: bool,
    /// Preserve hard links (`H` flag, `--hard-links`).
    pub hard_links: bool,
    /// Preserve ACLs (`A` flag, `--acls`).
    pub acls: bool,
    /// Preserve extended attributes (`X` flag, `--xattrs`).
    pub xattrs: bool,
    /// Numeric IDs only (`n` flag, but this conflicts with dry-run).
    pub numeric_ids: bool,
    /// Delete extraneous files (`d` flag for delete).
    pub delete: bool,
    /// Whole file transfer, no delta (`W` flag, `--whole-file`).
    pub whole_file: bool,
    /// Sparse file handling (`S` flag, `--sparse`).
    pub sparse: bool,
    /// One file system only (`x` flag, `--one-file-system`).
    pub one_file_system: bool,
    /// Relative paths (`R` flag, `--relative`).
    pub relative: bool,
    /// Keep partially transferred files (`P` flag, `--partial`).
    pub partial: bool,
    /// Update only newer files (`u` flag, `--update`).
    pub update: bool,
    /// Fuzzy basis file matching (`y` flag, `--fuzzy`).
    pub fuzzy: bool,

    /// Info flags after the first `.` separator.
    pub info_flags: InfoFlags,
}

/// Info/debug flags parsed from the suffix after `.`.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct InfoFlags {
    /// Itemize changes (`i` info flag).
    pub itemize: bool,
    /// Log format active (`L` info flag).
    pub log_format: bool,
    /// Statistics enabled (`s` info flag).
    pub stats: bool,
    /// File list debugging (`f` info flag).
    pub flist: bool,
    /// Checksum debugging (`x` info flag).
    pub checksum: bool,
    /// Compression debugging (`C` info flag).
    pub compress: bool,
}

impl ParsedServerFlags {
    /// Parses a compact flag string like `-logDtpre.iLsfxC`.
    ///
    /// Returns an error if the string doesn't start with `-` or contains
    /// invalid characters.
    pub fn parse(flag_string: &str) -> Result<Self, ParseFlagError> {
        let bytes = flag_string.as_bytes();
        if bytes.is_empty() || bytes[0] != b'-' {
            return Err(ParseFlagError::MissingLeadingDash);
        }

        let mut flags = ParsedServerFlags::default();
        let mut in_info_section = false;

        for &byte in &bytes[1..] {
            if byte == b'.' {
                in_info_section = true;
                continue;
            }

            if in_info_section {
                flags.info_flags.parse_info_flag(byte);
            } else {
                flags.parse_transfer_flag(byte);
            }
        }

        // Archive mode implies several flags
        if flags.archive {
            flags.recursive = true;
            flags.links = true;
            flags.perms = true;
            flags.times = true;
            flags.group = true;
            flags.owner = true;
            flags.devices = true;
            flags.specials = true;
        }

        Ok(flags)
    }

    fn parse_transfer_flag(&mut self, byte: u8) {
        match byte {
            b'l' => self.links = true,
            b'o' => self.owner = true,
            b'g' => self.group = true,
            b'D' => {
                self.devices = true;
                self.specials = true;
            }
            b't' => self.times = true,
            b'p' => self.perms = true,
            b'r' => self.recursive = true,
            b'e' => self.rsh = true,
            b'a' => self.archive = true,
            b'v' => self.verbose = true,
            b'z' => self.compress = true,
            b'c' => self.checksum = true,
            b'H' => self.hard_links = true,
            b'A' => self.acls = true,
            b'X' => self.xattrs = true,
            b'n' => self.numeric_ids = true,
            b'd' => self.delete = true,
            b'W' => self.whole_file = true,
            b'S' => self.sparse = true,
            b'x' => self.one_file_system = true,
            b'R' => self.relative = true,
            b'P' => self.partial = true,
            b'u' => self.update = true,
            b'y' => self.fuzzy = true,
            // Unknown flags are ignored to maintain forward compatibility
            _ => {}
        }
    }
}

impl InfoFlags {
    fn parse_info_flag(&mut self, byte: u8) {
        match byte {
            b'i' => self.itemize = true,
            b'L' => self.log_format = true,
            b's' => self.stats = true,
            b'f' => self.flist = true,
            b'x' => self.checksum = true,
            b'C' => self.compress = true,
            // Unknown info flags are ignored
            _ => {}
        }
    }
}

/// Error returned when parsing a flag string fails.
#[derive(Debug, Clone, Eq, PartialEq, Error)]
pub enum ParseFlagError {
    /// The flag string did not start with a leading `-`.
    #[error("flag string must start with '-'")]
    MissingLeadingDash,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_rsync_flag_string() {
        let flags = ParsedServerFlags::parse("-logDtpre.iLsfxC").unwrap();

        // Transfer flags
        assert!(flags.links);
        assert!(flags.owner);
        assert!(flags.group);
        assert!(flags.devices);
        assert!(flags.specials);
        assert!(flags.times);
        assert!(flags.perms);
        assert!(flags.recursive);
        assert!(flags.rsh);

        // Info flags
        assert!(flags.info_flags.itemize);
        assert!(flags.info_flags.log_format);
        assert!(flags.info_flags.stats);
        assert!(flags.info_flags.flist);
        assert!(flags.info_flags.checksum);
        assert!(flags.info_flags.compress);
    }

    #[test]
    fn parses_archive_mode() {
        let flags = ParsedServerFlags::parse("-av").unwrap();

        assert!(flags.archive);
        assert!(flags.verbose);
        // Archive implies these
        assert!(flags.recursive);
        assert!(flags.links);
        assert!(flags.perms);
        assert!(flags.times);
        assert!(flags.group);
        assert!(flags.owner);
        assert!(flags.devices);
        assert!(flags.specials);
    }

    #[test]
    fn parses_minimal_flags() {
        let flags = ParsedServerFlags::parse("-r").unwrap();
        assert!(flags.recursive);
        assert!(!flags.links);
        assert!(!flags.verbose);
    }

    #[test]
    fn parses_empty_info_section() {
        let flags = ParsedServerFlags::parse("-logDtpre.").unwrap();
        assert!(flags.links);
        assert!(!flags.info_flags.itemize);
    }

    #[test]
    fn rejects_missing_leading_dash() {
        let result = ParsedServerFlags::parse("logDtpre");
        assert_eq!(result.unwrap_err(), ParseFlagError::MissingLeadingDash);
    }

    #[test]
    fn ignores_unknown_flags() {
        // Unknown flags like 'Q' and 'Z' (uppercase) in transfer section
        let flags = ParsedServerFlags::parse("-rQZv").unwrap();
        assert!(flags.recursive);
        assert!(flags.verbose);
    }

    #[test]
    fn parses_checksum_and_compress() {
        let flags = ParsedServerFlags::parse("-cz").unwrap();
        assert!(flags.checksum);
        assert!(flags.compress);
    }

    #[test]
    fn parses_extended_attributes_and_acls() {
        let flags = ParsedServerFlags::parse("-AX").unwrap();
        assert!(flags.acls);
        assert!(flags.xattrs);
    }

    #[test]
    fn parses_fuzzy_flag() {
        let flags = ParsedServerFlags::parse("-ry").unwrap();
        assert!(flags.recursive);
        assert!(flags.fuzzy);
    }
}
