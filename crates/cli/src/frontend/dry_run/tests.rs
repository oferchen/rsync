use super::*;

#[test]
fn action_path_returns_send_file_path() {
    let action = DryRunAction::SendFile {
        path: "file.txt".to_string(),
        size: 100,
    };
    assert_eq!(action.path(), "file.txt");
}

#[test]
fn action_path_returns_receive_file_path() {
    let action = DryRunAction::ReceiveFile {
        path: "file.txt".to_string(),
        size: 100,
    };
    assert_eq!(action.path(), "file.txt");
}

#[test]
fn action_path_returns_delete_file_path() {
    let action = DryRunAction::DeleteFile {
        path: "old.txt".to_string(),
    };
    assert_eq!(action.path(), "old.txt");
}

#[test]
fn action_path_returns_create_dir_path() {
    let action = DryRunAction::CreateDir {
        path: "subdir/".to_string(),
    };
    assert_eq!(action.path(), "subdir/");
}

#[test]
fn action_size_returns_send_file_size() {
    let action = DryRunAction::SendFile {
        path: "file.txt".to_string(),
        size: 1024,
    };
    assert_eq!(action.size(), Some(1024));
}

#[test]
fn action_size_returns_receive_file_size() {
    let action = DryRunAction::ReceiveFile {
        path: "file.txt".to_string(),
        size: 2048,
    };
    assert_eq!(action.size(), Some(2048));
}

#[test]
fn action_size_returns_none_for_delete() {
    let action = DryRunAction::DeleteFile {
        path: "old.txt".to_string(),
    };
    assert_eq!(action.size(), None);
}

#[test]
fn action_size_returns_none_for_create_dir() {
    let action = DryRunAction::CreateDir {
        path: "subdir/".to_string(),
    };
    assert_eq!(action.size(), None);
}

#[test]
fn action_is_deletion_returns_true_for_delete_file() {
    let action = DryRunAction::DeleteFile {
        path: "old.txt".to_string(),
    };
    assert!(action.is_deletion());
}

#[test]
fn action_is_deletion_returns_true_for_delete_dir() {
    let action = DryRunAction::DeleteDir {
        path: "olddir/".to_string(),
    };
    assert!(action.is_deletion());
}

#[test]
fn action_is_deletion_returns_false_for_send_file() {
    let action = DryRunAction::SendFile {
        path: "file.txt".to_string(),
        size: 100,
    };
    assert!(!action.is_deletion());
}

#[test]
fn action_is_directory_returns_true_for_create_dir() {
    let action = DryRunAction::CreateDir {
        path: "subdir/".to_string(),
    };
    assert!(action.is_directory());
}

#[test]
fn action_is_directory_returns_true_for_delete_dir() {
    let action = DryRunAction::DeleteDir {
        path: "olddir/".to_string(),
    };
    assert!(action.is_directory());
}

#[test]
fn action_is_directory_returns_false_for_send_file() {
    let action = DryRunAction::SendFile {
        path: "file.txt".to_string(),
        size: 100,
    };
    assert!(!action.is_directory());
}

#[test]
fn summary_new_creates_empty_summary() {
    let summary = DryRunSummary::new();
    assert_eq!(summary.action_count(), 0);
    assert_eq!(summary.total_size(), 0);
}

#[test]
fn summary_default_is_same_as_new() {
    assert_eq!(DryRunSummary::default(), DryRunSummary::new());
}

#[test]
fn summary_add_action_increments_count() {
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::SendFile {
        path: "file.txt".to_string(),
        size: 100,
    });
    assert_eq!(summary.action_count(), 1);
}

#[test]
fn summary_add_action_updates_total_size() {
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::SendFile {
        path: "file1.txt".to_string(),
        size: 100,
    });
    summary.add_action(DryRunAction::ReceiveFile {
        path: "file2.txt".to_string(),
        size: 200,
    });
    assert_eq!(summary.total_size(), 300);
}

#[test]
fn summary_add_action_ignores_size_for_delete() {
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::DeleteFile {
        path: "old.txt".to_string(),
    });
    assert_eq!(summary.total_size(), 0);
}

#[test]
fn summary_actions_returns_all_actions() {
    let mut summary = DryRunSummary::new();
    let action1 = DryRunAction::SendFile {
        path: "file1.txt".to_string(),
        size: 100,
    };
    let action2 = DryRunAction::DeleteFile {
        path: "old.txt".to_string(),
    };
    summary.add_action(action1.clone());
    summary.add_action(action2.clone());
    assert_eq!(summary.actions(), &[action1, action2]);
}

#[test]
fn summary_format_output_verbosity_zero_returns_empty() {
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::SendFile {
        path: "file.txt".to_string(),
        size: 100,
    });
    assert_eq!(summary.format_output(0), "");
}

#[test]
fn summary_format_output_shows_send_file() {
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::SendFile {
        path: "file.txt".to_string(),
        size: 100,
    });
    let output = summary.format_output(1);
    assert!(output.contains("file.txt"));
}

#[test]
fn summary_format_output_shows_receive_file() {
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::ReceiveFile {
        path: "file.txt".to_string(),
        size: 100,
    });
    let output = summary.format_output(1);
    assert!(output.contains("file.txt"));
}

#[test]
fn summary_format_output_shows_delete_with_prefix() {
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::DeleteFile {
        path: "old.txt".to_string(),
    });
    let output = summary.format_output(1);
    assert!(output.contains("deleting old.txt"));
}

#[test]
fn summary_format_output_shows_create_dir() {
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::CreateDir {
        path: "subdir/".to_string(),
    });
    let output = summary.format_output(1);
    assert!(output.contains("subdir/"));
}

#[test]
fn summary_format_output_shows_symlink_at_verbosity_two() {
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::CreateSymlink {
        path: "link".to_string(),
        target: "target".to_string(),
    });
    let output = summary.format_output(2);
    assert!(output.contains("link -> target"));
}

#[test]
fn summary_format_output_shows_symlink_without_target_at_verbosity_one() {
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::CreateSymlink {
        path: "link".to_string(),
        target: "target".to_string(),
    });
    let output = summary.format_output(1);
    assert!(output.contains("link\n"));
    assert!(!output.contains("->"));
}

#[test]
fn summary_format_output_shows_hardlink_at_verbosity_two() {
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::CreateHardlink {
        path: "link".to_string(),
        target: "target".to_string(),
    });
    let output = summary.format_output(2);
    assert!(output.contains("link => target"));
}

#[test]
fn summary_format_output_hides_perms_at_verbosity_one() {
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::UpdatePerms {
        path: "file.txt".to_string(),
    });
    let output = summary.format_output(1);
    assert_eq!(output, "");
}

#[test]
fn summary_format_output_shows_perms_at_verbosity_two() {
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::UpdatePerms {
        path: "file.txt".to_string(),
    });
    let output = summary.format_output(2);
    assert!(output.contains("file.txt"));
}

#[test]
fn summary_format_output_handles_multiple_actions() {
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::SendFile {
        path: "file1.txt".to_string(),
        size: 100,
    });
    summary.add_action(DryRunAction::SendFile {
        path: "file2.txt".to_string(),
        size: 200,
    });
    summary.add_action(DryRunAction::DeleteFile {
        path: "old.txt".to_string(),
    });
    let output = summary.format_output(1);
    assert!(output.contains("file1.txt"));
    assert!(output.contains("file2.txt"));
    assert!(output.contains("deleting old.txt"));
}

#[test]
fn summary_format_summary_includes_dry_run_marker() {
    let summary = DryRunSummary::new();
    let output = summary.format_summary();
    assert!(output.contains("(DRY RUN)"));
}

#[test]
fn summary_format_summary_shows_zero_bytes_sent_received() {
    let summary = DryRunSummary::new();
    let output = summary.format_summary();
    assert!(output.contains("sent 0 bytes  received 0 bytes"));
}

#[test]
fn summary_format_summary_shows_total_size() {
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::SendFile {
        path: "file.txt".to_string(),
        size: 1234,
    });
    let output = summary.format_summary();
    assert!(output.contains("total size is 1,234"));
}

#[test]
fn summary_format_summary_shows_speedup() {
    let summary = DryRunSummary::new();
    let output = summary.format_summary();
    assert!(output.contains("speedup is 0.00"));
}

#[test]
fn formatter_new_creates_formatter() {
    let formatter = DryRunFormatter::new(1);
    assert_eq!(formatter.verbosity, 1);
}

#[test]
fn formatter_format_action_returns_empty_at_verbosity_zero() {
    let formatter = DryRunFormatter::new(0);
    let action = DryRunAction::SendFile {
        path: "file.txt".to_string(),
        size: 100,
    };
    assert_eq!(formatter.format_action(&action), "");
}

#[test]
fn formatter_format_action_formats_send_file() {
    let formatter = DryRunFormatter::new(1);
    let action = DryRunAction::SendFile {
        path: "file.txt".to_string(),
        size: 100,
    };
    assert_eq!(formatter.format_action(&action), "file.txt\n");
}

#[test]
fn formatter_format_action_formats_delete_file() {
    let formatter = DryRunFormatter::new(1);
    let action = DryRunAction::DeleteFile {
        path: "old.txt".to_string(),
    };
    assert_eq!(formatter.format_action(&action), "deleting old.txt\n");
}

#[test]
fn formatter_format_action_formats_create_dir() {
    let formatter = DryRunFormatter::new(1);
    let action = DryRunAction::CreateDir {
        path: "subdir/".to_string(),
    };
    assert_eq!(formatter.format_action(&action), "subdir/\n");
}

#[test]
fn formatter_format_action_formats_symlink_with_target_at_verbosity_two() {
    let formatter = DryRunFormatter::new(2);
    let action = DryRunAction::CreateSymlink {
        path: "link".to_string(),
        target: "target".to_string(),
    };
    assert_eq!(formatter.format_action(&action), "link -> target\n");
}

#[test]
fn formatter_format_action_formats_symlink_without_target_at_verbosity_one() {
    let formatter = DryRunFormatter::new(1);
    let action = DryRunAction::CreateSymlink {
        path: "link".to_string(),
        target: "target".to_string(),
    };
    assert_eq!(formatter.format_action(&action), "link\n");
}

#[test]
fn formatter_format_actions_formats_multiple_actions() {
    let formatter = DryRunFormatter::new(1);
    let actions = vec![
        DryRunAction::SendFile {
            path: "file1.txt".to_string(),
            size: 100,
        },
        DryRunAction::SendFile {
            path: "file2.txt".to_string(),
            size: 200,
        },
    ];
    let output = formatter.format_actions(&actions);
    assert_eq!(output, "file1.txt\nfile2.txt\n");
}

#[test]
fn format_number_with_commas_handles_zero() {
    assert_eq!(format_number_with_commas(0), "0");
}

#[test]
fn format_number_with_commas_handles_small_numbers() {
    assert_eq!(format_number_with_commas(1), "1");
    assert_eq!(format_number_with_commas(12), "12");
    assert_eq!(format_number_with_commas(123), "123");
}

#[test]
fn format_number_with_commas_adds_single_comma() {
    assert_eq!(format_number_with_commas(1234), "1,234");
    assert_eq!(format_number_with_commas(9999), "9,999");
}

#[test]
fn format_number_with_commas_adds_multiple_commas() {
    assert_eq!(format_number_with_commas(1234567), "1,234,567");
    assert_eq!(format_number_with_commas(1234567890), "1,234,567,890");
}

#[test]
fn format_number_with_commas_handles_large_numbers() {
    assert_eq!(
        format_number_with_commas(9_999_999_999_999_999_999),
        "9,999,999,999,999,999,999"
    );
}

#[test]
fn summary_format_output_handles_paths_with_spaces() {
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::SendFile {
        path: "file with spaces.txt".to_string(),
        size: 100,
    });
    let output = summary.format_output(1);
    assert!(output.contains("file with spaces.txt"));
}

#[test]
fn summary_format_output_handles_paths_with_unicode() {
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::SendFile {
        path: "\u{6587}\u{4ef6}.txt".to_string(),
        size: 100,
    });
    let output = summary.format_output(1);
    assert!(output.contains("\u{6587}\u{4ef6}.txt"));
}

#[test]
fn summary_handles_large_file_sizes() {
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::SendFile {
        path: "large.bin".to_string(),
        size: 10_000_000_000,
    });
    assert_eq!(summary.total_size(), 10_000_000_000);
    let formatted = summary.format_summary();
    assert!(formatted.contains("10,000,000,000"));
}

#[test]
fn summary_handles_size_overflow_gracefully() {
    let mut summary = DryRunSummary::new();
    summary.add_action(DryRunAction::SendFile {
        path: "file1.bin".to_string(),
        size: u64::MAX,
    });
    summary.add_action(DryRunAction::SendFile {
        path: "file2.bin".to_string(),
        size: 1,
    });
    assert_eq!(summary.total_size(), u64::MAX);
}

#[test]
fn summary_format_output_empty_list_returns_empty_string() {
    let summary = DryRunSummary::new();
    let output = summary.format_output(1);
    assert_eq!(output, "");
}

#[test]
fn summary_format_summary_empty_list_shows_zero_size() {
    let summary = DryRunSummary::new();
    let output = summary.format_summary();
    assert!(output.contains("total size is 0"));
}
