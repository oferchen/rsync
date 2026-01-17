//! Integration tests for log level filtering.
//!
//! These tests verify that verbosity configuration correctly filters
//! which log messages are emitted based on their level thresholds.
//! This is core to rsync's -v and --debug flag handling.
//!
//! Reference: rsync 3.4.1 options.c for verbosity level parsing.

use logging::{
    info_log, debug_log, InfoFlag, DebugFlag, VerbosityConfig,
    drain_events, init, info_gte, debug_gte, apply_info_flag, apply_debug_flag,
};

// ============================================================================
// Level Comparison Tests
// ============================================================================

/// Verifies info_gte returns true for levels at or below configured.
#[test]
fn info_gte_returns_true_for_sufficient_level() {
    let mut config = VerbosityConfig::default();
    config.info.copy = 3;
    init(config);

    assert!(info_gte(InfoFlag::Copy, 0));
    assert!(info_gte(InfoFlag::Copy, 1));
    assert!(info_gte(InfoFlag::Copy, 2));
    assert!(info_gte(InfoFlag::Copy, 3));
}

/// Verifies info_gte returns false for levels above configured.
#[test]
fn info_gte_returns_false_for_insufficient_level() {
    let mut config = VerbosityConfig::default();
    config.info.copy = 3;
    init(config);

    assert!(!info_gte(InfoFlag::Copy, 4));
    assert!(!info_gte(InfoFlag::Copy, 5));
    assert!(!info_gte(InfoFlag::Copy, 100));
}

/// Verifies debug_gte returns true for levels at or below configured.
#[test]
fn debug_gte_returns_true_for_sufficient_level() {
    let mut config = VerbosityConfig::default();
    config.debug.recv = 4;
    init(config);

    assert!(debug_gte(DebugFlag::Recv, 0));
    assert!(debug_gte(DebugFlag::Recv, 1));
    assert!(debug_gte(DebugFlag::Recv, 2));
    assert!(debug_gte(DebugFlag::Recv, 3));
    assert!(debug_gte(DebugFlag::Recv, 4));
}

/// Verifies debug_gte returns false for levels above configured.
#[test]
fn debug_gte_returns_false_for_insufficient_level() {
    let mut config = VerbosityConfig::default();
    config.debug.recv = 4;
    init(config);

    assert!(!debug_gte(DebugFlag::Recv, 5));
    assert!(!debug_gte(DebugFlag::Recv, 6));
    assert!(!debug_gte(DebugFlag::Recv, 255));
}

// ============================================================================
// Flag Independence Tests
// ============================================================================

/// Verifies different info flags have independent levels.
#[test]
fn info_flags_have_independent_levels() {
    let mut config = VerbosityConfig::default();
    config.info.copy = 1;
    config.info.del = 2;
    config.info.name = 3;
    init(config);

    assert!(info_gte(InfoFlag::Copy, 1));
    assert!(!info_gte(InfoFlag::Copy, 2));

    assert!(info_gte(InfoFlag::Del, 2));
    assert!(!info_gte(InfoFlag::Del, 3));

    assert!(info_gte(InfoFlag::Name, 3));
    assert!(!info_gte(InfoFlag::Name, 4));
}

/// Verifies different debug flags have independent levels.
#[test]
fn debug_flags_have_independent_levels() {
    let mut config = VerbosityConfig::default();
    config.debug.recv = 1;
    config.debug.send = 2;
    config.debug.flist = 3;
    init(config);

    assert!(debug_gte(DebugFlag::Recv, 1));
    assert!(!debug_gte(DebugFlag::Recv, 2));

    assert!(debug_gte(DebugFlag::Send, 2));
    assert!(!debug_gte(DebugFlag::Send, 3));

    assert!(debug_gte(DebugFlag::Flist, 3));
    assert!(!debug_gte(DebugFlag::Flist, 4));
}

// ============================================================================
// Runtime Flag Application Tests
// ============================================================================

/// Verifies apply_info_flag updates configuration.
#[test]
fn apply_info_flag_updates_config() {
    init(VerbosityConfig::default());

    assert!(!info_gte(InfoFlag::Copy, 1));

    apply_info_flag("copy2").unwrap();

    assert!(info_gte(InfoFlag::Copy, 1));
    assert!(info_gte(InfoFlag::Copy, 2));
    assert!(!info_gte(InfoFlag::Copy, 3));
}

/// Verifies apply_debug_flag updates configuration.
#[test]
fn apply_debug_flag_updates_config() {
    init(VerbosityConfig::default());

    assert!(!debug_gte(DebugFlag::Io, 1));

    apply_debug_flag("io3").unwrap();

    assert!(debug_gte(DebugFlag::Io, 1));
    assert!(debug_gte(DebugFlag::Io, 2));
    assert!(debug_gte(DebugFlag::Io, 3));
    assert!(!debug_gte(DebugFlag::Io, 4));
}

/// Verifies apply_info_flag with no level defaults to 1.
#[test]
fn apply_info_flag_default_level() {
    init(VerbosityConfig::default());

    apply_info_flag("stats").unwrap();

    assert!(info_gte(InfoFlag::Stats, 1));
    assert!(!info_gte(InfoFlag::Stats, 2));
}

/// Verifies apply_debug_flag with no level defaults to 1.
#[test]
fn apply_debug_flag_default_level() {
    init(VerbosityConfig::default());

    apply_debug_flag("hash").unwrap();

    assert!(debug_gte(DebugFlag::Hash, 1));
    assert!(!debug_gte(DebugFlag::Hash, 2));
}

/// Verifies apply_info_flag rejects unknown flags.
#[test]
fn apply_info_flag_rejects_unknown() {
    init(VerbosityConfig::default());

    let result = apply_info_flag("unknown_flag");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("unknown info flag"));
}

/// Verifies apply_debug_flag rejects unknown flags.
#[test]
fn apply_debug_flag_rejects_unknown() {
    init(VerbosityConfig::default());

    let result = apply_debug_flag("not_a_flag");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("unknown debug flag"));
}

// ============================================================================
// Filtering Effect on Log Output
// ============================================================================

/// Verifies filtering prevents log emission.
#[test]
fn filtering_prevents_log_emission() {
    let mut config = VerbosityConfig::default();
    config.info.name = 1;
    config.debug.recv = 1;
    init(config);
    drain_events();

    // These should emit
    info_log!(Name, 1, "visible");
    debug_log!(Recv, 1, "visible");

    // These should be filtered
    info_log!(Name, 2, "filtered");
    debug_log!(Recv, 2, "filtered");
    info_log!(Copy, 1, "different flag - filtered");
    debug_log!(Send, 1, "different flag - filtered");

    let events = drain_events();
    assert_eq!(events.len(), 2);
}

/// Verifies filtering with multiple flags.
#[test]
fn filtering_multiple_flags() {
    let mut config = VerbosityConfig::default();
    config.info.name = 2;
    config.info.copy = 1;
    config.info.del = 3;
    init(config);
    drain_events();

    info_log!(Name, 1, "name level 1 - visible");
    info_log!(Name, 2, "name level 2 - visible");
    info_log!(Name, 3, "name level 3 - filtered");

    info_log!(Copy, 1, "copy level 1 - visible");
    info_log!(Copy, 2, "copy level 2 - filtered");

    info_log!(Del, 3, "del level 3 - visible");
    info_log!(Del, 4, "del level 4 - filtered");

    let events = drain_events();
    assert_eq!(events.len(), 4);
}

// ============================================================================
// Level Zero Behavior
// ============================================================================

/// Verifies level 0 check always passes if flag is at least 0.
#[test]
fn level_zero_always_passes() {
    init(VerbosityConfig::default());

    // Even with default (0) config, level 0 checks pass
    assert!(info_gte(InfoFlag::Name, 0));
    assert!(debug_gte(DebugFlag::Recv, 0));
}

/// Verifies level 0 logs are emitted with default config.
#[test]
fn level_zero_logs_emit() {
    init(VerbosityConfig::default());
    drain_events();

    info_log!(Name, 0, "level zero info");
    debug_log!(Recv, 0, "level zero debug");

    let events = drain_events();
    assert_eq!(events.len(), 2);
}

// ============================================================================
// Maximum Level Tests
// ============================================================================

/// Verifies u8 maximum level is handled correctly.
#[test]
fn max_level_handling() {
    let mut config = VerbosityConfig::default();
    config.info.stats = 255;
    config.debug.deltasum = 255;
    init(config);

    assert!(info_gte(InfoFlag::Stats, 255));
    assert!(debug_gte(DebugFlag::Deltasum, 255));
}

/// Verifies level overflow doesn't occur.
#[test]
fn level_boundary_conditions() {
    let mut config = VerbosityConfig::default();
    config.info.copy = 254;
    init(config);

    assert!(info_gte(InfoFlag::Copy, 254));
    assert!(!info_gte(InfoFlag::Copy, 255));
}

// ============================================================================
// Config Modification Tests
// ============================================================================

/// Verifies multiple flag applications accumulate.
#[test]
fn multiple_flag_applications() {
    init(VerbosityConfig::default());

    apply_info_flag("copy").unwrap();
    apply_info_flag("del").unwrap();
    apply_debug_flag("recv").unwrap();

    assert!(info_gte(InfoFlag::Copy, 1));
    assert!(info_gte(InfoFlag::Del, 1));
    assert!(debug_gte(DebugFlag::Recv, 1));
}

/// Verifies later flag application overwrites earlier.
#[test]
fn flag_application_overwrites() {
    init(VerbosityConfig::default());

    apply_info_flag("copy").unwrap();
    assert!(info_gte(InfoFlag::Copy, 1));
    assert!(!info_gte(InfoFlag::Copy, 2));

    apply_info_flag("copy3").unwrap();
    assert!(info_gte(InfoFlag::Copy, 3));
}

/// Verifies reinit completely replaces config.
#[test]
fn reinit_replaces_config() {
    let mut config1 = VerbosityConfig::default();
    config1.info.copy = 5;
    config1.debug.recv = 5;
    init(config1);

    assert!(info_gte(InfoFlag::Copy, 5));
    assert!(debug_gte(DebugFlag::Recv, 5));

    // Reinit with default
    init(VerbosityConfig::default());

    assert!(!info_gte(InfoFlag::Copy, 1));
    assert!(!debug_gte(DebugFlag::Recv, 1));
}
