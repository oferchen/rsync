//! TCP Fast Open mode selection for daemon listener and client connect.
//!
//! `auto` lets oc-rsync enable TFO opportunistically: enabled on platforms
//! that implement the option (Linux server side, FreeBSD), silently
//! skipped on platforms that do not (Windows, macOS server side). `on`
//! requests TFO unconditionally and emits a one-shot startup warning when
//! the platform does not support it. `off` disables TFO entirely.
//!
//! Wire-compatible with upstream rsync: TFO only affects the initial SYN
//! exchange and does not alter the rsync protocol.

use std::fmt;
use std::str::FromStr;

/// CLI selection for the `--tcp-fastopen` flag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[doc(alias = "--tcp-fastopen")]
#[derive(Default)]
pub enum TcpFastOpenMode {
    /// Enable TFO opportunistically; silently skip on unsupported
    /// platforms. This is the default.
    #[default]
    Auto,
    /// Enable TFO unconditionally. Emits a startup warning on platforms
    /// that do not support the option.
    On,
    /// Disable TFO.
    Off,
}

impl TcpFastOpenMode {
    /// Returns `true` when the caller requested that TFO be enabled.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Auto | Self::On)
    }

    /// Returns `true` when the caller asked for TFO unconditionally.
    ///
    /// Callers use this to decide whether to surface an unsupported
    /// platform warning at startup.
    #[must_use]
    pub const fn is_strict(self) -> bool {
        matches!(self, Self::On)
    }

    /// Returns the human-readable token written by the CLI parser.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }
}

/// Error returned when parsing an unknown `--tcp-fastopen` value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseTcpFastOpenModeError {
    value: String,
}

impl ParseTcpFastOpenModeError {
    /// Returns the raw value that failed to parse.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for ParseTcpFastOpenModeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid --tcp-fastopen value '{}': expected 'auto', 'on', or 'off'",
            self.value
        )
    }
}

impl std::error::Error for ParseTcpFastOpenModeError {}

impl FromStr for TcpFastOpenMode {
    type Err = ParseTcpFastOpenModeError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Ok(Self::Auto),
            "on" | "yes" | "true" | "1" => Ok(Self::On),
            "off" | "no" | "false" | "0" => Ok(Self::Off),
            _ => Err(ParseTcpFastOpenModeError {
                value: input.to_string(),
            }),
        }
    }
}

impl fmt::Display for TcpFastOpenMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_auto() {
        assert_eq!(TcpFastOpenMode::default(), TcpFastOpenMode::Auto);
    }

    #[test]
    fn auto_and_on_are_enabled() {
        assert!(TcpFastOpenMode::Auto.is_enabled());
        assert!(TcpFastOpenMode::On.is_enabled());
        assert!(!TcpFastOpenMode::Off.is_enabled());
    }

    #[test]
    fn only_on_is_strict() {
        assert!(TcpFastOpenMode::On.is_strict());
        assert!(!TcpFastOpenMode::Auto.is_strict());
        assert!(!TcpFastOpenMode::Off.is_strict());
    }

    #[test]
    fn parses_canonical_tokens() {
        assert_eq!(
            "auto".parse::<TcpFastOpenMode>().unwrap(),
            TcpFastOpenMode::Auto
        );
        assert_eq!(
            "on".parse::<TcpFastOpenMode>().unwrap(),
            TcpFastOpenMode::On
        );
        assert_eq!(
            "off".parse::<TcpFastOpenMode>().unwrap(),
            TcpFastOpenMode::Off
        );
    }

    #[test]
    fn parses_aliases_and_ignores_case() {
        assert_eq!(
            "YES".parse::<TcpFastOpenMode>().unwrap(),
            TcpFastOpenMode::On
        );
        assert_eq!(
            "False".parse::<TcpFastOpenMode>().unwrap(),
            TcpFastOpenMode::Off
        );
        assert_eq!("1".parse::<TcpFastOpenMode>().unwrap(), TcpFastOpenMode::On);
        assert_eq!(
            "0".parse::<TcpFastOpenMode>().unwrap(),
            TcpFastOpenMode::Off
        );
    }

    #[test]
    fn rejects_unknown_token() {
        let err = "maybe".parse::<TcpFastOpenMode>().unwrap_err();
        assert_eq!(err.value(), "maybe");
        assert!(err.to_string().contains("--tcp-fastopen"));
    }

    #[test]
    fn round_trip_display() {
        for mode in [
            TcpFastOpenMode::Auto,
            TcpFastOpenMode::On,
            TcpFastOpenMode::Off,
        ] {
            let token = mode.to_string();
            let parsed: TcpFastOpenMode = token.parse().unwrap();
            assert_eq!(parsed, mode);
        }
    }
}
