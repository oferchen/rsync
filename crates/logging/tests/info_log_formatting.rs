//! Integration tests for info log formatting and output.
//!
//! These tests verify that the `info_log!` macro correctly formats and emits
//! diagnostic messages for user-facing information. Info messages are typically
//! shown during normal operation (e.g., file names, statistics).
//!
//! Reference: rsync 3.4.1 log.c for info output formatting.

use logging::{info_log, InfoFlag, VerbosityConfig, drain_events, init, DiagnosticEvent};

// ============================================================================
// Basic Info Log Emission Tests
// ============================================================================

/// Verifies info_log emits message when flag level is sufficient.
#[test]
fn info_log_emits_when_level_sufficient() {
    let mut config = VerbosityConfig::default();
    config.info.name = 2;
    init(config);
    drain_events();

    info_log!(Name, 1, "file.txt");

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Info { flag, level, message } => {
            assert_eq!(*flag, InfoFlag::Name);
            assert_eq!(*level, 1);
            assert_eq!(message, "file.txt");
        }
        _ => panic!("expected info event"),
    }
}

/// Verifies info_log suppresses message when level is insufficient.
#[test]
fn info_log_suppresses_when_level_insufficient() {
    let mut config = VerbosityConfig::default();
    config.info.name = 1;
    init(config);
    drain_events();

    info_log!(Name, 2, "should not appear");

    let events = drain_events();
    assert_eq!(events.len(), 0);
}

/// Verifies info_log emits message when level exactly matches.
#[test]
fn info_log_emits_when_level_exact_match() {
    let mut config = VerbosityConfig::default();
    config.info.stats = 2;
    init(config);
    drain_events();

    info_log!(Stats, 2, "exact match");

    let events = drain_events();
    assert_eq!(events.len(), 1);
}

// ============================================================================
// Info Flag Category Tests
// ============================================================================

/// Verifies each info flag category emits independently.
#[test]
fn info_log_flags_are_independent() {
    let mut config = VerbosityConfig::default();
    config.info.copy = 2;
    config.info.del = 0;
    init(config);
    drain_events();

    info_log!(Copy, 1, "copy message");
    info_log!(Del, 1, "del message");

    let events = drain_events();
    // Only copy should emit
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Info { flag, message, .. } => {
            assert_eq!(*flag, InfoFlag::Copy);
            assert_eq!(message, "copy message");
        }
        _ => panic!("expected info event"),
    }
}

/// Verifies all info flags can be tested.
#[test]
fn info_log_all_flags() {
    let mut config = VerbosityConfig::default();
    config.info.set_all(1);
    init(config);
    drain_events();

    info_log!(Backup, 1, "backup");
    info_log!(Copy, 1, "copy");
    info_log!(Del, 1, "del");
    info_log!(Flist, 1, "flist");
    info_log!(Misc, 1, "misc");
    info_log!(Mount, 1, "mount");
    info_log!(Name, 1, "name");
    info_log!(Nonreg, 1, "nonreg");
    info_log!(Progress, 1, "progress");
    info_log!(Remove, 1, "remove");
    info_log!(Skip, 1, "skip");
    info_log!(Stats, 1, "stats");
    info_log!(Symsafe, 1, "symsafe");

    let events = drain_events();
    assert_eq!(events.len(), 13);
}

// ============================================================================
// Info Log Formatting Tests
// ============================================================================

/// Verifies info_log supports format string arguments.
#[test]
fn info_log_format_string() {
    let mut config = VerbosityConfig::default();
    config.info.copy = 1;
    init(config);
    drain_events();

    let bytes = 1024;
    info_log!(Copy, 1, "copied {} bytes", bytes);

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Info { message, .. } => {
            assert_eq!(message, "copied 1024 bytes");
        }
        _ => panic!("expected info event"),
    }
}

/// Verifies info_log handles multiple format arguments.
#[test]
fn info_log_multiple_format_args() {
    let mut config = VerbosityConfig::default();
    config.info.stats = 1;
    init(config);
    drain_events();

    info_log!(Stats, 1, "sent {} received {} total {}", 100, 50, 150);

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Info { message, .. } => {
            assert_eq!(message, "sent 100 received 50 total 150");
        }
        _ => panic!("expected info event"),
    }
}

/// Verifies info_log handles path-like format.
#[test]
fn info_log_path_format() {
    let mut config = VerbosityConfig::default();
    config.info.name = 1;
    init(config);
    drain_events();

    let path = "/home/user/documents/file.txt";
    info_log!(Name, 1, "{}", path);

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Info { message, .. } => {
            assert_eq!(message, "/home/user/documents/file.txt");
        }
        _ => panic!("expected info event"),
    }
}

/// Verifies info_log handles itemize-changes style format.
/// This matches rsync's -i/--itemize-changes output format.
#[test]
fn info_log_itemize_format() {
    let mut config = VerbosityConfig::default();
    config.info.name = 2;
    init(config);
    drain_events();

    // Itemize format: YXcstpoguax where each position has meaning
    // Y = update type (< sent, > received, c created, h hard link, . no change)
    // X = file type (f file, d directory, L symlink, D device, S special)
    // c = checksum, s = size, t = time, p = permissions, o = owner, g = group
    // u = reserved, a = ACL, x = extended attributes
    let itemize = ">f..t......";
    let filename = "updated_file.txt";
    info_log!(Name, 2, "{} {}", itemize, filename);

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Info { message, .. } => {
            assert_eq!(message, ">f..t...... updated_file.txt");
        }
        _ => panic!("expected info event"),
    }
}

/// Verifies info_log handles progress percentage format.
#[test]
fn info_log_progress_format() {
    let mut config = VerbosityConfig::default();
    config.info.progress = 1;
    init(config);
    drain_events();

    let percent = 45.5;
    let speed = "1.2MB/s";
    info_log!(Progress, 1, "{:.1}% complete, {}", percent, speed);

    let events = drain_events();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DiagnosticEvent::Info { message, .. } => {
            assert_eq!(message, "45.5% complete, 1.2MB/s");
        }
        _ => panic!("expected info event"),
    }
}

/// Verifies info_log handles statistics summary format.
#[test]
fn info_log_stats_summary_format() {
    let mut config = VerbosityConfig::default();
    config.info.stats = 1;
    init(config);
    drain_events();

    info_log!(Stats, 1, "Number of files: {}", 1_234_567_u64);
    info_log!(Stats, 1, "Total file size: {} bytes", 9_876_543_210_u64);

    let events = drain_events();
    assert_eq!(events.len(), 2);
}

// ============================================================================
// Info Level Threshold Tests
// ============================================================================

/// Verifies info output at level 0 always emits when flag is set.
#[test]
fn info_log_level_zero_always_emits() {
    let mut config = VerbosityConfig::default();
    config.info.misc = 1;
    init(config);
    drain_events();

    info_log!(Misc, 0, "level zero message");

    let events = drain_events();
    assert_eq!(events.len(), 1);
}

/// Verifies high info levels require matching configuration.
#[test]
fn info_log_high_level_requires_config() {
    let mut config = VerbosityConfig::default();
    config.info.name = 2;
    init(config);
    drain_events();

    info_log!(Name, 1, "level 1");
    info_log!(Name, 2, "level 2");
    info_log!(Name, 3, "level 3 - should not emit");

    let events = drain_events();
    assert_eq!(events.len(), 2);
}

// ============================================================================
// Info Event Order Preservation
// ============================================================================

/// Verifies info events preserve chronological order.
#[test]
fn info_log_preserves_order() {
    let mut config = VerbosityConfig::default();
    config.info.set_all(1);
    init(config);
    drain_events();

    info_log!(Name, 1, "file1.txt");
    info_log!(Name, 1, "file2.txt");
    info_log!(Name, 1, "file3.txt");

    let events = drain_events();
    assert_eq!(events.len(), 3);

    let messages: Vec<_> = events.iter().map(|e| {
        match e {
            DiagnosticEvent::Info { message, .. } => message.as_str(),
            _ => panic!("expected info event"),
        }
    }).collect();

    assert_eq!(messages, vec!["file1.txt", "file2.txt", "file3.txt"]);
}

// ============================================================================
// Info Log With Default Configuration
// ============================================================================

/// Verifies info_log does not emit with default (zero) config.
#[test]
fn info_log_default_config_suppresses() {
    init(VerbosityConfig::default());
    drain_events();

    info_log!(Name, 1, "should not appear");
    info_log!(Copy, 1, "should not appear");
    info_log!(Stats, 1, "should not appear");

    let events = drain_events();
    assert_eq!(events.len(), 0);
}

// ============================================================================
// Mixed Info and Debug Events
// ============================================================================

/// Verifies info and debug events can be intermixed.
#[test]
fn info_and_debug_mixed() {
    use logging::debug_log;

    let mut config = VerbosityConfig::default();
    config.info.name = 1;
    config.debug.recv = 1;
    init(config);
    drain_events();

    info_log!(Name, 1, "transferring file");
    debug_log!(Recv, 1, "received block");
    info_log!(Name, 1, "transfer complete");

    let events = drain_events();
    assert_eq!(events.len(), 3);

    // Verify order and types
    assert!(matches!(&events[0], DiagnosticEvent::Info { .. }));
    assert!(matches!(&events[1], DiagnosticEvent::Debug { .. }));
    assert!(matches!(&events[2], DiagnosticEvent::Info { .. }));
}
