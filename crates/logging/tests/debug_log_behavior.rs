//! Integration tests for debug log macro behavior at different levels.
//!
//! These tests verify that the `debug_log!` macro correctly emits or suppresses
//! diagnostic messages based on the configured verbosity levels. The behavior
//! mirrors rsync's -debug=FLAG[N] option handling.
//!
//! Reference: rsync 3.4.1 log.c for debug output behavior.

use logging::{debug_log, DebugFlag, VerbosityConfig, drain_events, init, DiagnosticEvent};

// ============================================================================
// Basic Debug Log Emission Tests
// ============================================================================

/// Verifies debug_log emits message when flag level is sufficient.
#[test]
fn debug_log_emits_when_level_sufficient() {
    let mut config = VerbosityConfig::default();
    config.debug.recv = 2;
    init(config);
    drain_events(); // Clear any existing events

    debug_log!(Recv, 1, "test message");

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Debug { flag, level, message } => {
            assert_eq!(*flag, DebugFlag::Recv);
            assert_eq!(*level, 1);
            assert_eq!(message, "test message");
        }
        _ => panic!("expected debug event"),
    }
}

/// Verifies debug_log suppresses message when level is insufficient.
#[test]
fn debug_log_suppresses_when_level_insufficient() {
    let mut config = VerbosityConfig::default();
    config.debug.recv = 1;
    init(config);
    drain_events();

    debug_log!(Recv, 2, "should not appear");

    let events = drain_events();
    assert_eq!(events.len(), 0);
}

/// Verifies debug_log emits message when level exactly matches.
#[test]
fn debug_log_emits_when_level_exact_match() {
    let mut config = VerbosityConfig::default();
    config.debug.send = 3;
    init(config);
    drain_events();

    debug_log!(Send, 3, "exact match");

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Debug { message, .. } => {
            assert_eq!(message, "exact match");
        }
        _ => panic!("expected debug event"),
    }
}

// ============================================================================
// Debug Flag Category Tests
// ============================================================================

/// Verifies each debug flag category emits independently.
#[test]
fn debug_log_flags_are_independent() {
    let mut config = VerbosityConfig::default();
    config.debug.recv = 2;
    config.debug.send = 0;
    init(config);
    drain_events();

    debug_log!(Recv, 1, "recv message");
    debug_log!(Send, 1, "send message");

    let events = drain_events();
    // Only recv should emit
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Debug { flag, message, .. } => {
            assert_eq!(*flag, DebugFlag::Recv);
            assert_eq!(message, "recv message");
        }
        _ => panic!("expected debug event"),
    }
}

/// Verifies all debug flags can be tested.
#[test]
fn debug_log_all_flags() {
    let mut config = VerbosityConfig::default();
    config.debug.set_all(1);
    init(config);
    drain_events();

    // Test representative flags from different categories
    debug_log!(Acl, 1, "acl");
    debug_log!(Backup, 1, "backup");
    debug_log!(Bind, 1, "bind");
    debug_log!(Chdir, 1, "chdir");
    debug_log!(Connect, 1, "connect");
    debug_log!(Cmd, 1, "cmd");
    debug_log!(Del, 1, "del");
    debug_log!(Deltasum, 1, "deltasum");
    debug_log!(Dup, 1, "dup");
    debug_log!(Exit, 1, "exit");
    debug_log!(Filter, 1, "filter");
    debug_log!(Flist, 1, "flist");
    debug_log!(Fuzzy, 1, "fuzzy");
    debug_log!(Genr, 1, "genr");
    debug_log!(Hash, 1, "hash");
    debug_log!(Hlink, 1, "hlink");
    debug_log!(Iconv, 1, "iconv");
    debug_log!(Io, 1, "io");
    debug_log!(Nstr, 1, "nstr");
    debug_log!(Own, 1, "own");
    debug_log!(Proto, 1, "proto");
    debug_log!(Recv, 1, "recv");
    debug_log!(Send, 1, "send");
    debug_log!(Time, 1, "time");

    let events = drain_events();
    assert_eq!(events.len(), 24);
}

// ============================================================================
// Debug Level Threshold Tests
// ============================================================================

/// Verifies debug output at level 0 always emits when flag is set.
#[test]
fn debug_log_level_zero_always_emits() {
    let mut config = VerbosityConfig::default();
    config.debug.deltasum = 1;
    init(config);
    drain_events();

    debug_log!(Deltasum, 0, "level zero message");

    let events = drain_events();
    assert_eq!(events.len(), 1);
}

/// Verifies high debug levels require matching configuration.
#[test]
fn debug_log_high_level_requires_config() {
    let mut config = VerbosityConfig::default();
    config.debug.flist = 2;
    init(config);
    drain_events();

    debug_log!(Flist, 1, "level 1");
    debug_log!(Flist, 2, "level 2");
    debug_log!(Flist, 3, "level 3 - should not emit");
    debug_log!(Flist, 4, "level 4 - should not emit");

    let events = drain_events();
    assert_eq!(events.len(), 2);
}

/// Verifies debug level boundary at maximum typical value.
#[test]
fn debug_log_maximum_level() {
    let mut config = VerbosityConfig::default();
    config.debug.deltasum = 255; // u8 max
    init(config);
    drain_events();

    debug_log!(Deltasum, 255, "max level");

    let events = drain_events();
    assert_eq!(events.len(), 1);
}

// ============================================================================
// Debug Log Formatting Tests
// ============================================================================

/// Verifies debug_log supports format string arguments.
#[test]
fn debug_log_format_string() {
    let mut config = VerbosityConfig::default();
    config.debug.recv = 1;
    init(config);
    drain_events();

    let value = 42;
    debug_log!(Recv, 1, "received {} bytes", value);

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Debug { message, .. } => {
            assert_eq!(message, "received 42 bytes");
        }
        _ => panic!("expected debug event"),
    }
}

/// Verifies debug_log handles multiple format arguments.
#[test]
fn debug_log_multiple_format_args() {
    let mut config = VerbosityConfig::default();
    config.debug.io = 1;
    init(config);
    drain_events();

    debug_log!(Io, 1, "offset={} len={} tag={}", 100, 50, "DATA");

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Debug { message, .. } => {
            assert_eq!(message, "offset=100 len=50 tag=DATA");
        }
        _ => panic!("expected debug event"),
    }
}

/// Verifies debug_log handles complex format specifiers.
#[test]
fn debug_log_complex_format() {
    let mut config = VerbosityConfig::default();
    config.debug.hash = 1;
    init(config);
    drain_events();

    debug_log!(Hash, 1, "hash={:08x} block={:04}", 0xdeadbeef_u32, 7);

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Debug { message, .. } => {
            assert_eq!(message, "hash=deadbeef block=0007");
        }
        _ => panic!("expected debug event"),
    }
}

// ============================================================================
// Debug Event Order Preservation
// ============================================================================

/// Verifies debug events preserve chronological order.
#[test]
fn debug_log_preserves_order() {
    let mut config = VerbosityConfig::default();
    config.debug.set_all(1);
    init(config);
    drain_events();

    debug_log!(Recv, 1, "first");
    debug_log!(Send, 1, "second");
    debug_log!(Io, 1, "third");

    let events = drain_events();
    assert_eq!(events.len(), 3);

    let messages: Vec<_> = events.iter().map(|e| {
        match e {
            DiagnosticEvent::Debug { message, .. } => message.as_str(),
            _ => panic!("expected debug event"),
        }
    }).collect();

    assert_eq!(messages, vec!["first", "second", "third"]);
}

// ============================================================================
// Debug Log With Default Configuration
// ============================================================================

/// Verifies debug_log does not emit with default (zero) config.
#[test]
fn debug_log_default_config_suppresses() {
    init(VerbosityConfig::default());
    drain_events();

    debug_log!(Recv, 1, "should not appear");
    debug_log!(Send, 1, "should not appear");
    debug_log!(Flist, 1, "should not appear");

    let events = drain_events();
    assert_eq!(events.len(), 0);
}

/// Verifies debug_log at level 0 still requires flag to be set.
#[test]
fn debug_log_level_zero_with_default_config() {
    init(VerbosityConfig::default());
    drain_events();

    // Even level 0 requires the flag to be at least 0 (which it is)
    // Level 0 check: config.debug.recv >= 0 is always true
    debug_log!(Recv, 0, "level zero");

    let events = drain_events();
    // Level 0 means "always emit if checking", and 0 >= 0 is true
    assert_eq!(events.len(), 1);
}

// ============================================================================
// Debug Log Re-initialization
// ============================================================================

/// Verifies debug config can be changed at runtime.
#[test]
fn debug_log_reinit_changes_behavior() {
    let mut config1 = VerbosityConfig::default();
    config1.debug.recv = 1;
    init(config1);
    drain_events();

    debug_log!(Recv, 1, "should emit");
    assert_eq!(drain_events().len(), 1);

    // Reinitialize with different config
    let config2 = VerbosityConfig::default();
    init(config2);

    debug_log!(Recv, 1, "should not emit");
    assert_eq!(drain_events().len(), 0);
}
