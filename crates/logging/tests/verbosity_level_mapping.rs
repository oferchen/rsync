//! Integration tests for verbosity level mapping and message filtering.
//!
//! These tests verify that verbosity levels (-v flags and --quiet) map correctly
//! to log levels and that message filtering works as expected. This is critical
//! for ensuring rsync's verbosity system behaves consistently with upstream.
//!
//! Test coverage:
//! 1. -v (level 1) maps to correct log level
//! 2. -vv (level 2), -vvv (level 3) increase verbosity progressively
//! 3. --quiet (level 0) reduces output to minimal
//! 4. Verbosity affects message filtering correctly

use logging::{DebugFlag, InfoFlag, VerbosityConfig, debug_log, drain_events, info_log, init};

// ============================================================================
// Test 1: -v (Verbose Level 1) Mapping
// ============================================================================

/// Verifies -v (level 1) enables basic info output but not debug output.
#[test]
fn verbose_level_1_maps_to_basic_info() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config.clone());

    // Level 1 should enable these info flags at level 1
    assert_eq!(config.info.nonreg, 1);
    assert_eq!(config.info.copy, 1);
    assert_eq!(config.info.del, 1);
    assert_eq!(config.info.flist, 1);
    assert_eq!(config.info.misc, 1);
    assert_eq!(config.info.name, 1);
    assert_eq!(config.info.stats, 1);
    assert_eq!(config.info.symsafe, 1);
}

/// Verifies -v (level 1) does not enable debug flags.
#[test]
fn verbose_level_1_no_debug_output() {
    let config = VerbosityConfig::from_verbose_level(1);

    // Level 1 should not enable any debug flags
    assert_eq!(config.debug.bind, 0);
    assert_eq!(config.debug.cmd, 0);
    assert_eq!(config.debug.connect, 0);
    assert_eq!(config.debug.del, 0);
    assert_eq!(config.debug.deltasum, 0);
    assert_eq!(config.debug.dup, 0);
    assert_eq!(config.debug.filter, 0);
    assert_eq!(config.debug.flist, 0);
    assert_eq!(config.debug.iconv, 0);
    assert_eq!(config.debug.recv, 0);
    assert_eq!(config.debug.send, 0);
}

/// Verifies -v (level 1) does not enable enhanced info levels.
#[test]
fn verbose_level_1_no_enhanced_info() {
    let config = VerbosityConfig::from_verbose_level(1);

    // Level 1 should not enable level 2 info flags
    assert_eq!(config.info.backup, 0);
    assert_eq!(config.info.mount, 0);
    assert_eq!(config.info.remove, 0);
    assert_eq!(config.info.skip, 0);

    // Basic info should be at level 1, not 2
    assert_eq!(config.info.misc, 1);
    assert_eq!(config.info.name, 1);
}

/// Verifies -v (level 1) filters messages correctly.
#[test]
fn verbose_level_1_message_filtering() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    // These should pass through (level 1 info flags)
    info_log!(Name, 1, "file.txt");
    info_log!(Copy, 1, "copying file");
    info_log!(Del, 1, "deleting file");
    info_log!(Stats, 1, "transfer stats");

    // These should be filtered (level 2 or debug)
    info_log!(Name, 2, "itemized change");
    info_log!(Backup, 1, "backup created");
    debug_log!(Recv, 1, "receiver debug");
    debug_log!(Send, 1, "sender debug");

    let events = drain_events();
    // Should only have 4 events (the level 1 info logs)
    assert_eq!(events.len(), 4);
}

// ============================================================================
// Test 2: -vv (Verbose Level 2) Increased Verbosity
// ============================================================================

/// Verifies -vv (level 2) increases info levels and enables debug output.
#[test]
fn verbose_level_2_increases_verbosity() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config.clone());

    // Level 2 should increase some info flags to level 2
    assert_eq!(config.info.misc, 2);
    assert_eq!(config.info.name, 2);

    // Level 2 should enable additional info flags (at level 1, per upstream)
    assert_eq!(config.info.backup, 1);
    assert_eq!(config.info.mount, 1);
    assert_eq!(config.info.remove, 1);
    assert_eq!(config.info.skip, 1);

    // Level 2 should enable basic debug output
    assert_eq!(config.debug.bind, 1);
    assert_eq!(config.debug.cmd, 1);
    assert_eq!(config.debug.connect, 1);
    assert_eq!(config.debug.del, 1);
    assert_eq!(config.debug.deltasum, 1);
    assert_eq!(config.debug.dup, 1);
    assert_eq!(config.debug.filter, 1);
    assert_eq!(config.debug.flist, 1);
    assert_eq!(config.debug.iconv, 1);
}

/// Verifies -vv (level 2) retains all -v (level 1) capabilities.
#[test]
fn verbose_level_2_includes_level_1() {
    let config = VerbosityConfig::from_verbose_level(2);

    // All level 1 info flags should still be enabled
    assert!(config.info.nonreg >= 1);
    assert!(config.info.copy >= 1);
    assert!(config.info.del >= 1);
    assert!(config.info.flist >= 1);
    assert!(config.info.misc >= 1);
    assert!(config.info.name >= 1);
    assert!(config.info.stats >= 1);
    assert!(config.info.symsafe >= 1);
}

/// Verifies -vv (level 2) filters messages correctly.
#[test]
fn verbose_level_2_message_filtering() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);
    drain_events();

    // These should pass through (level 1 and 2 info, level 1 debug)
    info_log!(Name, 1, "file.txt");
    info_log!(Name, 2, "itemized change");
    info_log!(Backup, 1, "backup created");
    info_log!(Skip, 1, "skipped file");
    debug_log!(Deltasum, 1, "computing checksum");
    debug_log!(Flist, 1, "building file list");

    // These should be filtered (exceeds configured levels)
    info_log!(Name, 3, "level 3 info");
    info_log!(Backup, 2, "backup level 2");
    debug_log!(Deltasum, 2, "detailed checksum");
    debug_log!(Connect, 2, "connection detail");

    let events = drain_events();
    // Should have 6 events (4 info + 2 debug at appropriate levels)
    assert_eq!(events.len(), 6);
}

// ============================================================================
// Test 3: -vvv (Verbose Level 3) Further Increased Verbosity
// ============================================================================

/// Verifies -vvv (level 3) further increases debug levels.
#[test]
fn verbose_level_3_increases_debug_verbosity() {
    let config = VerbosityConfig::from_verbose_level(3);
    init(config.clone());

    // Level 3 should increase debug levels
    assert_eq!(config.debug.connect, 2);
    assert_eq!(config.debug.del, 2);
    assert_eq!(config.debug.deltasum, 2);
    assert_eq!(config.debug.filter, 2);
    assert_eq!(config.debug.flist, 2);
    assert_eq!(config.debug.exit, 1);

    // Level 3 should enable additional debug flags
    assert_eq!(config.debug.acl, 1);
    assert_eq!(config.debug.backup, 1);
    assert_eq!(config.debug.fuzzy, 1);
    assert_eq!(config.debug.genr, 1);
    assert_eq!(config.debug.own, 1);
    assert_eq!(config.debug.recv, 1);
    assert_eq!(config.debug.send, 1);
    assert_eq!(config.debug.time, 1);
}

/// Verifies -vvv (level 3) retains all -vv (level 2) capabilities.
#[test]
fn verbose_level_3_includes_level_2() {
    let config2 = VerbosityConfig::from_verbose_level(2);
    let config3 = VerbosityConfig::from_verbose_level(3);

    // All level 2 info flags should be maintained or increased
    assert!(config3.info.misc >= config2.info.misc);
    assert!(config3.info.name >= config2.info.name);
    assert!(config3.info.backup >= config2.info.backup);
    assert!(config3.info.mount >= config2.info.mount);

    // All level 2 debug flags should be maintained or increased
    assert!(config3.debug.bind >= config2.debug.bind);
    assert!(config3.debug.cmd >= config2.debug.cmd);
    assert!(config3.debug.deltasum >= config2.debug.deltasum);
}

/// Verifies -vvv (level 3) filters messages correctly.
#[test]
fn verbose_level_3_message_filtering() {
    let config = VerbosityConfig::from_verbose_level(3);
    init(config);
    drain_events();

    // These should pass through
    info_log!(Name, 2, "itemized change");
    debug_log!(Deltasum, 1, "basic checksum");
    debug_log!(Deltasum, 2, "detailed checksum");
    debug_log!(Recv, 1, "receiver operation");
    debug_log!(Send, 1, "sender operation");
    debug_log!(Genr, 1, "generator operation");

    // These should be filtered (levels too high)
    debug_log!(Deltasum, 3, "very detailed checksum");
    debug_log!(Flist, 3, "detailed file list");

    let events = drain_events();
    // Should have 6 events (all except the level 3 debug)
    assert_eq!(events.len(), 6);
}

// ============================================================================
// Test 4: --quiet (Verbose Level 0) Minimal Output
// ============================================================================

/// Verifies --quiet (level 0) reduces output to minimal.
#[test]
fn quiet_level_0_minimal_output() {
    let config = VerbosityConfig::from_verbose_level(0);
    init(config.clone());

    // Level 0 should only enable nonreg
    assert_eq!(config.info.nonreg, 1);

    // All other info flags should be disabled
    assert_eq!(config.info.copy, 0);
    assert_eq!(config.info.del, 0);
    assert_eq!(config.info.flist, 0);
    assert_eq!(config.info.misc, 0);
    assert_eq!(config.info.name, 0);
    assert_eq!(config.info.stats, 0);
    assert_eq!(config.info.symsafe, 0);
    assert_eq!(config.info.backup, 0);
    assert_eq!(config.info.mount, 0);

    // All debug flags should be disabled
    assert_eq!(config.debug.bind, 0);
    assert_eq!(config.debug.recv, 0);
    assert_eq!(config.debug.send, 0);
    assert_eq!(config.debug.deltasum, 0);
    assert_eq!(config.debug.flist, 0);
}

/// Verifies --quiet (level 0) filters all but nonreg messages.
#[test]
fn quiet_level_0_message_filtering() {
    let config = VerbosityConfig::from_verbose_level(0);
    init(config);
    drain_events();

    // Only nonreg should pass through
    info_log!(Nonreg, 1, "special file warning");

    // These should all be filtered
    info_log!(Name, 1, "file.txt");
    info_log!(Copy, 1, "copying file");
    info_log!(Del, 1, "deleting file");
    info_log!(Stats, 1, "transfer stats");
    debug_log!(Recv, 1, "receiver debug");
    debug_log!(Send, 1, "sender debug");

    let events = drain_events();
    // Should only have 1 event (nonreg)
    assert_eq!(events.len(), 1);
}

/// Verifies --quiet (level 0) is less verbose than -v (level 1).
#[test]
fn quiet_level_0_less_than_level_1() {
    let config0 = VerbosityConfig::from_verbose_level(0);
    let config1 = VerbosityConfig::from_verbose_level(1);

    // Level 0 should have fewer or equal flags enabled
    assert!(config0.info.copy <= config1.info.copy);
    assert!(config0.info.name <= config1.info.name);
    assert!(config0.info.stats <= config1.info.stats);
    assert!(config0.debug.recv <= config1.debug.recv);

    // Level 0 should have strictly less output
    let count0 = config0.info.copy + config0.info.name + config0.info.stats;
    let count1 = config1.info.copy + config1.info.name + config1.info.stats;
    assert!(count0 < count1);
}

// ============================================================================
// Test 5: Progressive Verbosity Increase
// ============================================================================

/// Verifies verbosity levels are strictly progressive.
#[test]
fn verbosity_levels_are_progressive() {
    let levels: Vec<VerbosityConfig> = (0..=5).map(VerbosityConfig::from_verbose_level).collect();

    // Check that specific flags increase or stay constant
    for i in 1..levels.len() {
        let prev = &levels[i - 1];
        let curr = &levels[i];

        // Info flags should not decrease
        assert!(curr.info.nonreg >= prev.info.nonreg);
        assert!(curr.info.copy >= prev.info.copy);
        assert!(curr.info.name >= prev.info.name);

        // Debug flags should not decrease
        assert!(curr.debug.deltasum >= prev.debug.deltasum);
        assert!(curr.debug.flist >= prev.debug.flist);
    }
}

/// Verifies higher levels emit more or equal messages.
#[test]
fn higher_levels_emit_more_messages() {
    // Test with level 0
    let config0 = VerbosityConfig::from_verbose_level(0);
    init(config0);
    drain_events();

    info_log!(Name, 1, "msg1");
    info_log!(Copy, 1, "msg2");
    debug_log!(Recv, 1, "msg3");

    let events0 = drain_events();

    // Test with level 1
    let config1 = VerbosityConfig::from_verbose_level(1);
    init(config1);
    drain_events();

    info_log!(Name, 1, "msg1");
    info_log!(Copy, 1, "msg2");
    debug_log!(Recv, 1, "msg3");

    let events1 = drain_events();

    // Test with level 2
    let config2 = VerbosityConfig::from_verbose_level(2);
    init(config2);
    drain_events();

    info_log!(Name, 1, "msg1");
    info_log!(Copy, 1, "msg2");
    debug_log!(Recv, 1, "msg3");

    let events2 = drain_events();

    // Higher levels should emit same or more messages
    assert!(events1.len() >= events0.len());
    assert!(events2.len() >= events1.len());
}

// ============================================================================
// Test 6: Verbosity Affects Message Filtering
// ============================================================================

/// Verifies same messages are filtered differently at different verbosity levels.
#[test]
fn verbosity_affects_message_filtering() {
    // Level 1: only basic info
    let config1 = VerbosityConfig::from_verbose_level(1);
    init(config1);
    drain_events();

    info_log!(Name, 1, "level 1 message");
    info_log!(Name, 2, "level 2 message");
    debug_log!(Recv, 1, "debug message");

    let events1 = drain_events();
    assert_eq!(events1.len(), 1); // Only name level 1

    // Level 2: enhanced info and debug
    let config2 = VerbosityConfig::from_verbose_level(2);
    init(config2);
    drain_events();

    info_log!(Name, 1, "level 1 message");
    info_log!(Name, 2, "level 2 message");
    debug_log!(Recv, 1, "debug message");

    let events2 = drain_events();
    assert_eq!(events2.len(), 2); // Name levels 1 and 2, no debug yet

    // Level 3: all including enhanced debug
    let config3 = VerbosityConfig::from_verbose_level(3);
    init(config3);
    drain_events();

    info_log!(Name, 1, "level 1 message");
    info_log!(Name, 2, "level 2 message");
    debug_log!(Recv, 1, "debug message");

    let events3 = drain_events();
    assert_eq!(events3.len(), 3); // All three messages
}

/// Verifies filtering works independently for different flags.
#[test]
fn filtering_independent_per_flag() {
    let mut config = VerbosityConfig::default();
    config.info.name = 2;
    config.info.copy = 1;
    config.debug.recv = 1;
    config.debug.send = 0;
    init(config);
    drain_events();

    // Name at level 1 and 2 should pass
    info_log!(Name, 1, "name 1");
    info_log!(Name, 2, "name 2");
    info_log!(Name, 3, "name 3"); // filtered

    // Copy at level 1 should pass
    info_log!(Copy, 1, "copy 1");
    info_log!(Copy, 2, "copy 2"); // filtered

    // Recv at level 1 should pass
    debug_log!(Recv, 1, "recv 1");
    debug_log!(Recv, 2, "recv 2"); // filtered

    // Send at any level should be filtered
    debug_log!(Send, 1, "send 1");

    let events = drain_events();
    assert_eq!(events.len(), 4); // name 1, name 2, copy 1, recv 1
}

/// Verifies verbosity level 0 filters more than level 1.
#[test]
fn level_0_filters_more_than_level_1() {
    let test_messages = vec![
        (InfoFlag::Name, 1),
        (InfoFlag::Copy, 1),
        (InfoFlag::Del, 1),
        (InfoFlag::Stats, 1),
    ];

    // Count messages at level 0
    let config0 = VerbosityConfig::from_verbose_level(0);
    init(config0);
    drain_events();

    for (flag, level) in &test_messages {
        match flag {
            InfoFlag::Name => info_log!(Name, *level, "test"),
            InfoFlag::Copy => info_log!(Copy, *level, "test"),
            InfoFlag::Del => info_log!(Del, *level, "test"),
            InfoFlag::Stats => info_log!(Stats, *level, "test"),
            _ => {}
        }
    }
    let events0 = drain_events();

    // Count messages at level 1
    let config1 = VerbosityConfig::from_verbose_level(1);
    init(config1);
    drain_events();

    for (flag, level) in &test_messages {
        match flag {
            InfoFlag::Name => info_log!(Name, *level, "test"),
            InfoFlag::Copy => info_log!(Copy, *level, "test"),
            InfoFlag::Del => info_log!(Del, *level, "test"),
            InfoFlag::Stats => info_log!(Stats, *level, "test"),
            _ => {}
        }
    }
    let events1 = drain_events();

    // Level 0 should filter more messages
    assert!(events0.len() < events1.len());
}

/// Verifies debug messages only appear at level 2+.
#[test]
fn debug_messages_require_level_2_or_higher() {
    let debug_messages = vec![
        (DebugFlag::Recv, 1),
        (DebugFlag::Send, 1),
        (DebugFlag::Deltasum, 1),
        (DebugFlag::Flist, 1),
    ];

    // Level 0 and 1 should filter all debug
    for level in 0..=1 {
        let config = VerbosityConfig::from_verbose_level(level);
        init(config);
        drain_events();

        for (flag, msg_level) in &debug_messages {
            match flag {
                DebugFlag::Recv => debug_log!(Recv, *msg_level, "test"),
                DebugFlag::Send => debug_log!(Send, *msg_level, "test"),
                DebugFlag::Deltasum => debug_log!(Deltasum, *msg_level, "test"),
                DebugFlag::Flist => debug_log!(Flist, *msg_level, "test"),
                _ => {}
            }
        }

        let events = drain_events();
        assert_eq!(events.len(), 0, "Level {level} should filter all debug");
    }

    // Level 2 should allow debug
    let config2 = VerbosityConfig::from_verbose_level(2);
    init(config2);
    drain_events();

    for (flag, msg_level) in &debug_messages {
        match flag {
            DebugFlag::Recv => debug_log!(Recv, *msg_level, "test"),
            DebugFlag::Send => debug_log!(Send, *msg_level, "test"),
            DebugFlag::Deltasum => debug_log!(Deltasum, *msg_level, "test"),
            DebugFlag::Flist => debug_log!(Flist, *msg_level, "test"),
            _ => {}
        }
    }

    let events2 = drain_events();
    assert!(!events2.is_empty(), "Level 2 should allow debug messages");
}

/// Verifies progressive filtering with specific deltasum levels.
#[test]
fn progressive_filtering_deltasum_example() {
    // Level 2: deltasum=1
    let config2 = VerbosityConfig::from_verbose_level(2);
    init(config2);
    drain_events();

    debug_log!(Deltasum, 1, "level 1");
    debug_log!(Deltasum, 2, "level 2");
    debug_log!(Deltasum, 3, "level 3");

    let events2 = drain_events();
    assert_eq!(events2.len(), 1); // Only deltasum level 1

    // Level 3: deltasum=2
    let config3 = VerbosityConfig::from_verbose_level(3);
    init(config3);
    drain_events();

    debug_log!(Deltasum, 1, "level 1");
    debug_log!(Deltasum, 2, "level 2");
    debug_log!(Deltasum, 3, "level 3");

    let events3 = drain_events();
    assert_eq!(events3.len(), 2); // Deltasum levels 1 and 2

    // Level 4: deltasum=3
    let config4 = VerbosityConfig::from_verbose_level(4);
    init(config4);
    drain_events();

    debug_log!(Deltasum, 1, "level 1");
    debug_log!(Deltasum, 2, "level 2");
    debug_log!(Deltasum, 3, "level 3");

    let events4 = drain_events();
    assert_eq!(events4.len(), 3); // Deltasum levels 1, 2, and 3

    // Level 5: deltasum=4
    let config5 = VerbosityConfig::from_verbose_level(5);
    init(config5);
    drain_events();

    debug_log!(Deltasum, 1, "level 1");
    debug_log!(Deltasum, 2, "level 2");
    debug_log!(Deltasum, 3, "level 3");
    debug_log!(Deltasum, 4, "level 4");

    let events5 = drain_events();
    assert_eq!(events5.len(), 4); // All deltasum levels
}
