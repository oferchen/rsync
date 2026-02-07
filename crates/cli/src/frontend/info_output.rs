//! Info output flags for controlling informational messages.
//!
//! This module implements upstream rsync's `--info=FLAGS` output behavior,
//! controlling which informational messages are displayed during transfer.
//!
//! # Examples
//!
//! ```
//! use cli::info_output::{InfoFlags, parse_info_flags};
//!
//! // Parse info flags from command-line
//! let flags = parse_info_flags("name2,del1,stats2").unwrap();
//! assert!(flags.should_show_name());
//! assert!(flags.should_show_stats());
//! assert!(flags.should_show_del());
//!
//! // Create from verbosity level
//! let flags = InfoFlags::from_verbosity(2);
//! assert!(flags.should_show_name());
//! ```

use logging::{InfoFlag, InfoLevels};
use std::fmt;

/// Error type for info flag parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfoFlagError {
    message: String,
}

impl InfoFlagError {
    /// Create a new error with the given message.
    fn new(message: String) -> Self {
        Self { message }
    }
}

impl fmt::Display for InfoFlagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for InfoFlagError {}

/// Info output flags controlling which informational messages are displayed.
///
/// This wraps the `logging::InfoLevels` type and provides helper methods
/// for determining what should be shown based on the configured levels.
#[derive(Clone, Debug, Default)]
pub struct InfoFlags {
    levels: InfoLevels,
}

impl InfoFlags {
    /// Create a new set of info flags from info levels.
    #[must_use]
    pub const fn new(levels: InfoLevels) -> Self {
        Self { levels }
    }

    /// Create info flags derived from a verbosity level.
    ///
    /// This maps the traditional `-v`/`-vv`/`-vvv` verbosity levels to
    /// appropriate info flag settings following upstream rsync's behavior.
    ///
    /// # Arguments
    ///
    /// * `level` - Verbosity level (0 = silent, 1 = normal, 2 = verbose, 3+ = debug)
    ///
    /// # Examples
    ///
    /// ```
    /// use cli::info_output::InfoFlags;
    ///
    /// let flags = InfoFlags::from_verbosity(1);
    /// assert!(flags.should_show_name());
    ///
    /// let flags = InfoFlags::from_verbosity(0);
    /// assert!(!flags.should_show_name());
    /// ```
    #[must_use]
    pub fn from_verbosity(level: u32) -> Self {
        let config = logging::VerbosityConfig::from_verbose_level(level.min(255) as u8);
        Self {
            levels: config.info,
        }
    }

    /// Get the underlying info levels.
    #[must_use]
    pub const fn levels(&self) -> &InfoLevels {
        &self.levels
    }

    /// Get a mutable reference to the underlying info levels.
    #[must_use]
    pub fn levels_mut(&mut self) -> &mut InfoLevels {
        &mut self.levels
    }

    /// Check if filename output should be shown during transfer.
    ///
    /// This corresponds to the `name` info flag.
    #[must_use]
    pub const fn should_show_name(&self) -> bool {
        self.levels.get(InfoFlag::Name) > 0
    }

    /// Check if progress messages should be shown.
    ///
    /// This corresponds to the `progress` info flag.
    #[must_use]
    pub const fn should_show_progress(&self) -> bool {
        self.levels.get(InfoFlag::Progress) > 0
    }

    /// Check if statistics should be shown.
    ///
    /// This corresponds to the `stats` info flag.
    #[must_use]
    pub const fn should_show_stats(&self) -> bool {
        self.levels.get(InfoFlag::Stats) > 0
    }

    /// Check if deletion messages should be shown.
    ///
    /// This corresponds to the `del` info flag.
    #[must_use]
    pub const fn should_show_del(&self) -> bool {
        self.levels.get(InfoFlag::Del) > 0
    }

    /// Check if skip messages should be shown.
    ///
    /// This corresponds to the `skip` info flag.
    #[must_use]
    pub const fn should_show_skip(&self) -> bool {
        self.levels.get(InfoFlag::Skip) > 0
    }

    /// Check if file list info should be shown.
    ///
    /// This corresponds to the `flist` info flag.
    #[must_use]
    pub const fn should_show_flist(&self) -> bool {
        self.levels.get(InfoFlag::Flist) > 0
    }

    /// Check if backup messages should be shown.
    ///
    /// This corresponds to the `backup` info flag.
    #[must_use]
    pub const fn should_show_backup(&self) -> bool {
        self.levels.get(InfoFlag::Backup) > 0
    }

    /// Check if copy messages should be shown.
    ///
    /// This corresponds to the `copy` info flag.
    #[must_use]
    pub const fn should_show_copy(&self) -> bool {
        self.levels.get(InfoFlag::Copy) > 0
    }

    /// Check if miscellaneous messages should be shown.
    ///
    /// This corresponds to the `misc` info flag.
    #[must_use]
    pub const fn should_show_misc(&self) -> bool {
        self.levels.get(InfoFlag::Misc) > 0
    }

    /// Check if mount point messages should be shown.
    ///
    /// This corresponds to the `mount` info flag.
    #[must_use]
    pub const fn should_show_mount(&self) -> bool {
        self.levels.get(InfoFlag::Mount) > 0
    }

    /// Check if remove messages should be shown.
    ///
    /// This corresponds to the `remove` info flag.
    #[must_use]
    pub const fn should_show_remove(&self) -> bool {
        self.levels.get(InfoFlag::Remove) > 0
    }

    /// Check if symlink safety messages should be shown.
    ///
    /// This corresponds to the `symsafe` info flag.
    #[must_use]
    pub const fn should_show_symsafe(&self) -> bool {
        self.levels.get(InfoFlag::Symsafe) > 0
    }
}

/// Parse info flags from a string like "name2,del1,stats2".
///
/// Supports the following syntax:
/// - Individual flags with optional level: `name2`, `del`, `stats3`
/// - Multiple flags separated by commas: `name2,del1,stats2`
/// - `ALL` keyword to set all flags to a level: `ALL2`
/// - `NONE` keyword to set all flags to 0: `NONE`
///
/// Flag names are case-insensitive.
///
/// # Errors
///
/// Returns `InfoFlagError` if:
/// - An invalid flag name is provided
/// - An invalid level is specified
/// - The input string has invalid syntax
///
/// # Examples
///
/// ```
/// use cli::info_output::parse_info_flags;
///
/// let flags = parse_info_flags("name2").unwrap();
/// assert!(flags.should_show_name());
///
/// let flags = parse_info_flags("name2,del1,stats2").unwrap();
/// assert!(flags.should_show_name());
/// assert!(flags.should_show_del());
/// assert!(flags.should_show_stats());
///
/// let flags = parse_info_flags("ALL2").unwrap();
/// assert!(flags.should_show_name());
/// assert!(flags.should_show_stats());
///
/// let flags = parse_info_flags("NONE").unwrap();
/// assert!(!flags.should_show_name());
/// assert!(!flags.should_show_stats());
/// ```
pub fn parse_info_flags(flags_str: &str) -> Result<InfoFlags, InfoFlagError> {
    if flags_str.is_empty() {
        return Ok(InfoFlags::default());
    }

    let mut levels = InfoLevels::default();

    for token in flags_str.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }

        // Check for ALL keyword
        if token.eq_ignore_ascii_case("all") {
            levels.set_all(1);
            continue;
        }

        // Check for ALL with level (e.g., ALL2)
        if let Some(rest) = token.strip_prefix("ALL") {
            if rest.is_empty() {
                levels.set_all(1);
            } else {
                let level = rest
                    .parse::<u8>()
                    .map_err(|_| InfoFlagError::new(format!("invalid level in flag: {}", token)))?;
                levels.set_all(level);
            }
            continue;
        }

        // Case-insensitive ALL check
        if let Some(rest) = token.to_lowercase().strip_prefix("all") {
            if rest.is_empty() {
                levels.set_all(1);
            } else {
                let level = rest
                    .parse::<u8>()
                    .map_err(|_| InfoFlagError::new(format!("invalid level in flag: {}", token)))?;
                levels.set_all(level);
            }
            continue;
        }

        // Check for NONE keyword
        if token.eq_ignore_ascii_case("none") {
            levels.set_all(0);
            continue;
        }

        // Parse individual flag
        let (flag_name, level) = parse_flag_token(token)?;
        let flag = parse_flag_name(flag_name)?;
        levels.set(flag, level);
    }

    Ok(InfoFlags { levels })
}

/// Parse a flag token like "name2" into (InfoFlag::Name, 2).
fn parse_flag_token(token: &str) -> Result<(&str, u8), InfoFlagError> {
    if token.is_empty() {
        return Err(InfoFlagError::new("empty flag token".to_owned()));
    }

    // Find where the digits start
    let digit_start = token.find(|c: char| c.is_ascii_digit());

    match digit_start {
        Some(pos) => {
            let name = &token[..pos];
            let level_str = &token[pos..];
            let level = level_str
                .parse::<u8>()
                .map_err(|_| InfoFlagError::new(format!("invalid level in flag: {}", token)))?;
            Ok((name, level))
        }
        None => {
            // No digits, default to level 1
            Ok((token, 1))
        }
    }
}

/// Parse a flag name into an InfoFlag enum.
fn parse_flag_name(name: &str) -> Result<InfoFlag, InfoFlagError> {
    match name.to_lowercase().as_str() {
        "backup" => Ok(InfoFlag::Backup),
        "copy" => Ok(InfoFlag::Copy),
        "del" => Ok(InfoFlag::Del),
        "flist" => Ok(InfoFlag::Flist),
        "misc" => Ok(InfoFlag::Misc),
        "mount" => Ok(InfoFlag::Mount),
        "name" => Ok(InfoFlag::Name),
        "nonreg" => Ok(InfoFlag::Nonreg),
        "progress" => Ok(InfoFlag::Progress),
        "remove" => Ok(InfoFlag::Remove),
        "skip" => Ok(InfoFlag::Skip),
        "stats" => Ok(InfoFlag::Stats),
        "symsafe" => Ok(InfoFlag::Symsafe),
        _ => Err(InfoFlagError::new(format!("unknown info flag: {}", name))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_flag() {
        let flags = parse_info_flags("name2").unwrap();
        assert_eq!(flags.levels().get(InfoFlag::Name), 2);
        assert!(flags.should_show_name());
    }

    #[test]
    fn test_parse_multiple_flags() {
        let flags = parse_info_flags("name2,del1,stats2").unwrap();
        assert_eq!(flags.levels().get(InfoFlag::Name), 2);
        assert_eq!(flags.levels().get(InfoFlag::Del), 1);
        assert_eq!(flags.levels().get(InfoFlag::Stats), 2);
        assert!(flags.should_show_name());
        assert!(flags.should_show_del());
        assert!(flags.should_show_stats());
    }

    #[test]
    fn test_parse_all_keyword() {
        let flags = parse_info_flags("ALL").unwrap();
        assert_eq!(flags.levels().get(InfoFlag::Name), 1);
        assert_eq!(flags.levels().get(InfoFlag::Stats), 1);
        assert_eq!(flags.levels().get(InfoFlag::Del), 1);
        assert!(flags.should_show_name());
        assert!(flags.should_show_stats());
    }

    #[test]
    fn test_parse_all_with_level() {
        let flags = parse_info_flags("ALL2").unwrap();
        assert_eq!(flags.levels().get(InfoFlag::Name), 2);
        assert_eq!(flags.levels().get(InfoFlag::Stats), 2);
        assert_eq!(flags.levels().get(InfoFlag::Del), 2);
    }

    #[test]
    fn test_parse_none_keyword() {
        let flags = parse_info_flags("NONE").unwrap();
        assert_eq!(flags.levels().get(InfoFlag::Name), 0);
        assert_eq!(flags.levels().get(InfoFlag::Stats), 0);
        assert_eq!(flags.levels().get(InfoFlag::Del), 0);
        assert!(!flags.should_show_name());
        assert!(!flags.should_show_stats());
    }

    #[test]
    fn test_from_verbosity_level_0() {
        let flags = InfoFlags::from_verbosity(0);
        assert!(!flags.should_show_name());
        assert!(!flags.should_show_stats());
        assert!(!flags.should_show_del());
    }

    #[test]
    fn test_from_verbosity_level_1() {
        let flags = InfoFlags::from_verbosity(1);
        assert!(flags.should_show_name());
        assert!(flags.should_show_stats());
        assert!(flags.should_show_del());
        assert_eq!(flags.levels().get(InfoFlag::Name), 1);
    }

    #[test]
    fn test_from_verbosity_level_2() {
        let flags = InfoFlags::from_verbosity(2);
        assert!(flags.should_show_name());
        assert!(flags.should_show_stats());
        assert_eq!(flags.levels().get(InfoFlag::Name), 2);
    }

    #[test]
    fn test_from_verbosity_level_3() {
        let flags = InfoFlags::from_verbosity(3);
        assert!(flags.should_show_name());
        assert!(flags.should_show_stats());
        assert_eq!(flags.levels().get(InfoFlag::Name), 2);
    }

    #[test]
    fn test_should_show_helpers() {
        let mut flags = InfoFlags::default();

        // Initially all should be false
        assert!(!flags.should_show_name());
        assert!(!flags.should_show_progress());
        assert!(!flags.should_show_stats());
        assert!(!flags.should_show_del());
        assert!(!flags.should_show_skip());
        assert!(!flags.should_show_flist());

        // Set individual flags
        flags.levels_mut().set(InfoFlag::Name, 1);
        assert!(flags.should_show_name());

        flags.levels_mut().set(InfoFlag::Progress, 2);
        assert!(flags.should_show_progress());

        flags.levels_mut().set(InfoFlag::Stats, 1);
        assert!(flags.should_show_stats());

        flags.levels_mut().set(InfoFlag::Del, 1);
        assert!(flags.should_show_del());

        flags.levels_mut().set(InfoFlag::Skip, 1);
        assert!(flags.should_show_skip());

        flags.levels_mut().set(InfoFlag::Flist, 1);
        assert!(flags.should_show_flist());
    }

    #[test]
    fn test_invalid_flag_name() {
        let result = parse_info_flags("invalid");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("unknown info flag"));
    }

    #[test]
    fn test_invalid_level() {
        let result = parse_info_flags("name999999999999999999");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invalid level"));
    }

    #[test]
    fn test_default_flags() {
        let flags = InfoFlags::default();
        assert!(!flags.should_show_name());
        assert!(!flags.should_show_stats());
        assert!(!flags.should_show_del());
        assert!(!flags.should_show_skip());
        assert!(!flags.should_show_flist());
        assert!(!flags.should_show_progress());
    }

    #[test]
    fn test_case_insensitivity() {
        let flags1 = parse_info_flags("NAME2").unwrap();
        let flags2 = parse_info_flags("name2").unwrap();
        let flags3 = parse_info_flags("Name2").unwrap();

        assert_eq!(flags1.levels().get(InfoFlag::Name), 2);
        assert_eq!(flags2.levels().get(InfoFlag::Name), 2);
        assert_eq!(flags3.levels().get(InfoFlag::Name), 2);
    }

    #[test]
    fn test_all_keyword_case_insensitive() {
        let flags1 = parse_info_flags("ALL").unwrap();
        let flags2 = parse_info_flags("all").unwrap();
        let flags3 = parse_info_flags("All").unwrap();

        assert!(flags1.should_show_name());
        assert!(flags2.should_show_name());
        assert!(flags3.should_show_name());
    }

    #[test]
    fn test_none_keyword_case_insensitive() {
        let flags1 = parse_info_flags("NONE").unwrap();
        let flags2 = parse_info_flags("none").unwrap();
        let flags3 = parse_info_flags("None").unwrap();

        assert!(!flags1.should_show_name());
        assert!(!flags2.should_show_name());
        assert!(!flags3.should_show_name());
    }

    #[test]
    fn test_empty_string_parsing() {
        let flags = parse_info_flags("").unwrap();
        assert!(!flags.should_show_name());
        assert!(!flags.should_show_stats());
    }

    #[test]
    fn test_flag_without_level_defaults_to_1() {
        let flags = parse_info_flags("name,stats,del").unwrap();
        assert_eq!(flags.levels().get(InfoFlag::Name), 1);
        assert_eq!(flags.levels().get(InfoFlag::Stats), 1);
        assert_eq!(flags.levels().get(InfoFlag::Del), 1);
    }

    #[test]
    fn test_all_info_flags() {
        let flags = parse_info_flags(
            "backup,copy,del,flist,misc,mount,name,nonreg,progress,remove,skip,stats,symsafe",
        )
        .unwrap();

        assert!(flags.should_show_backup());
        assert!(flags.should_show_copy());
        assert!(flags.should_show_del());
        assert!(flags.should_show_flist());
        assert!(flags.should_show_misc());
        assert!(flags.should_show_mount());
        assert!(flags.should_show_name());
        assert!(flags.should_show_progress());
        assert!(flags.should_show_remove());
        assert!(flags.should_show_skip());
        assert!(flags.should_show_stats());
        assert!(flags.should_show_symsafe());
    }

    #[test]
    fn test_mixed_flags_and_all() {
        // ALL followed by specific overrides
        let flags = parse_info_flags("ALL,name3").unwrap();
        assert_eq!(flags.levels().get(InfoFlag::Name), 3);
        assert_eq!(flags.levels().get(InfoFlag::Stats), 1);
    }

    #[test]
    fn test_whitespace_handling() {
        let flags = parse_info_flags("name2, del1, stats2").unwrap();
        assert_eq!(flags.levels().get(InfoFlag::Name), 2);
        assert_eq!(flags.levels().get(InfoFlag::Del), 1);
        assert_eq!(flags.levels().get(InfoFlag::Stats), 2);
    }

    #[test]
    fn test_all_should_show_helpers() {
        let flags = InfoFlags::from_verbosity(2);

        // Test all helper methods
        assert!(flags.should_show_backup());
        assert!(flags.should_show_copy());
        assert!(flags.should_show_del());
        assert!(flags.should_show_flist());
        assert!(flags.should_show_misc());
        assert!(flags.should_show_mount());
        assert!(flags.should_show_name());
        assert!(flags.should_show_remove());
        assert!(flags.should_show_skip());
        assert!(flags.should_show_stats());
        assert!(flags.should_show_symsafe());
    }

    #[test]
    fn test_verbosity_large_values() {
        // Should clamp to u8 max
        let flags = InfoFlags::from_verbosity(1000);
        assert!(flags.should_show_name());
    }

    #[test]
    fn test_error_display() {
        let error = InfoFlagError::new("test error".to_owned());
        assert_eq!(error.to_string(), "test error");
    }

    #[test]
    fn test_info_flags_clone() {
        let flags = parse_info_flags("name2,stats3").unwrap();
        let cloned = flags.clone();
        assert_eq!(cloned.levels().get(InfoFlag::Name), 2);
        assert_eq!(cloned.levels().get(InfoFlag::Stats), 3);
    }

    #[test]
    fn test_multiple_comma_separators() {
        let flags = parse_info_flags("name2,,del1,,,stats2").unwrap();
        assert_eq!(flags.levels().get(InfoFlag::Name), 2);
        assert_eq!(flags.levels().get(InfoFlag::Del), 1);
        assert_eq!(flags.levels().get(InfoFlag::Stats), 2);
    }
}
