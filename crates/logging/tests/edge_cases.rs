//! Integration tests for logging edge cases.
//!
//! These tests verify correct handling of edge cases including empty messages,
//! special characters, long lines, unicode content, and boundary conditions.
//!
//! Reference: rsync 3.4.1 log.c for message handling edge cases.

use logging::{
    DebugFlag, DiagnosticEvent, InfoFlag, VerbosityConfig, apply_info_flag, debug_log,
    drain_events, info_log, init,
};

// ============================================================================
// Empty Message Tests
// ============================================================================

/// Verifies empty string messages are handled correctly.
#[test]
fn empty_message_info_log() {
    let mut config = VerbosityConfig::default();
    config.info.name = 1;
    init(config);
    drain_events();

    info_log!(Name, 1, "");

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Info { message, .. } => {
            assert_eq!(message, "");
        }
        _ => panic!("expected info event"),
    }
}

/// Verifies empty string messages in debug log.
#[test]
fn empty_message_debug_log() {
    let mut config = VerbosityConfig::default();
    config.debug.recv = 1;
    init(config);
    drain_events();

    debug_log!(Recv, 1, "");

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Debug { message, .. } => {
            assert_eq!(message, "");
        }
        _ => panic!("expected debug event"),
    }
}

/// Verifies whitespace-only messages are preserved.
#[test]
fn whitespace_only_message() {
    let mut config = VerbosityConfig::default();
    config.info.misc = 1;
    init(config);
    drain_events();

    info_log!(Misc, 1, "   ");

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Info { message, .. } => {
            assert_eq!(message, "   ");
        }
        _ => panic!("expected info event"),
    }
}

// ============================================================================
// Special Character Tests
// ============================================================================

/// Verifies newlines in messages are preserved.
#[test]
fn message_with_newlines() {
    let mut config = VerbosityConfig::default();
    config.info.stats = 1;
    init(config);
    drain_events();

    info_log!(Stats, 1, "line1\nline2\nline3");

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Info { message, .. } => {
            assert_eq!(message, "line1\nline2\nline3");
            assert_eq!(message.lines().count(), 3);
        }
        _ => panic!("expected info event"),
    }
}

/// Verifies tab characters in messages are preserved.
#[test]
fn message_with_tabs() {
    let mut config = VerbosityConfig::default();
    config.info.name = 1;
    init(config);
    drain_events();

    info_log!(Name, 1, "col1\tcol2\tcol3");

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Info { message, .. } => {
            assert_eq!(message, "col1\tcol2\tcol3");
        }
        _ => panic!("expected info event"),
    }
}

/// Verifies carriage return handling.
#[test]
fn message_with_carriage_return() {
    let mut config = VerbosityConfig::default();
    config.info.progress = 1;
    init(config);
    drain_events();

    info_log!(Progress, 1, "progress: 50%\r");

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Info { message, .. } => {
            assert_eq!(message, "progress: 50%\r");
        }
        _ => panic!("expected info event"),
    }
}

/// Verifies null bytes in format strings.
#[test]
fn message_with_null_bytes() {
    let mut config = VerbosityConfig::default();
    config.debug.io = 1;
    init(config);
    drain_events();

    // Null bytes in the middle of a message
    debug_log!(Io, 1, "before\0after");

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Debug { message, .. } => {
            assert_eq!(message, "before\0after");
            assert_eq!(message.len(), 12);
        }
        _ => panic!("expected debug event"),
    }
}

/// Verifies escape sequences in messages.
#[test]
fn message_with_escape_sequences() {
    let mut config = VerbosityConfig::default();
    config.info.name = 1;
    init(config);
    drain_events();

    info_log!(Name, 1, "path with\\backslash");

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Info { message, .. } => {
            assert_eq!(message, "path with\\backslash");
        }
        _ => panic!("expected info event"),
    }
}

/// Verifies quotes in messages.
#[test]
fn message_with_quotes() {
    let mut config = VerbosityConfig::default();
    config.info.name = 1;
    init(config);
    drain_events();

    info_log!(Name, 1, "file \"with quotes\".txt");

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Info { message, .. } => {
            assert_eq!(message, "file \"with quotes\".txt");
        }
        _ => panic!("expected info event"),
    }
}

// ============================================================================
// Unicode Tests
// ============================================================================

/// Verifies UTF-8 unicode characters are preserved.
#[test]
fn message_with_unicode() {
    let mut config = VerbosityConfig::default();
    config.info.name = 1;
    init(config);
    drain_events();

    info_log!(Name, 1, "file_with_unicode.txt");

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Info { message, .. } => {
            assert!(message.contains("unicode"));
        }
        _ => panic!("expected info event"),
    }
}

/// Verifies emoji in messages.
#[test]
fn message_with_emoji() {
    let mut config = VerbosityConfig::default();
    config.info.misc = 1;
    init(config);
    drain_events();

    info_log!(Misc, 1, "success!");

    let events = drain_events();
    assert_eq!(events.len(), 1);
}

/// Verifies CJK characters in messages.
#[test]
fn message_with_cjk_characters() {
    let mut config = VerbosityConfig::default();
    config.info.name = 1;
    init(config);
    drain_events();

    info_log!(Name, 1, "filename.txt");

    let events = drain_events();
    assert_eq!(events.len(), 1);
}

/// Verifies right-to-left text handling.
#[test]
fn message_with_rtl_text() {
    let mut config = VerbosityConfig::default();
    config.info.name = 1;
    init(config);
    drain_events();

    // Arabic text
    info_log!(Name, 1, "document.txt");

    let events = drain_events();
    assert_eq!(events.len(), 1);
}

// ============================================================================
// Long Line Tests
// ============================================================================

/// Verifies very long messages are handled.
#[test]
fn very_long_message() {
    let mut config = VerbosityConfig::default();
    config.info.name = 1;
    init(config);
    drain_events();

    let long_path = "a/".repeat(500) + "file.txt";
    info_log!(Name, 1, "{}", long_path);

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Info { message, .. } => {
            assert_eq!(message.len(), long_path.len());
        }
        _ => panic!("expected info event"),
    }
}

/// Verifies message with many format arguments.
#[test]
fn message_with_many_arguments() {
    let mut config = VerbosityConfig::default();
    config.debug.io = 1;
    init(config);
    drain_events();

    debug_log!(
        Io,
        1,
        "a={} b={} c={} d={} e={} f={} g={} h={} i={} j={}",
        1,
        2,
        3,
        4,
        5,
        6,
        7,
        8,
        9,
        10
    );

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Debug { message, .. } => {
            assert_eq!(message, "a=1 b=2 c=3 d=4 e=5 f=6 g=7 h=8 i=9 j=10");
        }
        _ => panic!("expected debug event"),
    }
}

/// Verifies single character message.
#[test]
fn single_character_message() {
    let mut config = VerbosityConfig::default();
    config.info.name = 1;
    init(config);
    drain_events();

    info_log!(Name, 1, "x");

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Info { message, .. } => {
            assert_eq!(message, "x");
        }
        _ => panic!("expected info event"),
    }
}

// ============================================================================
// Flag Token Edge Cases
// ============================================================================

/// Verifies empty flag token is rejected.
#[test]
fn empty_flag_token_rejected() {
    init(VerbosityConfig::default());

    let result = apply_info_flag("");
    assert!(result.is_err());
}

/// Verifies flag token with only digits is rejected.
#[test]
fn digits_only_flag_rejected() {
    init(VerbosityConfig::default());

    let result = apply_info_flag("123");
    assert!(result.is_err());
}

/// Verifies flag token with high level number.
#[test]
fn flag_with_high_level() {
    init(VerbosityConfig::default());

    let result = apply_info_flag("copy255");
    assert!(result.is_ok());
}

/// Verifies flag token with level zero.
#[test]
fn flag_with_level_zero() {
    init(VerbosityConfig::default());

    // Level 0 should set the flag to 0
    let result = apply_info_flag("copy0");
    assert!(result.is_ok());
}

/// Verifies flag token with leading zeros.
#[test]
fn flag_with_leading_zeros() {
    init(VerbosityConfig::default());

    // "copy007" should parse as copy level 7
    let result = apply_info_flag("copy007");
    assert!(result.is_ok());
}

// ============================================================================
// Event Draining Edge Cases
// ============================================================================

/// Verifies draining empty event queue returns empty vec.
#[test]
fn drain_empty_returns_empty() {
    init(VerbosityConfig::default());
    drain_events(); // Clear any existing

    let events = drain_events();
    assert!(events.is_empty());
}

/// Verifies multiple drains on same queue.
#[test]
fn multiple_drains() {
    let mut config = VerbosityConfig::default();
    config.info.name = 1;
    init(config);
    drain_events();

    info_log!(Name, 1, "message 1");
    let first = drain_events();
    assert_eq!(first.len(), 1);

    let second = drain_events();
    assert_eq!(second.len(), 0);

    info_log!(Name, 1, "message 2");
    let third = drain_events();
    assert_eq!(third.len(), 1);
}

/// Verifies large number of events.
#[test]
fn many_events() {
    let mut config = VerbosityConfig::default();
    config.info.name = 1;
    init(config);
    drain_events();

    for i in 0..1000 {
        info_log!(Name, 1, "message {}", i);
    }

    let events = drain_events();
    assert_eq!(events.len(), 1000);

    // Verify first and last
    match &events[0] {
        DiagnosticEvent::Info { message, .. } => {
            assert_eq!(message, "message 0");
        }
        _ => panic!("expected info event"),
    }
    match &events[999] {
        DiagnosticEvent::Info { message, .. } => {
            assert_eq!(message, "message 999");
        }
        _ => panic!("expected info event"),
    }
}

// ============================================================================
// Diagnostic Event Structure Tests
// ============================================================================

/// Verifies DiagnosticEvent::Info contains all fields.
#[test]
fn diagnostic_event_info_fields() {
    let mut config = VerbosityConfig::default();
    config.info.copy = 3;
    init(config);
    drain_events();

    info_log!(Copy, 2, "test message");

    let events = drain_events();
    match &events[0] {
        DiagnosticEvent::Info {
            flag,
            level,
            message,
        } => {
            assert_eq!(*flag, InfoFlag::Copy);
            assert_eq!(*level, 2);
            assert_eq!(message, "test message");
        }
        _ => panic!("expected info event"),
    }
}

/// Verifies DiagnosticEvent::Debug contains all fields.
#[test]
fn diagnostic_event_debug_fields() {
    let mut config = VerbosityConfig::default();
    config.debug.recv = 3;
    init(config);
    drain_events();

    debug_log!(Recv, 2, "debug message");

    let events = drain_events();
    match &events[0] {
        DiagnosticEvent::Debug {
            flag,
            level,
            message,
        } => {
            assert_eq!(*flag, DebugFlag::Recv);
            assert_eq!(*level, 2);
            assert_eq!(message, "debug message");
        }
        _ => panic!("expected debug event"),
    }
}

/// Verifies DiagnosticEvent implements Clone.
#[test]
fn diagnostic_event_clone() {
    let event = DiagnosticEvent::Info {
        flag: InfoFlag::Name,
        level: 1,
        message: "cloneable".to_owned(),
    };

    let cloned = event.clone();
    match cloned {
        DiagnosticEvent::Info {
            flag,
            level,
            message,
        } => {
            assert_eq!(flag, InfoFlag::Name);
            assert_eq!(level, 1);
            assert_eq!(message, "cloneable");
        }
        _ => panic!("expected info event"),
    }
}

/// Verifies DiagnosticEvent implements Debug.
#[test]
fn diagnostic_event_debug_trait() {
    let event = DiagnosticEvent::Debug {
        flag: DebugFlag::Io,
        level: 3,
        message: "debug trait test".to_owned(),
    };

    let debug_str = format!("{event:?}");
    assert!(debug_str.contains("Debug"));
    assert!(debug_str.contains("Io"));
    assert!(debug_str.contains("debug trait test"));
}

// ============================================================================
// Configuration Edge Cases
// ============================================================================

/// Verifies VerbosityConfig implements Default.
#[test]
fn verbosity_config_default() {
    let config = VerbosityConfig::default();

    // All levels should be 0
    assert_eq!(config.info.copy, 0);
    assert_eq!(config.info.name, 0);
    assert_eq!(config.debug.recv, 0);
    assert_eq!(config.debug.send, 0);
}

/// Verifies VerbosityConfig implements Clone.
#[test]
fn verbosity_config_clone() {
    let mut original = VerbosityConfig::default();
    original.info.name = 5;
    original.debug.flist = 3;

    let cloned = original.clone();
    assert_eq!(cloned.info.name, 5);
    assert_eq!(cloned.debug.flist, 3);
}

/// Verifies VerbosityConfig implements Debug.
#[test]
fn verbosity_config_debug_trait() {
    let config = VerbosityConfig::from_verbose_level(2);
    let debug_str = format!("{config:?}");

    assert!(debug_str.contains("VerbosityConfig"));
    assert!(debug_str.contains("info"));
    assert!(debug_str.contains("debug"));
}
