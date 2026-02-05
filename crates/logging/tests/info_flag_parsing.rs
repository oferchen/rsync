//! Integration tests for --info flag parsing.
//!
//! These tests verify that info flag parsing from command-line style arguments
//! works correctly. The --info flag accepts comma-separated flag specifications
//! like: --info=copy,del,name2,stats
//!
//! Special keywords ALL and NONE are also supported for convenience.
//!
//! Reference: rsync 3.4.1 options.c for --info flag parsing behavior.

use logging::{InfoFlag, VerbosityConfig, info_gte, init};

// ============================================================================
// Helper Functions
// ============================================================================

/// Parse an --info=FLAGS string and apply to config.
/// Format: "copy,del,name2" or "copy2,del,stats"
fn parse_info_flags(config: &mut VerbosityConfig, flags_str: &str) -> Result<(), String> {
    if flags_str.is_empty() {
        return Ok(());
    }

    // Handle special keywords
    if flags_str.eq_ignore_ascii_case("ALL") {
        config.info.set_all(1);
        return Ok(());
    }

    if flags_str.eq_ignore_ascii_case("NONE") {
        config.info.set_all(0);
        return Ok(());
    }

    // Parse comma-separated flags
    for token in flags_str.split(',') {
        let token = token.trim();
        if !token.is_empty() {
            config.apply_info_flag(token)?;
        }
    }

    Ok(())
}

// ============================================================================
// Single Flag Parsing Tests
// ============================================================================

/// Verifies single flag without level defaults to level 1.
#[test]
fn single_flag_no_level() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "copy").unwrap();

    assert_eq!(config.info.copy, 1);
    assert_eq!(config.info.del, 0);
    assert_eq!(config.info.name, 0);
}

/// Verifies single flag with explicit level 1.
#[test]
fn single_flag_level_1() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "copy1").unwrap();

    assert_eq!(config.info.copy, 1);
}

/// Verifies single flag with level 2.
#[test]
fn single_flag_level_2() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "name2").unwrap();

    assert_eq!(config.info.name, 2);
}

/// Verifies single flag with higher level.
#[test]
fn single_flag_level_5() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "stats5").unwrap();

    assert_eq!(config.info.stats, 5);
}

/// Verifies flag with level 0 disables it.
#[test]
fn single_flag_level_0() {
    let mut config = VerbosityConfig::default();
    config.info.copy = 2;
    parse_info_flags(&mut config, "copy0").unwrap();

    assert_eq!(config.info.copy, 0);
}

// ============================================================================
// Multiple Flag Parsing Tests
// ============================================================================

/// Verifies two flags separated by comma.
#[test]
fn two_flags_no_levels() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "copy,del").unwrap();

    assert_eq!(config.info.copy, 1);
    assert_eq!(config.info.del, 1);
    assert_eq!(config.info.name, 0);
}

/// Verifies multiple flags with mixed levels.
#[test]
fn multiple_flags_mixed_levels() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "copy,del2,name3").unwrap();

    assert_eq!(config.info.copy, 1);
    assert_eq!(config.info.del, 2);
    assert_eq!(config.info.name, 3);
}

/// Verifies all info flags can be set together.
#[test]
fn all_info_flags_together() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(
        &mut config,
        "backup,copy,del,flist,misc,mount,name,nonreg,progress,remove,skip,stats,symsafe",
    )
    .unwrap();

    assert_eq!(config.info.backup, 1);
    assert_eq!(config.info.copy, 1);
    assert_eq!(config.info.del, 1);
    assert_eq!(config.info.flist, 1);
    assert_eq!(config.info.misc, 1);
    assert_eq!(config.info.mount, 1);
    assert_eq!(config.info.name, 1);
    assert_eq!(config.info.nonreg, 1);
    assert_eq!(config.info.progress, 1);
    assert_eq!(config.info.remove, 1);
    assert_eq!(config.info.skip, 1);
    assert_eq!(config.info.stats, 1);
    assert_eq!(config.info.symsafe, 1);
}

/// Verifies multiple flags with varying levels.
#[test]
fn complex_flag_combination() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "copy2,del,name3,stats,misc2").unwrap();

    assert_eq!(config.info.copy, 2);
    assert_eq!(config.info.del, 1);
    assert_eq!(config.info.name, 3);
    assert_eq!(config.info.stats, 1);
    assert_eq!(config.info.misc, 2);
}

/// Verifies later flag overwrites earlier one.
#[test]
fn duplicate_flag_uses_last_value() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "copy,copy2,copy3").unwrap();

    assert_eq!(config.info.copy, 3);
}

// ============================================================================
// Whitespace Handling Tests
// ============================================================================

/// Verifies spaces around commas are handled.
#[test]
fn flags_with_spaces() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "copy, del, name2").unwrap();

    assert_eq!(config.info.copy, 1);
    assert_eq!(config.info.del, 1);
    assert_eq!(config.info.name, 2);
}

/// Verifies extra spaces are ignored.
#[test]
fn flags_with_extra_spaces() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "  copy  ,  del  ").unwrap();

    assert_eq!(config.info.copy, 1);
    assert_eq!(config.info.del, 1);
}

/// Verifies trailing comma is handled.
#[test]
fn flags_with_trailing_comma() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "copy,del,").unwrap();

    assert_eq!(config.info.copy, 1);
    assert_eq!(config.info.del, 1);
}

/// Verifies leading comma is handled.
#[test]
fn flags_with_leading_comma() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, ",copy,del").unwrap();

    assert_eq!(config.info.copy, 1);
    assert_eq!(config.info.del, 1);
}

/// Verifies empty string is valid (no-op).
#[test]
fn empty_flags_string() {
    let mut config = VerbosityConfig::default();
    config.info.copy = 2;
    parse_info_flags(&mut config, "").unwrap();

    // Config should be unchanged
    assert_eq!(config.info.copy, 2);
}

// ============================================================================
// ALL and NONE Keyword Tests
// ============================================================================

/// Verifies ALL sets all info flags to level 1.
#[test]
fn all_keyword_uppercase() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "ALL").unwrap();

    assert_eq!(config.info.backup, 1);
    assert_eq!(config.info.copy, 1);
    assert_eq!(config.info.del, 1);
    assert_eq!(config.info.flist, 1);
    assert_eq!(config.info.misc, 1);
    assert_eq!(config.info.mount, 1);
    assert_eq!(config.info.name, 1);
    assert_eq!(config.info.nonreg, 1);
    assert_eq!(config.info.progress, 1);
    assert_eq!(config.info.remove, 1);
    assert_eq!(config.info.skip, 1);
    assert_eq!(config.info.stats, 1);
    assert_eq!(config.info.symsafe, 1);
}

/// Verifies ALL is case-insensitive.
#[test]
fn all_keyword_lowercase() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "all").unwrap();

    assert_eq!(config.info.copy, 1);
    assert_eq!(config.info.del, 1);
    assert_eq!(config.info.name, 1);
}

/// Verifies ALL with mixed case.
#[test]
fn all_keyword_mixed_case() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "All").unwrap();

    assert_eq!(config.info.copy, 1);
    assert_eq!(config.info.del, 1);
}

/// Verifies NONE sets all info flags to level 0.
#[test]
fn none_keyword_uppercase() {
    let mut config = VerbosityConfig::default();
    config.info.copy = 2;
    config.info.del = 3;
    config.info.name = 1;

    parse_info_flags(&mut config, "NONE").unwrap();

    assert_eq!(config.info.backup, 0);
    assert_eq!(config.info.copy, 0);
    assert_eq!(config.info.del, 0);
    assert_eq!(config.info.flist, 0);
    assert_eq!(config.info.misc, 0);
    assert_eq!(config.info.mount, 0);
    assert_eq!(config.info.name, 0);
    assert_eq!(config.info.nonreg, 0);
    assert_eq!(config.info.progress, 0);
    assert_eq!(config.info.remove, 0);
    assert_eq!(config.info.skip, 0);
    assert_eq!(config.info.stats, 0);
    assert_eq!(config.info.symsafe, 0);
}

/// Verifies NONE is case-insensitive.
#[test]
fn none_keyword_lowercase() {
    let mut config = VerbosityConfig::default();
    config.info.copy = 2;
    parse_info_flags(&mut config, "none").unwrap();

    assert_eq!(config.info.copy, 0);
    assert_eq!(config.info.del, 0);
}

/// Verifies NONE with mixed case.
#[test]
fn none_keyword_mixed_case() {
    let mut config = VerbosityConfig::default();
    config.info.copy = 2;
    parse_info_flags(&mut config, "None").unwrap();

    assert_eq!(config.info.copy, 0);
}

// ============================================================================
// Error Handling Tests
// ============================================================================

/// Verifies unknown flag returns error.
#[test]
fn unknown_flag_error() {
    let mut config = VerbosityConfig::default();
    let result = parse_info_flags(&mut config, "invalid");

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("unknown info flag: invalid"));
}

/// Verifies unknown flag in list returns error.
#[test]
fn unknown_flag_in_list_error() {
    let mut config = VerbosityConfig::default();
    let result = parse_info_flags(&mut config, "copy,invalid,del");

    assert!(result.is_err());
    assert!(result.unwrap_err().contains("unknown info flag"));
}

/// Verifies partially valid list doesn't change config before error.
#[test]
fn error_stops_processing() {
    let mut config = VerbosityConfig::default();
    let result = parse_info_flags(&mut config, "copy,invalid,del");

    assert!(result.is_err());
    // First flag should have been applied before error
    assert_eq!(config.info.copy, 1);
    // Flag after error should not be applied
    assert_eq!(config.info.del, 0);
}

/// Verifies invalid level number returns error.
#[test]
fn invalid_level_number_error() {
    let mut config = VerbosityConfig::default();
    let result = parse_info_flags(&mut config, "copy999");

    // Should fail since 999 is out of u8 range (0-255)
    assert!(result.is_err());
}

/// Verifies malformed flag returns error.
#[test]
fn malformed_flag_error() {
    let mut config = VerbosityConfig::default();
    let result = parse_info_flags(&mut config, "copy2extra");

    assert!(result.is_err());
}

// ============================================================================
// Realistic Usage Pattern Tests
// ============================================================================

/// Verifies typical rsync --info=name,del pattern.
#[test]
fn typical_rsync_pattern_1() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "name,del").unwrap();
    init(config);

    assert!(info_gte(InfoFlag::Name, 1));
    assert!(info_gte(InfoFlag::Del, 1));
    assert!(!info_gte(InfoFlag::Copy, 1));
}

/// Verifies itemize-changes equivalent pattern.
#[test]
fn typical_rsync_pattern_itemize() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "name2,del,copy").unwrap();
    init(config);

    assert!(info_gte(InfoFlag::Name, 2));
    assert!(info_gte(InfoFlag::Del, 1));
    assert!(info_gte(InfoFlag::Copy, 1));
}

/// Verifies progress reporting pattern.
#[test]
fn typical_rsync_pattern_progress() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "progress,stats,name").unwrap();
    init(config);

    assert!(info_gte(InfoFlag::Progress, 1));
    assert!(info_gte(InfoFlag::Stats, 1));
    assert!(info_gte(InfoFlag::Name, 1));
}

/// Verifies verbose debugging pattern.
#[test]
fn typical_rsync_pattern_verbose() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "flist2,del2,copy,misc2").unwrap();
    init(config);

    assert!(info_gte(InfoFlag::Flist, 2));
    assert!(info_gte(InfoFlag::Del, 2));
    assert!(info_gte(InfoFlag::Copy, 1));
    assert!(info_gte(InfoFlag::Misc, 2));
}

// ============================================================================
// Integration with Verbose Levels Tests
// ============================================================================

/// Verifies --info flags can override verbose level settings.
#[test]
fn info_flags_override_verbose_level() {
    let mut config = VerbosityConfig::from_verbose_level(1);
    // Level 1 sets copy to 1
    assert_eq!(config.info.copy, 1);

    // Override with --info
    parse_info_flags(&mut config, "copy3").unwrap();
    assert_eq!(config.info.copy, 3);
}

/// Verifies --info flags can disable verbose level settings.
#[test]
fn info_flags_disable_verbose_level() {
    let mut config = VerbosityConfig::from_verbose_level(2);
    // Level 2 sets name to 2
    assert_eq!(config.info.name, 2);

    // Disable with --info=name0
    parse_info_flags(&mut config, "name0").unwrap();
    assert_eq!(config.info.name, 0);
}

/// Verifies selective override of verbose level.
#[test]
fn selective_info_override() {
    let mut config = VerbosityConfig::from_verbose_level(1);

    // Override only some flags
    parse_info_flags(&mut config, "name2,misc0").unwrap();

    // Overridden flags
    assert_eq!(config.info.name, 2);
    assert_eq!(config.info.misc, 0);

    // Unchanged flags from verbose level 1
    assert_eq!(config.info.copy, 1);
    assert_eq!(config.info.del, 1);
}

// ============================================================================
// Edge Case Tests
// ============================================================================

/// Verifies consecutive commas are handled.
#[test]
fn consecutive_commas() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "copy,,del").unwrap();

    assert_eq!(config.info.copy, 1);
    assert_eq!(config.info.del, 1);
}

/// Verifies only commas is handled (no-op).
#[test]
fn only_commas() {
    let mut config = VerbosityConfig::default();
    config.info.copy = 2;
    parse_info_flags(&mut config, ",,,").unwrap();

    // Config unchanged
    assert_eq!(config.info.copy, 2);
}

/// Verifies flag name with number in name doesn't confuse parser.
/// Note: None of the current info flags have numbers in their names,
/// but this tests the parsing logic robustness.
#[test]
fn flag_level_parsing_robustness() {
    let mut config = VerbosityConfig::default();
    // "copy2" should be parsed as flag "copy" with level 2
    parse_info_flags(&mut config, "copy2").unwrap();
    assert_eq!(config.info.copy, 2);

    // "copy23" should be parsed as flag "copy" with level 23
    parse_info_flags(&mut config, "copy23").unwrap();
    assert_eq!(config.info.copy, 23);
}

/// Verifies maximum u8 level value (255).
#[test]
fn max_level_value() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "copy255").unwrap();
    assert_eq!(config.info.copy, 255);
}

/// Verifies zero level explicitly.
#[test]
fn zero_level_explicit() {
    let mut config = VerbosityConfig::default();
    config.info.copy = 5;
    parse_info_flags(&mut config, "copy0").unwrap();
    assert_eq!(config.info.copy, 0);
}

// ============================================================================
// Case Sensitivity Tests (flags are case-sensitive)
// ============================================================================

/// Verifies flag names are case-sensitive (should fail).
#[test]
fn flag_name_case_sensitive() {
    let mut config = VerbosityConfig::default();
    let result = parse_info_flags(&mut config, "COPY");

    // Flag names are case-sensitive, so "COPY" should be unknown
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("unknown info flag"));
}

/// Verifies proper case works.
#[test]
fn flag_name_lowercase() {
    let mut config = VerbosityConfig::default();
    parse_info_flags(&mut config, "copy").unwrap();
    assert_eq!(config.info.copy, 1);
}
