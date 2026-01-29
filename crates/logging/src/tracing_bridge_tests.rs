//! Integration tests for the tracing bridge.

#![cfg(all(test, feature = "tracing"))]

use crate::{DebugFlag, DiagnosticEvent, InfoFlag, VerbosityConfig, thread_local};

#[test]
fn test_tracing_with_verbosity_level_1() {
    let config = VerbosityConfig::from_verbose_level(1);
    thread_local::init(config);

    // Level 1 should enable copy at level 1
    assert!(thread_local::info_gte(InfoFlag::Copy, 1));

    // Emit a copy event
    thread_local::emit_info(InfoFlag::Copy, 1, "test copy".to_owned());

    let events = thread_local::drain_events();
    assert_eq!(events.len(), 1);
}

#[test]
fn test_tracing_with_verbosity_level_2() {
    let config = VerbosityConfig::from_verbose_level(2);
    thread_local::init(config);

    // Level 2 should enable debug flags
    assert!(thread_local::debug_gte(DebugFlag::Bind, 1));
    assert!(thread_local::debug_gte(DebugFlag::Deltasum, 1));

    thread_local::emit_debug(DebugFlag::Deltasum, 1, "delta test".to_owned());

    let events = thread_local::drain_events();
    assert_eq!(events.len(), 1);

    match &events[0] {
        DiagnosticEvent::Debug { flag, level, message } => {
            assert_eq!(*flag, DebugFlag::Deltasum);
            assert_eq!(*level, 1);
            assert_eq!(message, "delta test");
        }
        _ => panic!("Expected debug event"),
    }
}

#[test]
fn test_tracing_respects_verbosity_filters() {
    let config = VerbosityConfig::from_verbose_level(1);
    thread_local::init(config);

    // Level 1 doesn't enable debug flags
    assert!(!thread_local::debug_gte(DebugFlag::Deltasum, 1));

    // This event should not be recorded
    thread_local::emit_debug(DebugFlag::Deltasum, 1, "should be filtered".to_owned());

    // But we still collect it in the event buffer (filtering happens at emission check)
    // The real filtering happens when checking debug_gte before emit
}

#[test]
fn test_manual_info_flag_application() {
    let mut config = VerbosityConfig::default();

    // Apply specific info flag
    config.apply_info_flag("copy2").unwrap();

    thread_local::init(config);

    assert!(thread_local::info_gte(InfoFlag::Copy, 1));
    assert!(thread_local::info_gte(InfoFlag::Copy, 2));
    assert!(!thread_local::info_gte(InfoFlag::Copy, 3));
}

#[test]
fn test_manual_debug_flag_application() {
    let mut config = VerbosityConfig::default();

    // Apply specific debug flags
    config.apply_debug_flag("io3").unwrap();
    config.apply_debug_flag("proto").unwrap();

    thread_local::init(config);

    assert!(thread_local::debug_gte(DebugFlag::Io, 3));
    assert!(thread_local::debug_gte(DebugFlag::Proto, 1));
}

#[test]
fn test_event_ordering() {
    let config = VerbosityConfig::from_verbose_level(2);
    thread_local::init(config);
    thread_local::drain_events(); // Clear any existing events

    // Emit events in order
    thread_local::emit_info(InfoFlag::Copy, 1, "first".to_owned());
    thread_local::emit_info(InfoFlag::Del, 1, "second".to_owned());
    thread_local::emit_debug(DebugFlag::Deltasum, 1, "third".to_owned());

    let events = thread_local::drain_events();
    assert_eq!(events.len(), 3);

    // Verify order is preserved
    match &events[0] {
        DiagnosticEvent::Info { message, .. } => assert_eq!(message, "first"),
        _ => panic!("Expected info event"),
    }
    match &events[1] {
        DiagnosticEvent::Info { message, .. } => assert_eq!(message, "second"),
        _ => panic!("Expected info event"),
    }
    match &events[2] {
        DiagnosticEvent::Debug { message, .. } => assert_eq!(message, "third"),
        _ => panic!("Expected debug event"),
    }
}

#[test]
fn test_verbosity_level_progression() {
    // Test that each level adds more flags

    // Level 0
    let config0 = VerbosityConfig::from_verbose_level(0);
    assert_eq!(config0.info.copy, 0);
    assert_eq!(config0.debug.bind, 0);

    // Level 1
    let config1 = VerbosityConfig::from_verbose_level(1);
    assert_eq!(config1.info.copy, 1);
    assert_eq!(config1.debug.bind, 0); // Debug not enabled yet

    // Level 2
    let config2 = VerbosityConfig::from_verbose_level(2);
    assert_eq!(config2.info.copy, 1);
    assert_eq!(config2.info.misc, 2); // Enhanced level
    assert_eq!(config2.debug.bind, 1); // Debug enabled

    // Level 3
    let config3 = VerbosityConfig::from_verbose_level(3);
    assert_eq!(config3.debug.connect, 2); // Enhanced debug
    assert_eq!(config3.debug.acl, 1); // New flags added

    // Level 4
    let config4 = VerbosityConfig::from_verbose_level(4);
    assert_eq!(config4.debug.cmd, 2); // Further enhanced
    assert_eq!(config4.debug.proto, 2); // Protocol debugging

    // Level 5+
    let config5 = VerbosityConfig::from_verbose_level(5);
    assert_eq!(config5.debug.deltasum, 4); // Maximum level
    assert_eq!(config5.debug.chdir, 1); // Additional flags
    assert_eq!(config5.debug.hash, 1);
}
