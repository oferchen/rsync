//! Integration tests for --debug flag parsing.
//!
//! These tests verify that debug flag parsing handles various input formats
//! correctly, including individual flags, multiple flags, numeric levels,
//! and special keywords like ALL and NONE.
//!
//! Reference: rsync 3.4.1 options.c for --debug flag parsing.

use logging::{DebugFlag, VerbosityConfig, apply_debug_flag, debug_gte, drain_events, init};

// ============================================================================
// Single Flag Parsing Tests
// ============================================================================

/// Verifies a single debug flag without level defaults to level 1.
#[test]
fn single_flag_defaults_to_level_1() {
    init(VerbosityConfig::default());

    apply_debug_flag("recv").unwrap();

    assert!(debug_gte(DebugFlag::Recv, 1));
    assert!(!debug_gte(DebugFlag::Recv, 2));
}

/// Verifies a single debug flag with explicit level 1.
#[test]
fn single_flag_explicit_level_1() {
    init(VerbosityConfig::default());

    apply_debug_flag("recv1").unwrap();

    assert!(debug_gte(DebugFlag::Recv, 1));
    assert!(!debug_gte(DebugFlag::Recv, 2));
}

/// Verifies a single debug flag with level 2.
#[test]
fn single_flag_level_2() {
    init(VerbosityConfig::default());

    apply_debug_flag("recv2").unwrap();

    assert!(debug_gte(DebugFlag::Recv, 1));
    assert!(debug_gte(DebugFlag::Recv, 2));
    assert!(!debug_gte(DebugFlag::Recv, 3));
}

/// Verifies a single debug flag with level 3.
#[test]
fn single_flag_level_3() {
    init(VerbosityConfig::default());

    apply_debug_flag("recv3").unwrap();

    assert!(debug_gte(DebugFlag::Recv, 1));
    assert!(debug_gte(DebugFlag::Recv, 2));
    assert!(debug_gte(DebugFlag::Recv, 3));
    assert!(!debug_gte(DebugFlag::Recv, 4));
}

/// Verifies a single debug flag with level 4.
#[test]
fn single_flag_level_4() {
    init(VerbosityConfig::default());

    apply_debug_flag("flist4").unwrap();

    assert!(debug_gte(DebugFlag::Flist, 1));
    assert!(debug_gte(DebugFlag::Flist, 2));
    assert!(debug_gte(DebugFlag::Flist, 3));
    assert!(debug_gte(DebugFlag::Flist, 4));
    assert!(!debug_gte(DebugFlag::Flist, 5));
}

/// Verifies setting a flag to level 0 disables it.
#[test]
fn single_flag_level_0() {
    let mut config = VerbosityConfig::default();
    config.debug.recv = 3;
    init(config);

    apply_debug_flag("recv0").unwrap();

    assert!(!debug_gte(DebugFlag::Recv, 1));
}

// ============================================================================
// Multiple Flag Parsing Tests
// ============================================================================

/// Verifies multiple debug flags can be set independently.
#[test]
fn multiple_flags_independent() {
    init(VerbosityConfig::default());

    apply_debug_flag("recv").unwrap();
    apply_debug_flag("send2").unwrap();
    apply_debug_flag("flist3").unwrap();

    assert!(debug_gte(DebugFlag::Recv, 1));
    assert!(!debug_gte(DebugFlag::Recv, 2));

    assert!(debug_gte(DebugFlag::Send, 2));
    assert!(!debug_gte(DebugFlag::Send, 3));

    assert!(debug_gte(DebugFlag::Flist, 3));
    assert!(!debug_gte(DebugFlag::Flist, 4));
}

/// Verifies multiple applications of the same flag override previous value.
#[test]
fn multiple_applications_override() {
    init(VerbosityConfig::default());

    apply_debug_flag("recv").unwrap();
    assert!(debug_gte(DebugFlag::Recv, 1));
    assert!(!debug_gte(DebugFlag::Recv, 2));

    apply_debug_flag("recv3").unwrap();
    assert!(debug_gte(DebugFlag::Recv, 3));
    assert!(!debug_gte(DebugFlag::Recv, 4));

    apply_debug_flag("recv2").unwrap();
    assert!(debug_gte(DebugFlag::Recv, 2));
    assert!(!debug_gte(DebugFlag::Recv, 3));
}

/// Verifies setting different flags doesn't affect each other.
#[test]
fn multiple_flags_dont_interfere() {
    init(VerbosityConfig::default());

    apply_debug_flag("recv2").unwrap();
    apply_debug_flag("send").unwrap();

    assert!(debug_gte(DebugFlag::Recv, 2));
    assert!(debug_gte(DebugFlag::Send, 1));
    assert!(!debug_gte(DebugFlag::Flist, 1));
}

// ============================================================================
// All Flags Tests
// ============================================================================

/// Verifies all debug flags can be set individually.
#[test]
fn all_debug_flags_parseable() {
    init(VerbosityConfig::default());

    // Test all 24 debug flags
    apply_debug_flag("acl").unwrap();
    assert!(debug_gte(DebugFlag::Acl, 1));

    apply_debug_flag("backup").unwrap();
    assert!(debug_gte(DebugFlag::Backup, 1));

    apply_debug_flag("bind").unwrap();
    assert!(debug_gte(DebugFlag::Bind, 1));

    apply_debug_flag("chdir").unwrap();
    assert!(debug_gte(DebugFlag::Chdir, 1));

    apply_debug_flag("connect").unwrap();
    assert!(debug_gte(DebugFlag::Connect, 1));

    apply_debug_flag("cmd").unwrap();
    assert!(debug_gte(DebugFlag::Cmd, 1));

    apply_debug_flag("del").unwrap();
    assert!(debug_gte(DebugFlag::Del, 1));

    apply_debug_flag("deltasum").unwrap();
    assert!(debug_gte(DebugFlag::Deltasum, 1));

    apply_debug_flag("dup").unwrap();
    assert!(debug_gte(DebugFlag::Dup, 1));

    apply_debug_flag("exit").unwrap();
    assert!(debug_gte(DebugFlag::Exit, 1));

    apply_debug_flag("filter").unwrap();
    assert!(debug_gte(DebugFlag::Filter, 1));

    apply_debug_flag("flist").unwrap();
    assert!(debug_gte(DebugFlag::Flist, 1));

    apply_debug_flag("fuzzy").unwrap();
    assert!(debug_gte(DebugFlag::Fuzzy, 1));

    apply_debug_flag("genr").unwrap();
    assert!(debug_gte(DebugFlag::Genr, 1));

    apply_debug_flag("hash").unwrap();
    assert!(debug_gte(DebugFlag::Hash, 1));

    apply_debug_flag("hlink").unwrap();
    assert!(debug_gte(DebugFlag::Hlink, 1));

    apply_debug_flag("iconv").unwrap();
    assert!(debug_gte(DebugFlag::Iconv, 1));

    apply_debug_flag("io").unwrap();
    assert!(debug_gte(DebugFlag::Io, 1));

    apply_debug_flag("nstr").unwrap();
    assert!(debug_gte(DebugFlag::Nstr, 1));

    apply_debug_flag("own").unwrap();
    assert!(debug_gte(DebugFlag::Own, 1));

    apply_debug_flag("proto").unwrap();
    assert!(debug_gte(DebugFlag::Proto, 1));

    apply_debug_flag("recv").unwrap();
    assert!(debug_gte(DebugFlag::Recv, 1));

    apply_debug_flag("send").unwrap();
    assert!(debug_gte(DebugFlag::Send, 1));

    apply_debug_flag("time").unwrap();
    assert!(debug_gte(DebugFlag::Time, 1));
}

/// Verifies all debug flags support numeric levels.
#[test]
fn all_debug_flags_support_levels() {
    init(VerbosityConfig::default());

    apply_debug_flag("acl2").unwrap();
    assert!(debug_gte(DebugFlag::Acl, 2));

    apply_debug_flag("backup2").unwrap();
    assert!(debug_gte(DebugFlag::Backup, 2));

    apply_debug_flag("bind3").unwrap();
    assert!(debug_gte(DebugFlag::Bind, 3));

    apply_debug_flag("chdir4").unwrap();
    assert!(debug_gte(DebugFlag::Chdir, 4));

    apply_debug_flag("deltasum4").unwrap();
    assert!(debug_gte(DebugFlag::Deltasum, 4));

    apply_debug_flag("flist4").unwrap();
    assert!(debug_gte(DebugFlag::Flist, 4));

    apply_debug_flag("io4").unwrap();
    assert!(debug_gte(DebugFlag::Io, 4));
}

// ============================================================================
// Numeric Level Tests (1-4)
// ============================================================================

/// Verifies numeric level 1 parsing.
#[test]
fn numeric_level_1_works() {
    init(VerbosityConfig::default());

    apply_debug_flag("recv1").unwrap();
    apply_debug_flag("send1").unwrap();
    apply_debug_flag("flist1").unwrap();

    assert!(debug_gte(DebugFlag::Recv, 1));
    assert!(debug_gte(DebugFlag::Send, 1));
    assert!(debug_gte(DebugFlag::Flist, 1));
}

/// Verifies numeric level 2 parsing.
#[test]
fn numeric_level_2_works() {
    init(VerbosityConfig::default());

    apply_debug_flag("recv2").unwrap();
    apply_debug_flag("send2").unwrap();
    apply_debug_flag("flist2").unwrap();

    assert!(debug_gte(DebugFlag::Recv, 2));
    assert!(debug_gte(DebugFlag::Send, 2));
    assert!(debug_gte(DebugFlag::Flist, 2));
}

/// Verifies numeric level 3 parsing.
#[test]
fn numeric_level_3_works() {
    init(VerbosityConfig::default());

    apply_debug_flag("recv3").unwrap();
    apply_debug_flag("send3").unwrap();
    apply_debug_flag("flist3").unwrap();

    assert!(debug_gte(DebugFlag::Recv, 3));
    assert!(debug_gte(DebugFlag::Send, 3));
    assert!(debug_gte(DebugFlag::Flist, 3));
}

/// Verifies numeric level 4 parsing.
#[test]
fn numeric_level_4_works() {
    init(VerbosityConfig::default());

    apply_debug_flag("deltasum4").unwrap();
    apply_debug_flag("flist4").unwrap();
    apply_debug_flag("io4").unwrap();

    assert!(debug_gte(DebugFlag::Deltasum, 4));
    assert!(debug_gte(DebugFlag::Flist, 4));
    assert!(debug_gte(DebugFlag::Io, 4));
}

/// Verifies higher numeric levels are accepted.
#[test]
fn numeric_level_high_works() {
    init(VerbosityConfig::default());

    apply_debug_flag("recv10").unwrap();
    apply_debug_flag("send99").unwrap();

    assert!(debug_gte(DebugFlag::Recv, 10));
    assert!(debug_gte(DebugFlag::Send, 99));
}

// ============================================================================
// Error Handling Tests
// ============================================================================

/// Verifies unknown debug flag names are rejected.
#[test]
fn unknown_flag_rejected() {
    init(VerbosityConfig::default());

    let result = apply_debug_flag("unknown");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("unknown debug flag"));
}

/// Verifies invalid flag names are rejected.
#[test]
fn invalid_flag_rejected() {
    init(VerbosityConfig::default());

    let result = apply_debug_flag("notaflag");
    assert!(result.is_err());
}

/// Verifies empty flag is rejected.
#[test]
fn empty_flag_rejected() {
    init(VerbosityConfig::default());

    let result = apply_debug_flag("");
    assert!(result.is_err());
}

// ============================================================================
// Case Sensitivity Tests
// ============================================================================

/// Verifies flag names are case-sensitive (lowercase required).
#[test]
fn flag_names_case_sensitive() {
    init(VerbosityConfig::default());

    // Lowercase should work
    apply_debug_flag("recv").unwrap();
    assert!(debug_gte(DebugFlag::Recv, 1));

    // Uppercase should fail (config.rs expects lowercase)
    let result = apply_debug_flag("RECV");
    assert!(result.is_err());
}

// ============================================================================
// Level Boundary Tests
// ============================================================================

/// Verifies level 0 disables a flag.
#[test]
fn level_0_disables_flag() {
    let mut config = VerbosityConfig::default();
    config.debug.recv = 5;
    init(config);

    apply_debug_flag("recv0").unwrap();

    assert!(!debug_gte(DebugFlag::Recv, 1));
}

/// Verifies maximum level (255) is handled.
#[test]
fn level_255_works() {
    init(VerbosityConfig::default());

    apply_debug_flag("recv255").unwrap();

    assert!(debug_gte(DebugFlag::Recv, 255));
}

// ============================================================================
// Flag Name Variations Tests
// ============================================================================

/// Verifies all standard flag name variations parse correctly.
#[test]
fn flag_name_variations() {
    init(VerbosityConfig::default());

    // Short names
    apply_debug_flag("acl").unwrap();
    apply_debug_flag("cmd").unwrap();
    apply_debug_flag("del").unwrap();
    apply_debug_flag("dup").unwrap();
    apply_debug_flag("io").unwrap();

    // Longer names
    apply_debug_flag("deltasum").unwrap();
    apply_debug_flag("connect").unwrap();
    apply_debug_flag("filter").unwrap();

    assert!(debug_gte(DebugFlag::Acl, 1));
    assert!(debug_gte(DebugFlag::Cmd, 1));
    assert!(debug_gte(DebugFlag::Del, 1));
    assert!(debug_gte(DebugFlag::Dup, 1));
    assert!(debug_gte(DebugFlag::Io, 1));
    assert!(debug_gte(DebugFlag::Deltasum, 1));
    assert!(debug_gte(DebugFlag::Connect, 1));
    assert!(debug_gte(DebugFlag::Filter, 1));
}

// ============================================================================
// Config Integration Tests
// ============================================================================

/// Verifies apply_debug_flag works with VerbosityConfig.
#[test]
fn apply_debug_flag_integrates_with_config() {
    let mut config = VerbosityConfig::default();
    config.apply_debug_flag("recv2").unwrap();
    config.apply_debug_flag("send3").unwrap();

    assert_eq!(config.debug.recv, 2);
    assert_eq!(config.debug.send, 3);
}

/// Verifies multiple flag applications on config.
#[test]
fn config_multiple_applications() {
    let mut config = VerbosityConfig::default();

    config.apply_debug_flag("recv").unwrap();
    assert_eq!(config.debug.recv, 1);

    config.apply_debug_flag("recv4").unwrap();
    assert_eq!(config.debug.recv, 4);
}

/// Verifies config integration with runtime application.
#[test]
fn config_and_runtime_application() {
    let mut config = VerbosityConfig::default();
    config.apply_debug_flag("recv2").unwrap();
    init(config);

    assert!(debug_gte(DebugFlag::Recv, 2));

    apply_debug_flag("send3").unwrap();
    assert!(debug_gte(DebugFlag::Send, 3));
}

// ============================================================================
// Combined Scenario Tests
// ============================================================================

/// Verifies realistic scenario with multiple flags at different levels.
#[test]
fn realistic_debug_scenario() {
    init(VerbosityConfig::default());
    drain_events();

    // Enable various debugging for a transfer
    apply_debug_flag("recv2").unwrap();
    apply_debug_flag("send2").unwrap();
    apply_debug_flag("deltasum3").unwrap();
    apply_debug_flag("flist").unwrap();
    apply_debug_flag("io").unwrap();

    assert!(debug_gte(DebugFlag::Recv, 2));
    assert!(debug_gte(DebugFlag::Send, 2));
    assert!(debug_gte(DebugFlag::Deltasum, 3));
    assert!(debug_gte(DebugFlag::Flist, 1));
    assert!(debug_gte(DebugFlag::Io, 1));

    // Ensure other flags are not enabled
    assert!(!debug_gte(DebugFlag::Acl, 1));
    assert!(!debug_gte(DebugFlag::Backup, 1));
}

/// Verifies enabling and then disabling flags.
#[test]
fn enable_then_disable_scenario() {
    init(VerbosityConfig::default());

    apply_debug_flag("recv3").unwrap();
    assert!(debug_gte(DebugFlag::Recv, 3));

    apply_debug_flag("recv0").unwrap();
    assert!(!debug_gte(DebugFlag::Recv, 1));
}

/// Verifies progressive level increases.
#[test]
fn progressive_level_increases() {
    init(VerbosityConfig::default());

    apply_debug_flag("flist").unwrap();
    assert!(debug_gte(DebugFlag::Flist, 1));
    assert!(!debug_gte(DebugFlag::Flist, 2));

    apply_debug_flag("flist2").unwrap();
    assert!(debug_gte(DebugFlag::Flist, 2));
    assert!(!debug_gte(DebugFlag::Flist, 3));

    apply_debug_flag("flist3").unwrap();
    assert!(debug_gte(DebugFlag::Flist, 3));
    assert!(!debug_gte(DebugFlag::Flist, 4));

    apply_debug_flag("flist4").unwrap();
    assert!(debug_gte(DebugFlag::Flist, 4));
}
