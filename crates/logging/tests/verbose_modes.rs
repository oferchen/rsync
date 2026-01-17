//! Integration tests for rsync verbose mode mappings.
//!
//! These tests verify that VerbosityConfig::from_verbose_level correctly
//! maps rsync's -v, -vv, -vvv flags to the appropriate info and debug
//! flag combinations.
//!
//! Reference: rsync 3.4.1 options.c for verbosity level to flag mapping.

use logging::{DebugFlag, InfoFlag, VerbosityConfig, debug_gte, drain_events, info_gte, init};

// ============================================================================
// Verbose Level 0 (No -v flags)
// ============================================================================

/// Verifies level 0 enables minimal output (only nonreg).
/// This matches rsync's behavior with no verbosity flags.
#[test]
fn verbose_level_0_minimal_output() {
    let config = VerbosityConfig::from_verbose_level(0);
    init(config);

    // Only nonreg is enabled at level 0
    assert!(info_gte(InfoFlag::Nonreg, 1));

    // All other info flags are off
    assert!(!info_gte(InfoFlag::Copy, 1));
    assert!(!info_gte(InfoFlag::Del, 1));
    assert!(!info_gte(InfoFlag::Flist, 1));
    assert!(!info_gte(InfoFlag::Misc, 1));
    assert!(!info_gte(InfoFlag::Name, 1));
    assert!(!info_gte(InfoFlag::Stats, 1));

    // All debug flags are off
    assert!(!debug_gte(DebugFlag::Bind, 1));
    assert!(!debug_gte(DebugFlag::Recv, 1));
    assert!(!debug_gte(DebugFlag::Send, 1));
}

// ============================================================================
// Verbose Level 1 (-v)
// ============================================================================

/// Verifies level 1 enables basic file listing output.
/// This matches rsync -v behavior.
#[test]
fn verbose_level_1_basic_output() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);

    // Basic info flags enabled at level 1
    assert!(info_gte(InfoFlag::Nonreg, 1));
    assert!(info_gte(InfoFlag::Copy, 1));
    assert!(info_gte(InfoFlag::Del, 1));
    assert!(info_gte(InfoFlag::Flist, 1));
    assert!(info_gte(InfoFlag::Misc, 1));
    assert!(info_gte(InfoFlag::Name, 1));
    assert!(info_gte(InfoFlag::Stats, 1));
    assert!(info_gte(InfoFlag::Symsafe, 1));

    // Enhanced levels not yet available
    assert!(!info_gte(InfoFlag::Misc, 2));
    assert!(!info_gte(InfoFlag::Name, 2));

    // Debug flags still off
    assert!(!debug_gte(DebugFlag::Bind, 1));
    assert!(!debug_gte(DebugFlag::Recv, 1));
}

/// Verifies level 1 shows file names but not extra details.
#[test]
fn verbose_level_1_name_output() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    // Can output file names at level 1
    assert!(info_gte(InfoFlag::Name, 1));

    // But not itemized changes (level 2)
    assert!(!info_gte(InfoFlag::Name, 2));
}

// ============================================================================
// Verbose Level 2 (-vv)
// ============================================================================

/// Verifies level 2 enables enhanced info and basic debug output.
/// This matches rsync -vv behavior.
#[test]
fn verbose_level_2_enhanced_output() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);

    // Enhanced info levels
    assert!(info_gte(InfoFlag::Misc, 2));
    assert!(info_gte(InfoFlag::Name, 2));
    assert!(info_gte(InfoFlag::Backup, 2));
    assert!(info_gte(InfoFlag::Mount, 2));
    assert!(info_gte(InfoFlag::Remove, 2));
    assert!(info_gte(InfoFlag::Skip, 2));

    // Basic debug flags enabled
    assert!(debug_gte(DebugFlag::Bind, 1));
    assert!(debug_gte(DebugFlag::Cmd, 1));
    assert!(debug_gte(DebugFlag::Connect, 1));
    assert!(debug_gte(DebugFlag::Del, 1));
    assert!(debug_gte(DebugFlag::Deltasum, 1));
    assert!(debug_gte(DebugFlag::Dup, 1));
    assert!(debug_gte(DebugFlag::Filter, 1));
    assert!(debug_gte(DebugFlag::Flist, 1));
    assert!(debug_gte(DebugFlag::Iconv, 1));
}

/// Verifies level 2 is suitable for itemize-changes output.
#[test]
fn verbose_level_2_itemize_changes() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);

    // Itemize changes requires name level 2
    assert!(info_gte(InfoFlag::Name, 2));
}

// ============================================================================
// Verbose Level 3 (-vvv)
// ============================================================================

/// Verifies level 3 enables detailed debug output.
/// This matches rsync -vvv behavior.
#[test]
fn verbose_level_3_detailed_debug() {
    let config = VerbosityConfig::from_verbose_level(3);
    init(config);

    // Enhanced debug levels
    assert!(debug_gte(DebugFlag::Connect, 2));
    assert!(debug_gte(DebugFlag::Del, 2));
    assert!(debug_gte(DebugFlag::Deltasum, 2));
    assert!(debug_gte(DebugFlag::Filter, 2));
    assert!(debug_gte(DebugFlag::Flist, 2));

    // Additional debug flags
    assert!(debug_gte(DebugFlag::Acl, 1));
    assert!(debug_gte(DebugFlag::Backup, 1));
    assert!(debug_gte(DebugFlag::Fuzzy, 1));
    assert!(debug_gte(DebugFlag::Genr, 1));
    assert!(debug_gte(DebugFlag::Own, 1));
    assert!(debug_gte(DebugFlag::Recv, 1));
    assert!(debug_gte(DebugFlag::Send, 1));
    assert!(debug_gte(DebugFlag::Time, 1));
    assert!(debug_gte(DebugFlag::Exit, 2));
}

// ============================================================================
// Verbose Level 4 (-vvvv)
// ============================================================================

/// Verifies level 4 enables highly detailed debug output.
#[test]
fn verbose_level_4_highly_detailed() {
    let config = VerbosityConfig::from_verbose_level(4);
    init(config);

    // Further enhanced debug levels
    assert!(debug_gte(DebugFlag::Cmd, 2));
    assert!(debug_gte(DebugFlag::Del, 3));
    assert!(debug_gte(DebugFlag::Deltasum, 3));
    assert!(debug_gte(DebugFlag::Flist, 3));
    assert!(debug_gte(DebugFlag::Iconv, 2));
    assert!(debug_gte(DebugFlag::Own, 2));
    assert!(debug_gte(DebugFlag::Time, 2));
    assert!(debug_gte(DebugFlag::Exit, 3));
    assert!(debug_gte(DebugFlag::Proto, 2));
}

// ============================================================================
// Verbose Level 5+ (-vvvvv or more)
// ============================================================================

/// Verifies level 5+ enables maximum debug output.
#[test]
fn verbose_level_5_maximum_output() {
    let config = VerbosityConfig::from_verbose_level(5);
    init(config);

    // Maximum debug levels
    assert!(debug_gte(DebugFlag::Deltasum, 4));
    assert!(debug_gte(DebugFlag::Flist, 4));

    // Additional debug flags at level 5
    assert!(debug_gte(DebugFlag::Chdir, 1));
    assert!(debug_gte(DebugFlag::Hash, 1));
    assert!(debug_gte(DebugFlag::Hlink, 1));
}

/// Verifies levels above 5 behave same as level 5.
#[test]
fn verbose_level_above_5_same_as_5() {
    let config5 = VerbosityConfig::from_verbose_level(5);
    let config10 = VerbosityConfig::from_verbose_level(10);
    let config255 = VerbosityConfig::from_verbose_level(255);

    // All should have the same debug levels
    assert_eq!(config5.debug.deltasum, config10.debug.deltasum);
    assert_eq!(config5.debug.flist, config10.debug.flist);
    assert_eq!(config5.debug.chdir, config10.debug.chdir);
    assert_eq!(config5.debug.hash, config10.debug.hash);
    assert_eq!(config5.debug.hlink, config10.debug.hlink);

    assert_eq!(config5.debug.deltasum, config255.debug.deltasum);
    assert_eq!(config5.debug.flist, config255.debug.flist);
}

// ============================================================================
// Progressive Enhancement Tests
// ============================================================================

/// Verifies each level includes previous level's capabilities.
#[test]
fn verbose_levels_are_progressive() {
    // Check that higher levels include lower level capabilities
    for level in 1..=5 {
        let config = VerbosityConfig::from_verbose_level(level);
        init(config);

        // All levels 1+ have basic info
        assert!(info_gte(InfoFlag::Copy, 1));
        assert!(info_gte(InfoFlag::Name, 1));
        assert!(info_gte(InfoFlag::Stats, 1));
    }
}

/// Verifies debug output only appears at level 2+.
#[test]
fn debug_output_starts_at_level_2() {
    let config0 = VerbosityConfig::from_verbose_level(0);
    let config1 = VerbosityConfig::from_verbose_level(1);
    let config2 = VerbosityConfig::from_verbose_level(2);

    assert_eq!(config0.debug.bind, 0);
    assert_eq!(config1.debug.bind, 0);
    assert!(config2.debug.bind >= 1);
}

// ============================================================================
// Specific Flag Mapping Tests
// ============================================================================

/// Verifies deltasum flag levels match rsync behavior.
/// Deltasum is used for delta-transfer algorithm debugging.
#[test]
fn deltasum_flag_levels() {
    assert_eq!(VerbosityConfig::from_verbose_level(0).debug.deltasum, 0);
    assert_eq!(VerbosityConfig::from_verbose_level(1).debug.deltasum, 0);
    assert_eq!(VerbosityConfig::from_verbose_level(2).debug.deltasum, 1);
    assert_eq!(VerbosityConfig::from_verbose_level(3).debug.deltasum, 2);
    assert_eq!(VerbosityConfig::from_verbose_level(4).debug.deltasum, 3);
    assert_eq!(VerbosityConfig::from_verbose_level(5).debug.deltasum, 4);
}

/// Verifies flist flag levels match rsync behavior.
/// Flist is used for file list debugging.
#[test]
fn flist_flag_levels() {
    assert_eq!(VerbosityConfig::from_verbose_level(0).debug.flist, 0);
    assert_eq!(VerbosityConfig::from_verbose_level(1).debug.flist, 0);
    assert_eq!(VerbosityConfig::from_verbose_level(2).debug.flist, 1);
    assert_eq!(VerbosityConfig::from_verbose_level(3).debug.flist, 2);
    assert_eq!(VerbosityConfig::from_verbose_level(4).debug.flist, 3);
    assert_eq!(VerbosityConfig::from_verbose_level(5).debug.flist, 4);
}

/// Verifies name flag levels match rsync behavior.
/// Name level 1 shows filenames, level 2 shows itemized changes.
#[test]
fn name_flag_levels() {
    assert_eq!(VerbosityConfig::from_verbose_level(0).info.name, 0);
    assert_eq!(VerbosityConfig::from_verbose_level(1).info.name, 1);
    assert_eq!(VerbosityConfig::from_verbose_level(2).info.name, 2);
    assert_eq!(VerbosityConfig::from_verbose_level(3).info.name, 2);
}

// ============================================================================
// Config Cloning and Modification
// ============================================================================

/// Verifies from_verbose_level produces clonable config.
#[test]
fn verbose_config_is_clonable() {
    let config = VerbosityConfig::from_verbose_level(3);
    let cloned = config.clone();

    assert_eq!(config.info.name, cloned.info.name);
    assert_eq!(config.debug.recv, cloned.debug.recv);
    assert_eq!(config.debug.flist, cloned.debug.flist);
}

/// Verifies config can be modified after creation.
#[test]
fn verbose_config_is_modifiable() {
    let mut config = VerbosityConfig::from_verbose_level(1);

    // Override specific flags
    config.debug.recv = 5;
    config.info.progress = 2;

    init(config);

    // Original verbose level 1 settings should remain
    assert!(info_gte(InfoFlag::Name, 1));

    // But overridden values should be applied
    assert!(debug_gte(DebugFlag::Recv, 5));
    assert!(info_gte(InfoFlag::Progress, 2));
}
