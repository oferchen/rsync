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
    /// Preserve access times (`U` flag, `--atimes`).
    pub atimes: bool,
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
    /// Numeric IDs only (long-form `--numeric-ids`).
    ///
    /// Not part of the compact flag string; set via long-form args or explicit
    /// propagation. In upstream, `'n'` means dry-run, not numeric-ids.
    pub numeric_ids: bool,
    /// Delete extraneous files (long-form `--delete-*` variants).
    ///
    /// Not part of the compact flag string; set via long-form args or explicit
    /// propagation. In upstream, `'d'` means `--dirs`, not delete.
    pub delete: bool,
    /// Dry-run / no-transfer mode (`n` flag, upstream: `!do_xfers`).
    pub dry_run: bool,
    /// Transfer directories without recursion (`d` flag, `--dirs`).
    pub dirs: bool,
    /// Whole file transfer, no delta (`W` flag, `--whole-file`).
    pub whole_file: bool,
    /// Sparse file handling (`S` flag, `--sparse`).
    pub sparse: bool,
    /// One file system level (`x` flag count, `--one-file-system`).
    /// 0 = off, 1 = single -x, 2 = double -xx.
    pub one_file_system: u8,
    /// Relative paths (`R` flag, `--relative`).
    pub relative: bool,
    /// Keep partially transferred files (`P` flag, `--partial`).
    pub partial: bool,
    /// Update only newer files (`u` flag, `--update`).
    pub update: bool,
    /// Preserve creation times (`N` flag, `--crtimes`).
    pub crtimes: bool,
    /// Ignore modification times for quick-check (`I` flag, `--ignore-times`).
    pub ignore_times: bool,
    /// Copy symlinks as the referent file/dir (`L` flag, `--copy-links`).
    pub copy_links: bool,
    /// Copy unsafe symlinks as files (long-form `--copy-unsafe-links`).
    ///
    /// Not part of the compact flag string; set via long-form args.
    pub copy_unsafe_links: bool,
    /// Ignore symlinks that point outside the source tree (long-form `--safe-links`).
    ///
    /// Not part of the compact flag string; set via long-form args.
    pub safe_links: bool,
    /// Append data onto shorter files (long-form `--append`).
    ///
    /// Not part of the compact flag string; set via long-form args.
    pub append: bool,
    /// Make backups before overwriting (`b` flag, `--backup`).
    ///
    /// Upstream: `options.c:2613` - `argstr[x++] = 'b'`.
    pub backup: bool,
    /// Fuzzy basis file matching level (`y` flag, `--fuzzy`).
    ///
    /// - 0: disabled
    /// - 1: search destination directory for similar files (`-y`)
    /// - 2: also search reference directories (`-yy`)
    ///
    /// Each `y` in the compact flag string increments this counter,
    /// matching upstream `options.c:764` - `fuzzy_basis++`.
    pub fuzzy_level: u8,
    /// Prune empty directories from destination (`m` flag, `--prune-empty-dirs`).
    pub prune_empty_dirs: bool,
    /// Incremental recursion mode (`i` flag, `--inc-recursive`).
    ///
    /// When enabled, file lists are processed incrementally as entries arrive
    /// rather than waiting for the complete list. This reduces startup latency
    /// for large directory transfers.
    pub incremental_recursion: bool,

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
    /// Clears feature-gated flags that are not supported in this build.
    ///
    /// When the remote peer requests ACL preservation (`-A`) or extended
    /// attribute preservation (`-X`), but this binary was compiled without
    /// the corresponding feature (`acl` or `xattr`), the flag is cleared
    /// and the feature name is returned so the caller can emit a warning.
    ///
    /// This mirrors upstream rsync's `options.c:1842-1857` where
    /// `SUPPORT_ACLS` / `SUPPORT_XATTRS` guards produce an error. We
    /// choose a graceful fallback instead - warn and continue without the
    /// unsupported feature.
    ///
    /// # Returns
    ///
    /// A `Vec` of human-readable feature names that were requested but
    /// disabled (e.g., `["ACLs", "xattrs"]`). Empty when all requested
    /// features are available.
    pub fn clear_unsupported_features(&mut self) -> Vec<&'static str> {
        #[allow(unused_mut)] // REASON: mutated when acl or xattr features are not enabled
        let mut cleared = Vec::new();

        // ACL support requires the `acl` feature (Unix POSIX/NFSv4 ACLs or Windows DACLs).
        #[cfg(not(all(any(unix, windows), feature = "acl")))]
        if self.acls {
            self.acls = false;
            cleared.push("ACLs");
        }

        // Extended attribute support requires the `xattr` feature on Unix.
        #[cfg(not(all(unix, feature = "xattr")))]
        if self.xattrs {
            self.xattrs = false;
            cleared.push("xattrs");
        }

        cleared
    }

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

    const fn parse_transfer_flag(&mut self, byte: u8) {
        match byte {
            b'l' => self.links = true,
            b'o' => self.owner = true,
            b'g' => self.group = true,
            b'D' => {
                self.devices = true;
                self.specials = true;
            }
            b't' => self.times = true,
            b'U' => self.atimes = true,
            b'p' => self.perms = true,
            b'r' => self.recursive = true,
            b'e' => self.rsh = true,
            b'a' => self.archive = true,
            b'v' => self.verbose = true,
            b'z' => self.compress = true,
            b'c' => self.checksum = true,
            b'H' => self.hard_links = true,
            b'I' => self.ignore_times = true,
            b'A' => self.acls = true,
            b'X' => self.xattrs = true,
            // upstream: 'n' = dry_run (!do_xfers), NOT numeric_ids.
            // numeric_ids is long-form only (options.c:2887 sends --numeric-ids).
            b'n' => self.dry_run = true,
            // upstream: 'd' = --dirs (xfer_dirs without recursion), NOT delete.
            // delete is long-form only (options.c:2818-2827 sends --delete-*).
            b'd' => self.dirs = true,
            b'W' => self.whole_file = true,
            b'S' => self.sparse = true,
            b'x' => self.one_file_system = self.one_file_system.saturating_add(1),
            b'R' => self.relative = true,
            b'P' => self.partial = true,
            b'u' => self.update = true,
            // upstream: options.c:2613 — 'b' = backup.
            b'b' => self.backup = true,
            b'N' => self.crtimes = true,
            // upstream: options.c:764 — 'L' = copy_links (resolve symlinks).
            b'L' => self.copy_links = true,
            // upstream: options.c:764 - fuzzy_basis++ for each 'y'
            b'y' => self.fuzzy_level = self.fuzzy_level.saturating_add(1),
            b'm' => self.prune_empty_dirs = true,
            // Unknown flags are ignored to maintain forward compatibility
            _ => {}
        }
    }
}

impl InfoFlags {
    const fn parse_info_flag(&mut self, byte: u8) {
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
        assert_eq!(flags.fuzzy_level, 1);
    }

    #[test]
    fn parses_double_fuzzy_flag() {
        let flags = ParsedServerFlags::parse("-ryy").unwrap();
        assert!(flags.recursive);
        assert_eq!(flags.fuzzy_level, 2);
    }

    #[test]
    fn fuzzy_level_defaults_to_zero() {
        let flags = ParsedServerFlags::parse("-r").unwrap();
        assert_eq!(flags.fuzzy_level, 0);
    }

    #[test]
    fn parses_prune_empty_dirs_flag() {
        let flags = ParsedServerFlags::parse("-rm").unwrap();
        assert!(flags.recursive);
        assert!(flags.prune_empty_dirs);
    }

    #[test]
    fn parses_single_one_file_system_flag() {
        let flags = ParsedServerFlags::parse("-rx").unwrap();
        assert!(flags.recursive);
        assert_eq!(flags.one_file_system, 1);
    }

    #[test]
    fn parses_double_one_file_system_flag() {
        let flags = ParsedServerFlags::parse("-rxx").unwrap();
        assert!(flags.recursive);
        assert_eq!(flags.one_file_system, 2);
    }

    #[test]
    fn one_file_system_not_set_by_default() {
        let flags = ParsedServerFlags::parse("-r").unwrap();
        assert_eq!(flags.one_file_system, 0);
    }

    #[test]
    fn parses_crtimes_flag() {
        let flags = ParsedServerFlags::parse("-tN").unwrap();
        assert!(flags.times);
        assert!(flags.crtimes);
    }

    #[test]
    fn crtimes_not_set_by_default() {
        let flags = ParsedServerFlags::parse("-r").unwrap();
        assert!(!flags.crtimes);
    }

    #[test]
    fn parses_backup_flag() {
        let flags = ParsedServerFlags::parse("-rb").unwrap();
        assert!(flags.recursive);
        assert!(flags.backup);
    }

    #[test]
    fn backup_not_set_by_default() {
        let flags = ParsedServerFlags::parse("-r").unwrap();
        assert!(!flags.backup);
    }

    /// When neither ACLs nor xattrs are requested, clearing unsupported features
    /// returns an empty list and leaves all flags unchanged.
    #[test]
    fn clear_unsupported_features_noop_when_not_requested() {
        let mut flags = ParsedServerFlags::parse("-r").unwrap();
        assert!(!flags.acls);
        assert!(!flags.xattrs);
        let cleared = flags.clear_unsupported_features();
        assert!(cleared.is_empty());
        assert!(!flags.acls);
        assert!(!flags.xattrs);
    }

    /// When ACLs are requested and the `acl` feature is compiled in (Unix),
    /// no clearing occurs. When the feature is absent, the flag is cleared
    /// and the feature name is returned.
    ///
    /// upstream: options.c:1842-1857 - SUPPORT_ACLS guard
    #[test]
    fn clear_unsupported_features_handles_acls() {
        let mut flags = ParsedServerFlags::parse("-A").unwrap();
        assert!(flags.acls);
        let cleared = flags.clear_unsupported_features();

        #[cfg(all(any(unix, windows), feature = "acl"))]
        {
            assert!(cleared.is_empty());
            assert!(flags.acls);
        }

        #[cfg(not(all(any(unix, windows), feature = "acl")))]
        {
            assert_eq!(cleared, vec!["ACLs"]);
            assert!(!flags.acls);
        }
    }

    /// When xattrs are requested and the `xattr` feature is compiled in (Unix),
    /// no clearing occurs. When the feature is absent, the flag is cleared
    /// and the feature name is returned.
    ///
    /// upstream: options.c:1859-1868 - SUPPORT_XATTRS guard
    #[test]
    fn clear_unsupported_features_handles_xattrs() {
        let mut flags = ParsedServerFlags::parse("-X").unwrap();
        assert!(flags.xattrs);
        let cleared = flags.clear_unsupported_features();

        #[cfg(all(unix, feature = "xattr"))]
        {
            assert!(cleared.is_empty());
            assert!(flags.xattrs);
        }

        #[cfg(not(all(unix, feature = "xattr")))]
        {
            assert_eq!(cleared, vec!["xattrs"]);
            assert!(!flags.xattrs);
        }
    }

    /// When both ACLs and xattrs are requested but the platform lacks support,
    /// both are cleared and reported.
    ///
    /// upstream: options.c:1842-1868 - both guards apply independently
    #[test]
    fn clear_unsupported_features_handles_both_acl_and_xattr() {
        let mut flags = ParsedServerFlags::parse("-AX").unwrap();
        assert!(flags.acls);
        assert!(flags.xattrs);
        let cleared = flags.clear_unsupported_features();

        // On a platform without both features, both are cleared.
        // On a platform with both, neither is cleared.
        // On a platform with only one, only the missing one is cleared.
        // The exact result depends on the build, but the function must not panic.
        for name in &cleared {
            assert!(
                *name == "ACLs" || *name == "xattrs",
                "unexpected cleared feature: {name}"
            );
        }

        // After clearing, the flag matches whether the feature is available.
        #[cfg(all(any(unix, windows), feature = "acl"))]
        assert!(flags.acls, "ACLs should remain when feature is available");
        #[cfg(not(all(any(unix, windows), feature = "acl")))]
        assert!(!flags.acls, "ACLs should be cleared when feature is absent");

        #[cfg(all(unix, feature = "xattr"))]
        assert!(
            flags.xattrs,
            "xattrs should remain when feature is available"
        );
        #[cfg(not(all(unix, feature = "xattr")))]
        assert!(
            !flags.xattrs,
            "xattrs should be cleared when feature is absent"
        );
    }

    /// Clearing unsupported features is idempotent - calling it twice
    /// produces an empty result on the second call.
    #[test]
    fn clear_unsupported_features_idempotent() {
        let mut flags = ParsedServerFlags::parse("-AX").unwrap();
        let _ = flags.clear_unsupported_features();
        let second = flags.clear_unsupported_features();
        assert!(
            second.is_empty(),
            "second call should return empty: {second:?}"
        );
    }

    /// Other transfer flags are not affected by clear_unsupported_features.
    #[test]
    fn clear_unsupported_features_preserves_other_flags() {
        let mut flags = ParsedServerFlags::parse("-rAXz").unwrap();
        let _ = flags.clear_unsupported_features();
        assert!(flags.recursive, "recursive should be preserved");
        assert!(flags.compress, "compress should be preserved");
    }
}
