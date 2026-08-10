use std::str::FromStr;

use thiserror::Error;

/// Controls how byte counters are rendered for user-facing output.
///
/// Upstream `rsync` tracks `human_readable` as an integer with four distinct
/// levels (`options.c:111` defaults it to `1`; `-h`/`--human-readable`
/// increment it at `options.c:1573`; `--no-human-readable`/`--no-h` reset it to
/// `0` at `options.c:617`). Each level changes both the digit grouping and the
/// `--list-only` size-column width in `lib/compat.c:do_big_num` and
/// `generator.c:1159`:
///
/// | Level | Variant | Rendering | Size width |
/// |-------|---------|-----------|------------|
/// | 0 | [`Self::Raw`] | raw digits, no separators (`1234567`) | 11 |
/// | 1 | [`Self::Grouped`] | thousands-separated (`1,234,567`) | 14 |
/// | 2 | [`Self::DecimalUnits`] | base-1000 suffix (`1.23M`) | 14 |
/// | 3 | [`Self::BinaryUnits`] | base-1024 suffix (`1.18M`) | 14 |
///
/// Level 1 is the default when neither `-h` nor `--no-h` is supplied, so
/// [`Self::Grouped`] names the "no suffix humanisation" default rather than a
/// suppressed-output mode. See [`Self::unit_base`], [`Self::size_width`], and
/// [`Self::uses_separators`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[doc(alias = "--human-readable")]
pub enum HumanReadableMode {
    /// Level 0 (`--no-human-readable`/`--no-h`): raw decimal digits with no
    /// thousands separators, rendered in an 11-wide `--list-only` size column.
    ///
    /// upstream: `options.c:617` sets `human_readable = 0`; `lib/compat.c:231`
    /// skips separator insertion when `human_flag == 0`; `generator.c:1159`
    /// selects `size_width = 11` when `human_readable` is falsy.
    Raw,
    /// Level 1 (default): thousands-separated decimal digits (`1,234,567`) in a
    /// 14-wide size column, with no unit suffix.
    ///
    /// upstream: `options.c:111` initialises `human_readable = 1`.
    Grouped,
    /// Level 2 (`-h`): base-1000 suffix formatting (e.g. `1.23K`, `4.56M`).
    DecimalUnits,
    /// Level 3 (`-hh`): base-1024 suffix formatting (e.g. `1.18M`).
    BinaryUnits,
}

impl HumanReadableMode {
    /// Parses a human-readable level from textual input.
    ///
    /// The parser trims ASCII whitespace before interpreting the value and
    /// accepts the numeric levels used by upstream `rsync` (`0`-`3`). A
    /// dedicated error type captures empty inputs and out-of-range values so
    /// callers can emit diagnostics that match the original CLI.
    pub fn parse(text: &str) -> Result<Self, HumanReadableModeParseError> {
        let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
        if trimmed.is_empty() {
            return Err(HumanReadableModeParseError::Empty);
        }

        match trimmed {
            "0" => Ok(Self::Raw),
            "1" => Ok(Self::Grouped),
            "2" => Ok(Self::DecimalUnits),
            "3" => Ok(Self::BinaryUnits),
            other => Err(HumanReadableModeParseError::Invalid {
                value: other.to_owned(),
            }),
        }
    }

    /// Reports whether unit-suffix (`K`/`M`/`G`) formatting should be used.
    ///
    /// Only levels 2 (`-h`) and 3 (`-hh`) apply a suffix; levels 0 and 1 emit
    /// plain digits (differing only in separator grouping). Mirrors upstream's
    /// `human_flag > 1` gate in `lib/compat.c:182`.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::DecimalUnits | Self::BinaryUnits)
    }

    /// Reports whether thousands separators are inserted between digit groups.
    ///
    /// Only the default level 1 ([`Self::Grouped`]) groups digits with the
    /// locale separator; level 0 ([`Self::Raw`]) emits raw digits. Mirrors
    /// upstream `lib/compat.c:231`, where the separator is inserted only when
    /// `human_flag` is non-zero and no unit suffix applies.
    #[must_use]
    pub const fn uses_separators(self) -> bool {
        matches!(self, Self::Grouped)
    }

    /// The `--list-only` size-column width for this level.
    ///
    /// Mirrors `generator.c:1159`: `size_width = human_readable ? 14 : 11`, so
    /// only level 0 ([`Self::Raw`]) uses the 11-wide field.
    #[must_use]
    pub const fn size_width(self) -> usize {
        match self {
            Self::Raw => 11,
            Self::Grouped | Self::DecimalUnits | Self::BinaryUnits => 14,
        }
    }

    /// The unit multiplier upstream `do_big_num` applies for K/M/G/T units.
    ///
    /// Mirrors `lib/compat.c:183`: `mult = human_flag == 2 ? 1000 : 1024`.
    /// A single `-h` (level 2, [`Self::DecimalUnits`]) uses base 1000; `-hh`
    /// (level 3, [`Self::BinaryUnits`]) uses base 1024. The value is only
    /// meaningful when [`Self::is_enabled`] is true.
    #[must_use]
    pub const fn unit_base(self) -> f64 {
        match self {
            Self::BinaryUnits => 1024.0,
            _ => 1000.0,
        }
    }
}

impl FromStr for HumanReadableMode {
    type Err = HumanReadableModeParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Errors produced when parsing a [`HumanReadableMode`] from text.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum HumanReadableModeParseError {
    /// The provided value was empty after trimming ASCII whitespace.
    #[error("human-readable level must not be empty")]
    Empty,
    /// The provided value did not match an accepted human-readable level.
    #[error("invalid human-readable level '{value}': expected 0, 1, 2, or 3")]
    Invalid {
        /// The invalid value supplied by the caller after trimming ASCII whitespace.
        value: String,
    },
}

impl HumanReadableModeParseError {
    /// Returns the invalid value supplied by the caller when available.
    pub const fn invalid_value(&self) -> Option<&str> {
        match self {
            Self::Invalid { value } => Some(value.as_str()),
            Self::Empty => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // upstream: options.c levels 0-3 map to Raw / Grouped / DecimalUnits / BinaryUnits.
    #[test]
    fn parse_level_0() {
        assert_eq!(
            HumanReadableMode::parse("0").unwrap(),
            HumanReadableMode::Raw
        );
    }

    #[test]
    fn parse_level_1() {
        assert_eq!(
            HumanReadableMode::parse("1").unwrap(),
            HumanReadableMode::Grouped
        );
    }

    #[test]
    fn parse_level_2() {
        assert_eq!(
            HumanReadableMode::parse("2").unwrap(),
            HumanReadableMode::DecimalUnits
        );
    }

    #[test]
    fn parse_level_3() {
        assert_eq!(
            HumanReadableMode::parse("3").unwrap(),
            HumanReadableMode::BinaryUnits
        );
    }

    #[test]
    fn parse_with_whitespace() {
        assert_eq!(
            HumanReadableMode::parse("  1  ").unwrap(),
            HumanReadableMode::Grouped
        );
    }

    #[test]
    fn parse_empty_returns_error() {
        let result = HumanReadableMode::parse("");
        assert!(matches!(result, Err(HumanReadableModeParseError::Empty)));
    }

    #[test]
    fn parse_invalid_returns_error() {
        let result = HumanReadableMode::parse("4");
        assert!(matches!(
            result,
            Err(HumanReadableModeParseError::Invalid { .. })
        ));
    }

    #[test]
    fn from_str_works() {
        use std::str::FromStr;
        assert_eq!(
            HumanReadableMode::from_str("2").unwrap(),
            HumanReadableMode::DecimalUnits
        );
    }

    #[test]
    fn is_enabled_raw() {
        // Level 0 emits raw digits, so no unit-suffix formatting.
        assert!(!HumanReadableMode::Raw.is_enabled());
    }

    #[test]
    fn is_enabled_disabled() {
        // Level 1 (default) groups digits but applies no unit suffix.
        assert!(!HumanReadableMode::Grouped.is_enabled());
    }

    #[test]
    fn is_enabled_enabled() {
        assert!(HumanReadableMode::DecimalUnits.is_enabled());
    }

    #[test]
    fn is_enabled_combined() {
        assert!(HumanReadableMode::BinaryUnits.is_enabled());
    }

    #[test]
    fn uses_separators_only_default_level() {
        // upstream: lib/compat.c:231 - separators only when human_flag != 0 and
        // no suffix applies, i.e. exactly level 1.
        assert!(!HumanReadableMode::Raw.uses_separators());
        assert!(HumanReadableMode::Grouped.uses_separators());
        assert!(!HumanReadableMode::DecimalUnits.uses_separators());
        assert!(!HumanReadableMode::BinaryUnits.uses_separators());
    }

    #[test]
    fn size_width_only_raw_is_eleven() {
        // upstream: generator.c:1159 - size_width = human_readable ? 14 : 11.
        assert_eq!(HumanReadableMode::Raw.size_width(), 11);
        assert_eq!(HumanReadableMode::Grouped.size_width(), 14);
        assert_eq!(HumanReadableMode::DecimalUnits.size_width(), 14);
        assert_eq!(HumanReadableMode::BinaryUnits.size_width(), 14);
    }

    #[test]
    fn unit_base_levels() {
        // upstream: lib/compat.c:183 - mult = human_flag == 2 ? 1000 : 1024.
        assert_eq!(HumanReadableMode::DecimalUnits.unit_base(), 1000.0);
        assert_eq!(HumanReadableMode::BinaryUnits.unit_base(), 1024.0);
    }

    #[test]
    fn parse_error_invalid_value() {
        let err = HumanReadableMode::parse("foo").unwrap_err();
        assert_eq!(err.invalid_value(), Some("foo"));
    }

    #[test]
    fn parse_error_empty_value() {
        let err = HumanReadableMode::parse("").unwrap_err();
        assert_eq!(err.invalid_value(), None);
    }

    #[test]
    fn error_display() {
        let empty_err = HumanReadableModeParseError::Empty;
        assert!(empty_err.to_string().contains("must not be empty"));

        let invalid_err = HumanReadableModeParseError::Invalid {
            value: "3".to_owned(),
        };
        assert!(invalid_err.to_string().contains("invalid"));
    }
}
