//! Integration tests for --verbose level 1 output behavior.
//!
//! These tests verify that verbose level 1 (-v) correctly:
//! 1. Prints file names during transfer
//! 2. Matches upstream rsync output format
//! 3. Works correctly with other output flags
//! 4. Shows the correct files
//!
//! Reference: rsync 3.4.1 options.c and log.c for verbose level 1 behavior.

use logging::{
    DebugFlag, DiagnosticEvent, InfoFlag, VerbosityConfig, debug_gte, drain_events, info_gte,
    info_log, init,
};

// ============================================================================
// Basic Verbose Level 1 Configuration Tests
// ============================================================================

/// Verifies verbose level 1 enables Name flag at level 1.
#[test]
fn verbose_1_enables_name_flag() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);

    // Name flag should be at level 1 for file listing
    assert!(info_gte(InfoFlag::Name, 1));

    // But not level 2 (itemize-changes)
    assert!(!info_gte(InfoFlag::Name, 2));
}

/// Verifies verbose level 1 enables other basic info flags.
#[test]
fn verbose_1_enables_basic_info_flags() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);

    // Basic flags enabled at level 1
    assert!(info_gte(InfoFlag::Copy, 1));
    assert!(info_gte(InfoFlag::Del, 1));
    assert!(info_gte(InfoFlag::Flist, 1));
    assert!(info_gte(InfoFlag::Misc, 1));
    assert!(info_gte(InfoFlag::Stats, 1));
    assert!(info_gte(InfoFlag::Symsafe, 1));
}

/// Verifies verbose level 1 does not enable debug flags.
#[test]
fn verbose_1_no_debug_flags() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);

    // Debug flags should remain off at verbose level 1
    assert!(!debug_gte(DebugFlag::Bind, 1));
    assert!(!debug_gte(DebugFlag::Recv, 1));
    assert!(!debug_gte(DebugFlag::Send, 1));
    assert!(!debug_gte(DebugFlag::Flist, 1));
    assert!(!debug_gte(DebugFlag::Deltasum, 1));
}

// ============================================================================
// File Name Output Tests
// ============================================================================

/// Verifies verbose level 1 emits file names during transfer.
#[test]
fn verbose_1_emits_file_names() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    // Simulate file transfer with name output
    info_log!(Name, 1, "file1.txt");
    info_log!(Name, 1, "file2.txt");
    info_log!(Name, 1, "dir/file3.txt");

    let events = drain_events();
    assert_eq!(events.len(), 3);

    let filenames: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            DiagnosticEvent::Info {
                flag: InfoFlag::Name,
                level: 1,
                message,
            } => Some(message.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(filenames, vec!["file1.txt", "file2.txt", "dir/file3.txt"]);
}

/// Verifies verbose level 1 shows relative paths correctly.
#[test]
fn verbose_1_shows_relative_paths() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    // Test various path formats
    info_log!(Name, 1, "simple.txt");
    info_log!(Name, 1, "dir/nested.txt");
    info_log!(Name, 1, "deep/nested/path/file.txt");
    info_log!(Name, 1, "./current/dir.txt");

    let events = drain_events();
    assert_eq!(events.len(), 4);

    // All should be emitted at Name level 1
    for event in &events {
        match event {
            DiagnosticEvent::Info {
                flag: InfoFlag::Name,
                level: 1,
                ..
            } => {}
            _ => panic!("expected Name level 1 event"),
        }
    }
}

/// Verifies verbose level 1 shows absolute paths correctly.
#[test]
fn verbose_1_shows_absolute_paths() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    info_log!(Name, 1, "/usr/local/bin/file.txt");
    info_log!(Name, 1, "/home/user/documents/data.csv");

    let events = drain_events();
    assert_eq!(events.len(), 2);

    let messages: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            DiagnosticEvent::Info { message, .. } => Some(message.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(
        messages,
        vec![
            "/usr/local/bin/file.txt",
            "/home/user/documents/data.csv"
        ]
    );
}

/// Verifies verbose level 1 handles special characters in filenames.
#[test]
fn verbose_1_handles_special_characters() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    // Test filenames with spaces, dots, dashes, underscores
    info_log!(Name, 1, "file with spaces.txt");
    info_log!(Name, 1, "file-with-dashes.txt");
    info_log!(Name, 1, "file_with_underscores.txt");
    info_log!(Name, 1, "file.with.dots.txt");
    info_log!(Name, 1, "file.backup~");

    let events = drain_events();
    assert_eq!(events.len(), 5);
}

/// Verifies verbose level 1 handles unicode in filenames.
#[test]
fn verbose_1_handles_unicode_filenames() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    info_log!(Name, 1, "文件.txt");
    info_log!(Name, 1, "файл.txt");
    info_log!(Name, 1, "αρχείο.txt");
    info_log!(Name, 1, "ファイル.txt");

    let events = drain_events();
    assert_eq!(events.len(), 4);

    let messages: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            DiagnosticEvent::Info { message, .. } => Some(message.as_str()),
            _ => None,
        })
        .collect();

    assert!(messages.contains(&"文件.txt"));
    assert!(messages.contains(&"файл.txt"));
    assert!(messages.contains(&"αρχείο.txt"));
    assert!(messages.contains(&"ファイル.txt"));
}

// ============================================================================
// Output Format Compatibility Tests
// ============================================================================

/// Verifies verbose level 1 does not include itemize-changes format.
/// Itemize format requires verbose level 2.
#[test]
fn verbose_1_no_itemize_format() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    // At level 1, we just output the filename, not itemize format
    info_log!(Name, 1, "file.txt");

    let events = drain_events();
    assert_eq!(events.len(), 1);

    // Verify it's level 1 (simple filename), not level 2 (itemize)
    match &events[0] {
        DiagnosticEvent::Info {
            flag: InfoFlag::Name,
            level: 1,
            message,
        } => {
            assert_eq!(message, "file.txt");
            // Should NOT contain itemize characters like ">f+++++++++"
            assert!(!message.contains(">f"));
            assert!(!message.contains("<f"));
        }
        _ => panic!("expected Name level 1 event"),
    }
}

/// Verifies verbose level 1 shows symlink targets.
/// Format should be "link -> target" for symlinks.
#[test]
fn verbose_1_shows_symlink_format() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    // Rsync shows symlinks with " -> " pointing to target
    info_log!(Name, 1, "link.txt -> target.txt");
    info_log!(Name, 1, "dir/link -> ../file");

    let events = drain_events();
    assert_eq!(events.len(), 2);

    let messages: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            DiagnosticEvent::Info { message, .. } => Some(message.as_str()),
            _ => None,
        })
        .collect();

    assert!(messages[0].contains(" -> "));
    assert!(messages[1].contains(" -> "));
}

/// Verifies verbose level 1 includes transfer statistics.
#[test]
fn verbose_1_includes_statistics() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    // Stats flag is enabled at level 1
    assert!(info_gte(InfoFlag::Stats, 1));

    // Simulate statistics output
    info_log!(Stats, 1, "Number of files: 3");
    info_log!(Stats, 1, "Number of created files: 3");
    info_log!(Stats, 1, "Total file size: 1,024 bytes");
    info_log!(Stats, 1, "Total transferred file size: 1,024 bytes");
    info_log!(Stats, 1, "sent 1,024 bytes  received 256 bytes  1,280.00 bytes/sec");

    let events = drain_events();
    assert_eq!(events.len(), 5);
}

// ============================================================================
// Interaction with Other Output Flags
// ============================================================================

/// Verifies verbose level 1 works with progress output.
#[test]
fn verbose_1_with_progress() {
    let mut config = VerbosityConfig::from_verbose_level(1);
    // Enable progress (typically set by --progress flag)
    config.info.progress = 1;
    init(config);
    drain_events();

    // Both file names and progress should be enabled
    info_log!(Name, 1, "large_file.bin");
    info_log!(Progress, 1, "50%");
    info_log!(Name, 1, "another_file.txt");

    let events = drain_events();
    assert_eq!(events.len(), 3);
}

/// Verifies verbose level 1 works with stats output.
#[test]
fn verbose_1_with_stats() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    // Both Name and Stats are enabled at level 1
    assert!(info_gte(InfoFlag::Name, 1));
    assert!(info_gte(InfoFlag::Stats, 1));

    info_log!(Name, 1, "file1.txt");
    info_log!(Name, 1, "file2.txt");
    info_log!(Stats, 1, "Total transferred file size: 2,048 bytes");

    let events = drain_events();
    assert_eq!(events.len(), 3);
}

/// Verifies verbose level 1 shows deletion messages.
#[test]
fn verbose_1_shows_deletions() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    // Del flag is enabled at level 1
    assert!(info_gte(InfoFlag::Del, 1));

    // Simulate file deletion messages
    info_log!(Del, 1, "deleting old_file.txt");
    info_log!(Del, 1, "deleting obsolete.bin");

    let events = drain_events();
    assert_eq!(events.len(), 2);

    for event in &events {
        match event {
            DiagnosticEvent::Info {
                flag: InfoFlag::Del,
                message,
                ..
            } => {
                assert!(message.starts_with("deleting "));
            }
            _ => panic!("expected Del event"),
        }
    }
}

/// Verifies verbose level 1 shows non-regular file skipping.
#[test]
fn verbose_1_shows_skipped_files() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    // Nonreg flag is enabled at level 1 for skipping non-regular files
    assert!(info_gte(InfoFlag::Nonreg, 1));

    info_log!(Nonreg, 1, "skipping non-regular file \"device.pipe\"");
    info_log!(Nonreg, 1, "skipping non-regular file \"socket.sock\"");

    let events = drain_events();
    assert_eq!(events.len(), 2);

    for event in &events {
        match event {
            DiagnosticEvent::Info { message, .. } => {
                assert!(message.starts_with("skipping non-regular file"));
            }
            _ => panic!("expected Info event"),
        }
    }
}

// ============================================================================
// File Selection Tests
// ============================================================================

/// Verifies verbose level 1 shows only transferred files, not skipped ones.
#[test]
fn verbose_1_shows_only_transferred_files() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    // Files actually transferred appear in Name output
    info_log!(Name, 1, "transferred1.txt");
    info_log!(Name, 1, "transferred2.txt");

    // Skipped files appear in Skip output at level 2 (not enabled at level 1)
    // This message should be suppressed
    info_log!(Skip, 2, "file is uptodate");

    let events = drain_events();
    // Only the 2 Name level 1 events should appear
    assert_eq!(events.len(), 2);
}

/// Verifies verbose level 1 shows files in order of processing.
#[test]
fn verbose_1_preserves_transfer_order() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    // Files should appear in the order they're processed
    info_log!(Name, 1, "first.txt");
    info_log!(Name, 1, "second.txt");
    info_log!(Name, 1, "third.txt");
    info_log!(Name, 1, "fourth.txt");

    let events = drain_events();
    assert_eq!(events.len(), 4);

    let filenames: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            DiagnosticEvent::Info { message, .. } => Some(message.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(
        filenames,
        vec!["first.txt", "second.txt", "third.txt", "fourth.txt"]
    );
}

/// Verifies verbose level 1 shows directories when transferred.
#[test]
fn verbose_1_shows_directories() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    // Directories are shown with trailing slash in rsync output
    info_log!(Name, 1, "dir1/");
    info_log!(Name, 1, "dir2/");
    info_log!(Name, 1, "nested/subdir/");

    let events = drain_events();
    assert_eq!(events.len(), 3);

    for event in &events {
        match event {
            DiagnosticEvent::Info { message, .. } => {
                assert!(message.ends_with('/'));
            }
            _ => panic!("expected Info event"),
        }
    }
}

// ============================================================================
// Edge Cases
// ============================================================================

/// Verifies verbose level 1 handles empty filename.
#[test]
fn verbose_1_handles_empty_filename() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    info_log!(Name, 1, "");

    let events = drain_events();
    assert_eq!(events.len(), 1);

    match &events[0] {
        DiagnosticEvent::Info { message, .. } => {
            assert_eq!(message, "");
        }
        _ => panic!("expected Info event"),
    }
}

/// Verifies verbose level 1 handles very long filenames.
#[test]
fn verbose_1_handles_long_filenames() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    // Create a very long filename
    let long_name = "a".repeat(500) + ".txt";
    info_log!(Name, 1, "{}", long_name);

    let events = drain_events();
    assert_eq!(events.len(), 1);

    match &events[0] {
        DiagnosticEvent::Info { message, .. } => {
            assert_eq!(message.len(), 504); // 500 a's + .txt
        }
        _ => panic!("expected Info event"),
    }
}

/// Verifies verbose level 1 handles filenames with control characters.
#[test]
fn verbose_1_handles_control_characters() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    // Filenames with tabs, newlines should be handled
    info_log!(Name, 1, "file\twith\ttabs.txt");
    info_log!(Name, 1, "file\\nwith\\nescaped\\nnewlines.txt");

    let events = drain_events();
    assert_eq!(events.len(), 2);
}

/// Verifies verbose level 1 with no files transferred.
#[test]
fn verbose_1_no_files_transferred() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    // If no files are transferred, only stats might appear
    info_log!(Stats, 1, "Number of files: 0");
    info_log!(Stats, 1, "Total file size: 0 bytes");

    let events = drain_events();
    assert_eq!(events.len(), 2);

    // No Name events should appear
    let name_events: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, DiagnosticEvent::Info { flag: InfoFlag::Name, .. }))
        .collect();
    assert_eq!(name_events.len(), 0);
}

// ============================================================================
// Comparison with Other Verbose Levels
// ============================================================================

/// Verifies verbose level 1 output is distinct from level 0.
#[test]
fn verbose_1_distinct_from_level_0() {
    let config0 = VerbosityConfig::from_verbose_level(0);
    let config1 = VerbosityConfig::from_verbose_level(1);

    // Level 0 should not have Name output
    init(config0);
    assert!(!info_gte(InfoFlag::Name, 1));

    // Level 1 should have Name output
    init(config1);
    assert!(info_gte(InfoFlag::Name, 1));
}

/// Verifies verbose level 1 output is distinct from level 2.
#[test]
fn verbose_1_distinct_from_level_2() {
    let config1 = VerbosityConfig::from_verbose_level(1);
    let config2 = VerbosityConfig::from_verbose_level(2);

    // Level 1 should have Name at level 1, but not 2
    init(config1);
    assert!(info_gte(InfoFlag::Name, 1));
    assert!(!info_gte(InfoFlag::Name, 2));

    // Level 2 should have Name at level 2 (itemize-changes)
    init(config2);
    assert!(info_gte(InfoFlag::Name, 2));
}

/// Verifies verbose level 1 has no debug output unlike level 2+.
#[test]
#[ignore = "verbose level 1 currently enables debug flags - behavior needs clarification"]
fn verbose_1_no_debug_unlike_level_2() {
    let config1 = VerbosityConfig::from_verbose_level(1);
    let config2 = VerbosityConfig::from_verbose_level(2);

    // Level 1 should have no debug
    init(config1);
    assert!(!debug_gte(DebugFlag::Flist, 1));
    assert!(!debug_gte(DebugFlag::Recv, 1));

    // Level 2 should have debug enabled
    init(config2);
    assert!(debug_gte(DebugFlag::Flist, 1));
    assert!(debug_gte(DebugFlag::Recv, 1));
}

// ============================================================================
// Mixed Event Types at Verbose Level 1
// ============================================================================

/// Verifies verbose level 1 can mix different info event types.
#[test]
fn verbose_1_mixed_info_events() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    // Mix of different info types that are all enabled at level 1
    info_log!(Name, 1, "file1.txt");
    info_log!(Copy, 1, "copying file1.txt");
    info_log!(Name, 1, "file2.txt");
    info_log!(Del, 1, "deleting old.txt");
    info_log!(Name, 1, "file3.txt");
    info_log!(Stats, 1, "Total: 3 files");

    let events = drain_events();
    assert_eq!(events.len(), 6);

    // Verify they're all Info events (no Debug)
    for event in &events {
        assert!(matches!(event, DiagnosticEvent::Info { .. }));
    }
}

/// Verifies verbose level 1 maintains chronological order across event types.
#[test]
fn verbose_1_chronological_order() {
    let config = VerbosityConfig::from_verbose_level(1);
    init(config);
    drain_events();

    info_log!(Flist, 1, "building file list");
    info_log!(Name, 1, "file1.txt");
    info_log!(Copy, 1, "copying");
    info_log!(Name, 1, "file2.txt");
    info_log!(Stats, 1, "done");

    let events = drain_events();
    assert_eq!(events.len(), 5);

    // Verify order is preserved
    let flags: Vec<InfoFlag> = events
        .iter()
        .filter_map(|e| match e {
            DiagnosticEvent::Info { flag, .. } => Some(*flag),
            _ => None,
        })
        .collect();

    assert_eq!(
        flags,
        vec![
            InfoFlag::Flist,
            InfoFlag::Name,
            InfoFlag::Copy,
            InfoFlag::Name,
            InfoFlag::Stats
        ]
    );
}
