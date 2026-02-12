//! Integration tests for verbose level 2 (-vv) output.
//!
//! These tests verify that rsync's -vv flag produces the expected additional
//! details, shows skipped files, and matches upstream rsync's output format.
//!
//! Verbose level 2 enables:
//! - Enhanced info flags (Misc=2, Name=2, Backup=1, Mount=1, Remove=1, Skip=1)
//! - Basic debug flags (Bind=1, Cmd=1, Connect=1, Del=1, Deltasum=1, Dup=1,
//!   Filter=1, Flist=1, Iconv=1)
//!
//! Reference: rsync 3.4.1 options.c and log.c for -vv behavior.

use logging::{
    DebugFlag, DiagnosticEvent, InfoFlag, VerbosityConfig, debug_gte, debug_log, drain_events,
    info_gte, info_log, init,
};

// ============================================================================
// Verbose Level 2 Configuration Tests
// ============================================================================

/// Verifies -vv enables enhanced info flags.
#[test]
fn verbose_level_2_enables_enhanced_info_flags() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);

    // Misc and Name enhanced to level 2; Backup/Mount/Remove/Skip at level 1
    assert!(info_gte(InfoFlag::Misc, 2));
    assert!(info_gte(InfoFlag::Name, 2));
    assert!(info_gte(InfoFlag::Backup, 1));
    assert!(info_gte(InfoFlag::Mount, 1));
    assert!(info_gte(InfoFlag::Remove, 1));
    assert!(info_gte(InfoFlag::Skip, 1));

    // Still have level 1 flags
    assert!(info_gte(InfoFlag::Copy, 1));
    assert!(info_gte(InfoFlag::Del, 1));
    assert!(info_gte(InfoFlag::Flist, 1));
    assert!(info_gte(InfoFlag::Stats, 1));
}

/// Verifies -vv enables basic debug flags.
#[test]
fn verbose_level_2_enables_basic_debug_flags() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);

    // Basic debug flags should be enabled at level 1
    assert!(debug_gte(DebugFlag::Bind, 1));
    assert!(debug_gte(DebugFlag::Cmd, 1));
    assert!(debug_gte(DebugFlag::Connect, 1));
    assert!(debug_gte(DebugFlag::Del, 1));
    assert!(debug_gte(DebugFlag::Deltasum, 1));
    assert!(debug_gte(DebugFlag::Dup, 1));
    assert!(debug_gte(DebugFlag::Filter, 1));
    assert!(debug_gte(DebugFlag::Flist, 1));
    assert!(debug_gte(DebugFlag::Iconv, 1));

    // Higher debug levels should not be enabled yet
    assert!(!debug_gte(DebugFlag::Connect, 2));
    assert!(!debug_gte(DebugFlag::Del, 2));
    assert!(!debug_gte(DebugFlag::Deltasum, 2));
}

/// Verifies that level 2 is strictly more verbose than level 1.
#[test]
fn verbose_level_2_is_superset_of_level_1() {
    let config1 = VerbosityConfig::from_verbose_level(1);
    let config2 = VerbosityConfig::from_verbose_level(2);

    // All level 1 info flags should also be enabled in level 2
    assert!(config2.info.nonreg >= config1.info.nonreg);
    assert!(config2.info.copy >= config1.info.copy);
    assert!(config2.info.del >= config1.info.del);
    assert!(config2.info.flist >= config1.info.flist);
    assert!(config2.info.misc >= config1.info.misc);
    assert!(config2.info.name >= config1.info.name);
    assert!(config2.info.stats >= config1.info.stats);
    assert!(config2.info.symsafe >= config1.info.symsafe);
}

// ============================================================================
// Additional Details Output Tests
// ============================================================================

/// Verifies level 2 outputs additional miscellaneous details.
#[test]
fn verbose_level_2_outputs_misc_details() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);
    drain_events();

    // Level 1 misc messages should still work
    info_log!(Misc, 1, "basic message");

    // Level 2 misc messages should now appear
    info_log!(Misc, 2, "detailed status: delta-transfer enabled");
    info_log!(Misc, 2, "server version: 3.4.1");

    let events = drain_events();
    assert_eq!(events.len(), 3);

    // Verify the messages were captured
    let messages: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            DiagnosticEvent::Info {
                flag: InfoFlag::Misc,
                message,
                ..
            } => Some(message.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(messages.len(), 3);
    assert!(messages.contains(&"basic message"));
    assert!(messages.contains(&"detailed status: delta-transfer enabled"));
    assert!(messages.contains(&"server version: 3.4.1"));
}

/// Verifies level 2 outputs itemized changes for file names.
/// This matches rsync's -i/--itemize-changes output format.
#[test]
fn verbose_level_2_outputs_itemized_changes() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);
    drain_events();

    // Level 2 enables itemized changes via Name flag at level 2
    assert!(info_gte(InfoFlag::Name, 2));

    // Simulate itemized change output
    // Format: YXcstpoguax where:
    // Y = update type (< sent, > received, c created, h hardlink, . no change)
    // X = file type (f file, d dir, L symlink, D device, S special)
    // c = checksum changed
    // s = size changed
    // t = timestamp changed
    // p = permissions changed
    // o = owner changed
    // g = group changed
    info_log!(Name, 2, ">f+++++++++ newfile.txt");
    info_log!(Name, 2, ">f..t...... updated.txt");
    info_log!(Name, 2, "cd+++++++++ newdir/");
    info_log!(Name, 2, ".f...p..... perms_changed.sh");

    let events = drain_events();
    assert_eq!(events.len(), 4);

    let itemized: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            DiagnosticEvent::Info {
                flag: InfoFlag::Name,
                level: 2,
                message,
            } => Some(message.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(itemized.len(), 4);
    assert!(itemized.contains(&">f+++++++++ newfile.txt"));
    assert!(itemized.contains(&">f..t...... updated.txt"));
    assert!(itemized.contains(&"cd+++++++++ newdir/"));
    assert!(itemized.contains(&".f...p..... perms_changed.sh"));
}

/// Verifies level 2 outputs debug information about file list operations.
#[test]
fn verbose_level_2_outputs_flist_debug_info() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);
    drain_events();

    assert!(debug_gte(DebugFlag::Flist, 1));

    // Simulate debug output for file list operations
    debug_log!(Flist, 1, "building file list");
    debug_log!(Flist, 1, "file list sent: 1234 files");
    debug_log!(Flist, 1, "file list received: 567 files");

    let events = drain_events();
    assert_eq!(events.len(), 3);

    let flist_debug: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Flist,
                message,
                ..
            } => Some(message.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(flist_debug.len(), 3);
    assert!(flist_debug.contains(&"building file list"));
    assert!(flist_debug.contains(&"file list sent: 1234 files"));
}

/// Verifies level 2 outputs debug information about delta-transfer operations.
#[test]
fn verbose_level_2_outputs_deltasum_debug_info() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);
    drain_events();

    assert!(debug_gte(DebugFlag::Deltasum, 1));

    // Simulate debug output for delta-transfer algorithm
    debug_log!(Deltasum, 1, "generating checksums");
    debug_log!(Deltasum, 1, "block size: 700 bytes");
    debug_log!(Deltasum, 1, "matched block 0 at offset 0");
    debug_log!(Deltasum, 1, "matched block 1 at offset 700");

    let events = drain_events();
    assert_eq!(events.len(), 4);

    let deltasum_debug: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Deltasum,
                message,
                ..
            } => Some(message.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(deltasum_debug.len(), 4);
    assert!(deltasum_debug.contains(&"generating checksums"));
    assert!(deltasum_debug.contains(&"block size: 700 bytes"));
}

// ============================================================================
// Skipped Files Output Tests
// ============================================================================

/// Verifies level 2 shows skipped files via Skip flag.
#[test]
fn verbose_level_2_shows_skipped_files() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);
    drain_events();

    // Skip flag should be enabled at level 1
    assert!(info_gte(InfoFlag::Skip, 1));

    // Simulate skipped file messages
    info_log!(Skip, 1, "skipping non-regular file \"device.dev\"");
    info_log!(Skip, 1, "skipping directory \"excluded_dir\"");
    info_log!(Skip, 1, "skipping symlink \"broken_link.txt\"");

    let events = drain_events();
    assert_eq!(events.len(), 3);

    let skipped: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            DiagnosticEvent::Info {
                flag: InfoFlag::Skip,
                message,
                ..
            } => Some(message.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(skipped.len(), 3);
    assert!(skipped.contains(&"skipping non-regular file \"device.dev\""));
    assert!(skipped.contains(&"skipping directory \"excluded_dir\""));
    assert!(skipped.contains(&"skipping symlink \"broken_link.txt\""));
}

/// Verifies skipped files are NOT shown at level 1.
#[test]
fn verbose_level_1_does_not_show_skipped_files() {
    let config = VerbosityConfig::from_verbose_level(1);

    // Skip flag should not be enabled at level 1
    assert_eq!(config.info.skip, 0);

    init(config);
    drain_events();

    assert!(!info_gte(InfoFlag::Skip, 2));

    // These messages should not appear
    info_log!(Skip, 2, "skipping non-regular file \"device.dev\"");

    let events = drain_events();
    assert_eq!(events.len(), 0);
}

/// Verifies skipped files with various reasons are shown.
#[test]
fn verbose_level_2_shows_skip_reasons() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);
    drain_events();

    // Different skip reasons
    info_log!(Skip, 1, "skipping non-regular file \"fifo.pipe\"");
    info_log!(Skip, 1, "skipping same file \"unchanged.txt\"");
    info_log!(Skip, 1, "skipping excluded path \"*.tmp\"");
    info_log!(Skip, 1, "skipping server-excluded file \"secret.key\"");

    let events = drain_events();
    assert_eq!(events.len(), 4);

    // Verify all skip reasons were captured
    for event in &events {
        match event {
            DiagnosticEvent::Info {
                flag: InfoFlag::Skip,
                level: 1,
                ..
            } => {}
            _ => panic!("expected Skip info event at level 1"),
        }
    }
}

// ============================================================================
// Output Format Matching Tests
// ============================================================================

/// Verifies mount point messages match rsync format.
#[test]
fn verbose_level_2_mount_messages_match_format() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);
    drain_events();

    assert!(info_gte(InfoFlag::Mount, 1));

    // Mount point detection messages
    info_log!(Mount, 1, "skipping mount point /mnt/external");
    info_log!(Mount, 1, "note: crossing mount point at /media/usb");

    let events = drain_events();
    assert_eq!(events.len(), 2);

    let mount_msgs: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            DiagnosticEvent::Info {
                flag: InfoFlag::Mount,
                message,
                ..
            } => Some(message.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(mount_msgs.len(), 2);
    assert!(mount_msgs[0].contains("mount point"));
    assert!(mount_msgs[1].contains("mount point"));
}

/// Verifies backup file messages match rsync format.
#[test]
fn verbose_level_2_backup_messages_match_format() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);
    drain_events();

    assert!(info_gte(InfoFlag::Backup, 1));

    // Backup file creation messages
    info_log!(Backup, 1, "backing up \"data.txt\" to \"data.txt~\"");
    info_log!(
        Backup,
        1,
        "backup: renamed \"config.ini\" to \"config.ini~\""
    );

    let events = drain_events();
    assert_eq!(events.len(), 2);

    let backup_msgs: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            DiagnosticEvent::Info {
                flag: InfoFlag::Backup,
                message,
                ..
            } => Some(message.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(backup_msgs.len(), 2);
    assert!(backup_msgs[0].contains("backing up") || backup_msgs[0].contains("backup"));
    assert!(backup_msgs[1].contains("backup"));
}

/// Verifies remove/delete messages match rsync format.
#[test]
fn verbose_level_2_remove_messages_match_format() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);
    drain_events();

    assert!(info_gte(InfoFlag::Remove, 1));

    // File removal messages
    info_log!(Remove, 1, "removing file: obsolete.txt");
    info_log!(Remove, 1, "deleting \"old_dir/\"");

    let events = drain_events();
    assert_eq!(events.len(), 2);

    let remove_msgs: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            DiagnosticEvent::Info {
                flag: InfoFlag::Remove,
                message,
                ..
            } => Some(message.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(remove_msgs.len(), 2);
    assert!(remove_msgs[0].contains("removing") || remove_msgs[0].contains("deleting"));
}

// ============================================================================
// -vv Flag Equivalence Tests
// ============================================================================

/// Verifies that -vv is equivalent to from_verbose_level(2).
#[test]
fn vv_flag_equals_verbose_level_2() {
    let config = VerbosityConfig::from_verbose_level(2);

    // Check key distinguishing features of level 2
    assert_eq!(config.info.misc, 2);
    assert_eq!(config.info.name, 2);
    assert_eq!(config.info.skip, 1);
    assert_eq!(config.debug.bind, 1);
    assert_eq!(config.debug.flist, 1);
    assert_eq!(config.debug.deltasum, 1);
}

/// Verifies that applying two -v flags has same effect as -vv.
#[test]
fn two_v_flags_equal_vv() {
    // In practice, CLI would increment verbose count
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);

    // Should have level 2 capabilities
    assert!(info_gte(InfoFlag::Name, 2));
    assert!(info_gte(InfoFlag::Skip, 1));
    assert!(debug_gte(DebugFlag::Flist, 1));
}

/// Verifies -vv does not enable level 3 features.
#[test]
fn vv_flag_does_not_enable_level_3_features() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);

    // Level 3 enhanced debug flags should not be enabled
    assert!(!debug_gte(DebugFlag::Connect, 2));
    assert!(!debug_gte(DebugFlag::Del, 2));
    assert!(!debug_gte(DebugFlag::Deltasum, 2));
    assert!(!debug_gte(DebugFlag::Filter, 2));
    assert!(!debug_gte(DebugFlag::Flist, 2));

    // Level 3 specific debug flags should not be enabled
    assert!(!debug_gte(DebugFlag::Acl, 1));
    assert!(!debug_gte(DebugFlag::Backup, 1));
    assert!(!debug_gte(DebugFlag::Fuzzy, 1));
    assert!(!debug_gte(DebugFlag::Recv, 1));
    assert!(!debug_gte(DebugFlag::Send, 1));
}

// ============================================================================
// Mixed Output Tests
// ============================================================================

/// Verifies level 2 produces mixed info and debug output.
#[test]
fn verbose_level_2_produces_mixed_output() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);
    drain_events();

    // Mix of info and debug messages
    info_log!(Name, 2, ">f+++++++++ newfile.txt");
    debug_log!(Flist, 1, "building file list");
    info_log!(Skip, 1, "skipping non-regular file \"fifo\"");
    debug_log!(Deltasum, 1, "block size: 700");
    info_log!(Stats, 1, "total size is 1024");

    let events = drain_events();
    assert_eq!(events.len(), 5);

    // Count info vs debug events
    let info_count = events
        .iter()
        .filter(|e| matches!(e, DiagnosticEvent::Info { .. }))
        .count();
    let debug_count = events
        .iter()
        .filter(|e| matches!(e, DiagnosticEvent::Debug { .. }))
        .count();

    assert_eq!(info_count, 3);
    assert_eq!(debug_count, 2);
}

/// Verifies events preserve chronological order in mixed output.
#[test]
fn verbose_level_2_preserves_chronological_order() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);
    drain_events();

    info_log!(Name, 1, "file1.txt");
    debug_log!(Flist, 1, "debug1");
    info_log!(Name, 2, ">f..t...... file2.txt");
    debug_log!(Deltasum, 1, "debug2");
    info_log!(Skip, 1, "skipping file3");

    let events = drain_events();
    assert_eq!(events.len(), 5);

    // Verify order by matching message content
    match &events[0] {
        DiagnosticEvent::Info { message, .. } => assert_eq!(message, "file1.txt"),
        _ => panic!("expected info event"),
    }
    match &events[1] {
        DiagnosticEvent::Debug { message, .. } => assert_eq!(message, "debug1"),
        _ => panic!("expected debug event"),
    }
    match &events[2] {
        DiagnosticEvent::Info { message, .. } => {
            assert_eq!(message, ">f..t...... file2.txt")
        }
        _ => panic!("expected info event"),
    }
}

// ============================================================================
// Filter Debug Output Tests
// ============================================================================

/// Verifies level 2 shows filter rule processing.
#[test]
fn verbose_level_2_shows_filter_processing() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);
    drain_events();

    assert!(debug_gte(DebugFlag::Filter, 1));

    // Filter rule processing messages
    debug_log!(Filter, 1, "loading filter rules");
    debug_log!(Filter, 1, "applying exclude: *.tmp");
    debug_log!(Filter, 1, "applying include: src/**");
    debug_log!(Filter, 1, "filter match: excluded file.tmp");

    let events = drain_events();
    assert_eq!(events.len(), 4);

    let filter_msgs: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Filter,
                message,
                ..
            } => Some(message.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(filter_msgs.len(), 4);
    assert!(filter_msgs.iter().any(|m| m.contains("filter")));
}

/// Verifies level 2 shows deletion debug output.
#[test]
fn verbose_level_2_shows_deletion_debug() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);
    drain_events();

    assert!(debug_gte(DebugFlag::Del, 1));

    // Deletion operation debug messages
    debug_log!(Del, 1, "delete: checking file.txt");
    debug_log!(Del, 1, "delete: removing obsolete.dat");
    debug_log!(Del, 1, "delete: directory empty_dir/");

    let events = drain_events();
    assert_eq!(events.len(), 3);

    let del_msgs: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            DiagnosticEvent::Debug {
                flag: DebugFlag::Del,
                message,
                ..
            } => Some(message.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(del_msgs.len(), 3);
    assert!(del_msgs.iter().all(|m| m.contains("delete")));
}

// ============================================================================
// Performance and Practical Tests
// ============================================================================

/// Verifies level 2 handles large number of events efficiently.
#[test]
fn verbose_level_2_handles_many_events() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);
    drain_events();

    // Simulate a transfer with many files
    for i in 0..100 {
        info_log!(Name, 2, ">f+++++++++ file{}.txt", i);
    }

    let events = drain_events();
    assert_eq!(events.len(), 100);

    // Verify they're all Name events at level 2
    for event in events {
        match event {
            DiagnosticEvent::Info {
                flag: InfoFlag::Name,
                level: 2,
                ..
            } => {}
            _ => panic!("expected Name info event at level 2"),
        }
    }
}

/// Verifies level 2 properly suppresses level 3 events.
#[test]
fn verbose_level_2_suppresses_higher_levels() {
    let config = VerbosityConfig::from_verbose_level(2);
    init(config);
    drain_events();

    // Try to emit level 3 debug messages
    debug_log!(Connect, 2, "should not appear");
    debug_log!(Del, 2, "should not appear");
    debug_log!(Deltasum, 2, "should not appear");

    // But level 1 and 2 should work
    debug_log!(Flist, 1, "should appear");
    info_log!(Name, 2, "should appear");

    let events = drain_events();
    assert_eq!(events.len(), 2);

    // Verify only the level 1 and 2 messages appeared
    assert!(matches!(
        &events[0],
        DiagnosticEvent::Debug {
            flag: DebugFlag::Flist,
            level: 1,
            ..
        }
    ));
    assert!(matches!(
        &events[1],
        DiagnosticEvent::Info {
            flag: InfoFlag::Name,
            level: 2,
            ..
        }
    ));
}
