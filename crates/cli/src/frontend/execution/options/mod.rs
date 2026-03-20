//! Command-line argument parsing utilities for rsync options.
//!
//! This module provides validation and parsing functions for command-line arguments
//! that require special handling beyond simple string conversion. Each parser produces
//! user-friendly error messages that mirror upstream rsync's diagnostic style.
//!
//! # Submodules
//!
//! - [`iconv`] - Charset specification parsing for `--iconv`
//! - [`numeric`] - Simple numeric argument parsers (`--timeout`, `--max-delete`, etc.)
//! - [`protocol`] - Protocol version parsing for `--protocol`
//! - [`size`] - Size specification parsing with unit suffixes (`--block-size`, `--max-size`, etc.)

mod iconv;
mod numeric;
mod protocol;
mod size;

pub(crate) use iconv::resolve_iconv_setting;
pub(crate) use numeric::{
    parse_checksum_seed_argument, parse_human_readable_level, parse_max_delete_argument,
    parse_modify_window_argument, parse_timeout_argument,
};
pub(crate) use protocol::parse_protocol_version_arg;
#[cfg(test)]
pub(crate) use size::{SizeParseError, pow_u128_for_size};
pub(crate) use size::{parse_block_size_argument, parse_size_limit_argument};

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    use core::client::{HumanReadableMode, TransferTimeout};

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    // --- parse_timeout_argument tests ---

    #[test]
    fn parse_timeout_argument_zero() {
        let result = parse_timeout_argument(&os("0")).unwrap();
        assert_eq!(result, TransferTimeout::Disabled);
    }

    #[test]
    fn parse_timeout_argument_positive() {
        let result = parse_timeout_argument(&os("30")).unwrap();
        assert!(matches!(result, TransferTimeout::Seconds(n) if n.get() == 30));
    }

    #[test]
    fn parse_timeout_argument_with_plus() {
        let result = parse_timeout_argument(&os("+60")).unwrap();
        assert!(matches!(result, TransferTimeout::Seconds(n) if n.get() == 60));
    }

    #[test]
    fn parse_timeout_argument_empty() {
        let result = parse_timeout_argument(&os(""));
        assert!(result.is_err());
    }

    #[test]
    fn parse_timeout_argument_negative() {
        let result = parse_timeout_argument(&os("-10"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_timeout_argument_invalid() {
        let result = parse_timeout_argument(&os("abc"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_timeout_argument_whitespace() {
        let result = parse_timeout_argument(&os("  30  ")).unwrap();
        assert!(matches!(result, TransferTimeout::Seconds(n) if n.get() == 30));
    }

    // --- parse_max_delete_argument tests ---

    #[test]
    fn parse_max_delete_argument_zero() {
        assert_eq!(parse_max_delete_argument(&os("0")).unwrap(), 0);
    }

    #[test]
    fn parse_max_delete_argument_positive() {
        assert_eq!(parse_max_delete_argument(&os("100")).unwrap(), 100);
    }

    #[test]
    fn parse_max_delete_argument_with_plus() {
        assert_eq!(parse_max_delete_argument(&os("+50")).unwrap(), 50);
    }

    #[test]
    fn parse_max_delete_argument_empty() {
        assert!(parse_max_delete_argument(&os("")).is_err());
    }

    #[test]
    fn parse_max_delete_argument_negative() {
        assert!(parse_max_delete_argument(&os("-10")).is_err());
    }

    #[test]
    fn parse_max_delete_argument_invalid() {
        assert!(parse_max_delete_argument(&os("xyz")).is_err());
    }

    // --- parse_checksum_seed_argument tests ---

    #[test]
    fn parse_checksum_seed_argument_zero() {
        assert_eq!(parse_checksum_seed_argument(&os("0")).unwrap(), 0);
    }

    #[test]
    fn parse_checksum_seed_argument_positive() {
        assert_eq!(parse_checksum_seed_argument(&os("12345")).unwrap(), 12345);
    }

    #[test]
    fn parse_checksum_seed_argument_with_plus() {
        assert_eq!(parse_checksum_seed_argument(&os("+999")).unwrap(), 999);
    }

    #[test]
    fn parse_checksum_seed_argument_empty() {
        assert!(parse_checksum_seed_argument(&os("")).is_err());
    }

    #[test]
    fn parse_checksum_seed_argument_negative() {
        assert!(parse_checksum_seed_argument(&os("-1")).is_err());
    }

    #[test]
    fn parse_checksum_seed_argument_invalid() {
        assert!(parse_checksum_seed_argument(&os("abc")).is_err());
    }

    // --- parse_modify_window_argument tests ---

    #[test]
    fn parse_modify_window_argument_zero() {
        assert_eq!(parse_modify_window_argument(&os("0")).unwrap(), 0);
    }

    #[test]
    fn parse_modify_window_argument_positive() {
        assert_eq!(parse_modify_window_argument(&os("2")).unwrap(), 2);
    }

    #[test]
    fn parse_modify_window_argument_with_plus() {
        assert_eq!(parse_modify_window_argument(&os("+5")).unwrap(), 5);
    }

    #[test]
    fn parse_modify_window_argument_empty() {
        assert!(parse_modify_window_argument(&os("")).is_err());
    }

    #[test]
    fn parse_modify_window_argument_negative() {
        assert!(parse_modify_window_argument(&os("-1")).is_err());
    }

    #[test]
    fn parse_modify_window_argument_invalid() {
        assert!(parse_modify_window_argument(&os("foo")).is_err());
    }

    // --- parse_human_readable_level tests ---

    #[test]
    fn parse_human_readable_level_zero() {
        let result = parse_human_readable_level(&os("0")).unwrap();
        assert_eq!(result, HumanReadableMode::Disabled);
    }

    #[test]
    fn parse_human_readable_level_one() {
        let result = parse_human_readable_level(&os("1")).unwrap();
        assert_eq!(result, HumanReadableMode::Enabled);
    }

    #[test]
    fn parse_human_readable_level_two() {
        let result = parse_human_readable_level(&os("2")).unwrap();
        assert_eq!(result, HumanReadableMode::Combined);
    }

    #[test]
    fn parse_human_readable_level_invalid() {
        let result = parse_human_readable_level(&os("invalid"));
        assert!(result.is_err());
    }
}
