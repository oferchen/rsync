#![deny(unsafe_code)]

//! Integration tests for out-format rendering: itemized changes, placeholder
//! rendering, and combined format strings.

use std::path::PathBuf;

use core::client::{ClientEntryKind, ClientEvent, ClientEventKind};
use engine::local_copy::{LocalCopyChangeSet, TimeChange};

use crate::frontend::out_format::parser::parse_out_format;
use crate::frontend::out_format::tokens::{
    HumanizeMode, OutFormatContext, PlaceholderAlignment, PlaceholderFormat,
};

use super::emit_out_format;
use super::format::format_numeric_value;
use super::itemize::format_itemized_changes;

fn make_event(
    kind: ClientEventKind,
    created: bool,
    metadata_kind: Option<ClientEntryKind>,
    change_set: LocalCopyChangeSet,
) -> ClientEvent {
    let metadata = metadata_kind.map(ClientEvent::test_metadata);
    ClientEvent::for_test(
        PathBuf::from("test.txt"),
        kind,
        created,
        metadata,
        change_set,
    )
}

//
// Upstream rsync --itemize-changes format reference:
//   YXcstpoguax  filename
//   ^^ ^^^^^^^^^ (11 characters total)
//   ||
//   |+-- X = file type: f (file), d (directory), L (symlink), D (device), S (special)
//   +--- Y = update type: > (received), c (created), h (hardlink), . (not updated), * (message)
//
//   Positions 2-10 (c s t p o g u a x):
//     '.' = attribute is unchanged
//     '+' = file is new (all attributes are new)
//     letter = attribute changed (c/s/t/T/p/o/g/u/n/b/a/x)

#[test]
fn itemize_format_length_is_eleven_for_new_file() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.len(),
        11,
        "format string should be 11 characters: {result:?}"
    );
}

#[test]
fn itemize_format_length_is_eleven_for_unchanged_file() {
    let event = make_event(
        ClientEventKind::MetadataReused,
        false,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.len(),
        11,
        "format string should be 11 characters: {result:?}"
    );
}

#[test]
fn itemize_format_length_is_eleven_for_modified_file() {
    let cs = LocalCopyChangeSet::new()
        .with_checksum_changed(true)
        .with_size_changed(true)
        .with_time_change(Some(TimeChange::Modified));
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.len(),
        11,
        "format string should be 11 characters: {result:?}"
    );
}

#[test]
fn itemize_y_position_data_copied_shows_greater_than() {
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().next(),
        Some('>'),
        "Y should be '>' for DataCopied: {result:?}"
    );
}

#[test]
fn itemize_y_position_data_copied_sender_shows_less_than() {
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, true);
    assert_eq!(
        result.chars().next(),
        Some('<'),
        "Y should be '<' for DataCopied with is_sender=true: {result:?}"
    );
}

#[test]
fn itemize_y_position_hardlink_shows_h() {
    let event = make_event(
        ClientEventKind::HardLink,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().next(),
        Some('h'),
        "Y should be 'h' for HardLink: {result:?}"
    );
}

#[test]
fn itemize_y_position_directory_created_shows_c() {
    let event = make_event(
        ClientEventKind::DirectoryCreated,
        true,
        Some(ClientEntryKind::Directory),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().next(),
        Some('c'),
        "Y should be 'c' for DirectoryCreated: {result:?}"
    );
}

#[test]
fn itemize_y_position_symlink_copied_shows_c() {
    let event = make_event(
        ClientEventKind::SymlinkCopied,
        true,
        Some(ClientEntryKind::Symlink),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().next(),
        Some('c'),
        "Y should be 'c' for SymlinkCopied: {result:?}"
    );
}

#[test]
fn itemize_y_position_fifo_copied_shows_c() {
    let event = make_event(
        ClientEventKind::FifoCopied,
        true,
        Some(ClientEntryKind::Fifo),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().next(),
        Some('c'),
        "Y should be 'c' for FifoCopied: {result:?}"
    );
}

#[test]
fn itemize_y_position_device_copied_shows_c() {
    let event = make_event(
        ClientEventKind::DeviceCopied,
        true,
        Some(ClientEntryKind::CharDevice),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().next(),
        Some('c'),
        "Y should be 'c' for DeviceCopied: {result:?}"
    );
}

#[test]
fn itemize_y_position_metadata_reused_shows_dot() {
    let event = make_event(
        ClientEventKind::MetadataReused,
        false,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().next(),
        Some('.'),
        "Y should be '.' for MetadataReused: {result:?}"
    );
}

#[test]
fn itemize_y_position_skipped_existing_shows_dot() {
    let event = make_event(
        ClientEventKind::SkippedExisting,
        false,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().next(),
        Some('.'),
        "Y should be '.' for SkippedExisting: {result:?}"
    );
}

#[test]
fn itemize_y_position_source_removed_shows_c() {
    let event = make_event(
        ClientEventKind::SourceRemoved,
        false,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().next(),
        Some('c'),
        "Y should be 'c' for SourceRemoved: {result:?}"
    );
}

#[test]
fn itemize_x_position_file_shows_f() {
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(1),
        Some('f'),
        "X should be 'f' for File: {result:?}"
    );
}

#[test]
fn itemize_x_position_directory_shows_d() {
    let event = make_event(
        ClientEventKind::DirectoryCreated,
        true,
        Some(ClientEntryKind::Directory),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(1),
        Some('d'),
        "X should be 'd' for Directory: {result:?}"
    );
}

#[test]
fn itemize_x_position_symlink_shows_l() {
    let event = make_event(
        ClientEventKind::SymlinkCopied,
        true,
        Some(ClientEntryKind::Symlink),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(1),
        Some('L'),
        "X should be 'L' for Symlink: {result:?}"
    );
}

#[test]
fn itemize_x_position_char_device_shows_d_upper() {
    let event = make_event(
        ClientEventKind::DeviceCopied,
        true,
        Some(ClientEntryKind::CharDevice),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(1),
        Some('D'),
        "X should be 'D' for CharDevice: {result:?}"
    );
}

#[test]
fn itemize_x_position_block_device_shows_d_upper() {
    let event = make_event(
        ClientEventKind::DeviceCopied,
        true,
        Some(ClientEntryKind::BlockDevice),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(1),
        Some('D'),
        "X should be 'D' for BlockDevice: {result:?}"
    );
}

#[test]
fn itemize_x_position_fifo_shows_s_upper() {
    let event = make_event(
        ClientEventKind::FifoCopied,
        true,
        Some(ClientEntryKind::Fifo),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(1),
        Some('S'),
        "X should be 'S' for Fifo: {result:?}"
    );
}

#[test]
fn itemize_x_position_socket_shows_s_upper() {
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::Socket),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(1),
        Some('S'),
        "X should be 'S' for Socket: {result:?}"
    );
}

#[test]
fn itemize_new_file_shows_all_plus_in_attribute_positions() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result, ">f+++++++++",
        "new file should be >f+++++++++: {result:?}"
    );
}

#[test]
fn itemize_new_directory_shows_all_plus_in_attribute_positions() {
    let event = make_event(
        ClientEventKind::DirectoryCreated,
        true,
        Some(ClientEntryKind::Directory),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result, "cd+++++++++",
        "new directory should be cd+++++++++: {result:?}"
    );
}

#[test]
fn itemize_new_symlink_shows_all_plus_in_attribute_positions() {
    let event = make_event(
        ClientEventKind::SymlinkCopied,
        true,
        Some(ClientEntryKind::Symlink),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result, "cL+++++++++",
        "new symlink should be cL+++++++++: {result:?}"
    );
}

#[test]
fn itemize_new_device_shows_all_plus_in_attribute_positions() {
    let event = make_event(
        ClientEventKind::DeviceCopied,
        true,
        Some(ClientEntryKind::CharDevice),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result, "cD+++++++++",
        "new device should be cD+++++++++: {result:?}"
    );
}

#[test]
fn itemize_new_fifo_shows_all_plus_in_attribute_positions() {
    let event = make_event(
        ClientEventKind::FifoCopied,
        true,
        Some(ClientEntryKind::Fifo),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result, "cS+++++++++",
        "new fifo should be cS+++++++++: {result:?}"
    );
}

#[test]
fn itemize_hardlink_shows_all_plus_in_attribute_positions() {
    let event = make_event(
        ClientEventKind::HardLink,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result, "hf+++++++++",
        "new hardlink should be hf+++++++++: {result:?}"
    );
}

#[test]
fn itemize_deleted_entry_shows_star_deleting() {
    let event = make_event(
        ClientEventKind::EntryDeleted,
        false,
        None,
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result, "*deleting  ",
        "deleted entry should be '*deleting  ' (11 chars): {result:?}"
    );
}

#[test]
fn itemize_checksum_changed_shows_c_at_position_2() {
    let cs = LocalCopyChangeSet::new().with_checksum_changed(true);
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(2),
        Some('c'),
        "position 2 should be 'c' for checksum: {result:?}"
    );
}

#[test]
fn itemize_size_changed_shows_s_at_position_3() {
    let cs = LocalCopyChangeSet::new().with_size_changed(true);
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(3),
        Some('s'),
        "position 3 should be 's' for size: {result:?}"
    );
}

#[test]
fn itemize_time_modified_shows_lowercase_t_at_position_4() {
    let cs = LocalCopyChangeSet::new().with_time_change(Some(TimeChange::Modified));
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(4),
        Some('t'),
        "position 4 should be 't' for Modified time: {result:?}"
    );
}

#[test]
fn itemize_time_transfer_shows_uppercase_t_at_position_4() {
    let cs = LocalCopyChangeSet::new().with_time_change(Some(TimeChange::TransferTime));
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(4),
        Some('T'),
        "position 4 should be 'T' for TransferTime: {result:?}"
    );
}

#[test]
fn itemize_permissions_changed_shows_p_at_position_5() {
    let cs = LocalCopyChangeSet::new().with_permissions_changed(true);
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(5),
        Some('p'),
        "position 5 should be 'p' for permissions: {result:?}"
    );
}

#[test]
fn itemize_owner_changed_shows_o_at_position_6() {
    let cs = LocalCopyChangeSet::new().with_owner_changed(true);
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(6),
        Some('o'),
        "position 6 should be 'o' for owner: {result:?}"
    );
}

#[test]
fn itemize_group_changed_shows_g_at_position_7() {
    let cs = LocalCopyChangeSet::new().with_group_changed(true);
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(7),
        Some('g'),
        "position 7 should be 'g' for group: {result:?}"
    );
}

#[test]
fn itemize_access_time_changed_shows_u_at_position_8() {
    let cs = LocalCopyChangeSet::new().with_access_time_changed(true);
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(8),
        Some('u'),
        "position 8 should be 'u' for access time: {result:?}"
    );
}

#[test]
fn itemize_create_time_changed_shows_n_at_position_8() {
    let cs = LocalCopyChangeSet::new().with_create_time_changed(true);
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(8),
        Some('n'),
        "position 8 should be 'n' for create time: {result:?}"
    );
}

#[test]
fn itemize_both_access_and_create_time_changed_shows_b_at_position_8() {
    let cs = LocalCopyChangeSet::new()
        .with_access_time_changed(true)
        .with_create_time_changed(true);
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(8),
        Some('b'),
        "position 8 should be 'b' for both times: {result:?}"
    );
}

#[test]
fn itemize_acl_changed_shows_a_at_position_9() {
    let cs = LocalCopyChangeSet::new().with_acl_changed(true);
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(9),
        Some('a'),
        "position 9 should be 'a' for ACL: {result:?}"
    );
}

#[test]
fn itemize_xattr_changed_shows_x_at_position_10() {
    let cs = LocalCopyChangeSet::new().with_xattr_changed(true);
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(10),
        Some('x'),
        "position 10 should be 'x' for xattr: {result:?}"
    );
}

#[test]
fn itemize_no_changes_shows_all_dots_in_attribute_positions() {
    let event = make_event(
        ClientEventKind::MetadataReused,
        false,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result, ".f         ",
        "no change with '.' update type should collapse dots to spaces: {result:?}"
    );
}

#[test]
fn itemize_checksum_and_size_change_shows_cs_at_positions_2_3() {
    let cs = LocalCopyChangeSet::new()
        .with_checksum_changed(true)
        .with_size_changed(true);
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        &result[..4],
        ">fcs",
        "should show '>fcs' for checksum+size: {result:?}"
    );
}

#[test]
fn itemize_full_change_shows_all_indicators() {
    let cs = LocalCopyChangeSet::new()
        .with_checksum_changed(true)
        .with_size_changed(true)
        .with_time_change(Some(TimeChange::Modified))
        .with_permissions_changed(true)
        .with_owner_changed(true)
        .with_group_changed(true)
        .with_access_time_changed(true)
        .with_create_time_changed(true)
        .with_acl_changed(true)
        .with_xattr_changed(true);
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result, ">fcstpogbax",
        "full changes should show all indicators: {result:?}"
    );
}

#[test]
fn itemize_typical_content_update_shows_cst_pattern() {
    let cs = LocalCopyChangeSet::new()
        .with_checksum_changed(true)
        .with_size_changed(true)
        .with_time_change(Some(TimeChange::Modified));
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result, ">fcst......",
        "typical update should show '>fcst......': {result:?}"
    );
}

#[test]
fn itemize_directory_timestamp_update_shows_dot_d_dot_t_pattern() {
    let cs = LocalCopyChangeSet::new().with_time_change(Some(TimeChange::Modified));
    let event = make_event(
        ClientEventKind::DirectoryCreated,
        false,
        Some(ClientEntryKind::Directory),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result, "cd..t......",
        "directory time update should show 'cd..t......': {result:?}"
    );
}

#[test]
fn itemize_permission_only_change() {
    let cs = LocalCopyChangeSet::new().with_permissions_changed(true);
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(result, ">f...p.....", "permission-only change: {result:?}");
}

#[test]
fn itemize_owner_and_group_change() {
    let cs = LocalCopyChangeSet::new()
        .with_owner_changed(true)
        .with_group_changed(true);
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(result, ">f....og...", "owner+group change: {result:?}");
}

#[test]
fn itemize_infers_file_type_from_data_copied_when_no_metadata() {
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        None,
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(1),
        Some('f'),
        "should infer 'f' for DataCopied: {result:?}"
    );
}

#[test]
fn itemize_infers_directory_type_from_directory_created_when_no_metadata() {
    let event = make_event(
        ClientEventKind::DirectoryCreated,
        true,
        None,
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(1),
        Some('d'),
        "should infer 'd' for DirectoryCreated: {result:?}"
    );
}

#[test]
fn itemize_infers_symlink_type_from_symlink_copied_when_no_metadata() {
    let event = make_event(
        ClientEventKind::SymlinkCopied,
        true,
        None,
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(1),
        Some('L'),
        "should infer 'L' for SymlinkCopied: {result:?}"
    );
}

#[test]
fn itemize_infers_fifo_type_from_fifo_copied_when_no_metadata() {
    let event = make_event(
        ClientEventKind::FifoCopied,
        true,
        None,
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(1),
        Some('S'),
        "should infer 'S' for FifoCopied: {result:?}"
    );
}

#[test]
fn itemize_infers_device_type_from_device_copied_when_no_metadata() {
    let event = make_event(
        ClientEventKind::DeviceCopied,
        true,
        None,
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(1),
        Some('D'),
        "should infer 'D' for DeviceCopied: {result:?}"
    );
}

#[test]
fn itemize_skipped_missing_destination_shows_dot() {
    let event = make_event(
        ClientEventKind::SkippedMissingDestination,
        false,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().next(),
        Some('.'),
        "SkippedMissingDestination should be '.': {result:?}"
    );
}

#[test]
fn itemize_skipped_newer_destination_shows_dot() {
    let event = make_event(
        ClientEventKind::SkippedNewerDestination,
        false,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().next(),
        Some('.'),
        "SkippedNewerDestination should be '.': {result:?}"
    );
}

#[test]
fn itemize_skipped_non_regular_shows_dot() {
    let event = make_event(
        ClientEventKind::SkippedNonRegular,
        false,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().next(),
        Some('.'),
        "SkippedNonRegular should be '.': {result:?}"
    );
}

#[test]
fn itemize_skipped_directory_shows_dot() {
    let event = make_event(
        ClientEventKind::SkippedDirectory,
        false,
        Some(ClientEntryKind::Directory),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().next(),
        Some('.'),
        "SkippedDirectory should be '.': {result:?}"
    );
}

#[test]
fn itemize_skipped_unsafe_symlink_shows_dot() {
    let event = make_event(
        ClientEventKind::SkippedUnsafeSymlink,
        false,
        Some(ClientEntryKind::Symlink),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().next(),
        Some('.'),
        "SkippedUnsafeSymlink should be '.': {result:?}"
    );
}

#[test]
fn itemize_skipped_mount_point_shows_dot() {
    let event = make_event(
        ClientEventKind::SkippedMountPoint,
        false,
        Some(ClientEntryKind::Directory),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().next(),
        Some('.'),
        "SkippedMountPoint should be '.': {result:?}"
    );
}

#[test]
fn itemize_upstream_new_regular_file_pattern() {
    // Upstream rsync: >f+++++++++ for a brand new regular file
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    assert_eq!(format_itemized_changes(&event, false), ">f+++++++++");
}

#[test]
fn itemize_upstream_new_directory_pattern() {
    // Upstream rsync: cd+++++++++ for a new directory
    let event = make_event(
        ClientEventKind::DirectoryCreated,
        true,
        Some(ClientEntryKind::Directory),
        LocalCopyChangeSet::new(),
    );
    assert_eq!(format_itemized_changes(&event, false), "cd+++++++++");
}

#[test]
fn itemize_upstream_new_symlink_pattern() {
    // Upstream rsync: cL+++++++++ for a new symlink
    let event = make_event(
        ClientEventKind::SymlinkCopied,
        true,
        Some(ClientEntryKind::Symlink),
        LocalCopyChangeSet::new(),
    );
    assert_eq!(format_itemized_changes(&event, false), "cL+++++++++");
}

#[test]
fn itemize_upstream_delete_pattern() {
    // upstream: log.c:697 - "*deleting  " padded to 11 chars
    let event = make_event(
        ClientEventKind::EntryDeleted,
        false,
        None,
        LocalCopyChangeSet::new(),
    );
    assert_eq!(format_itemized_changes(&event, false), "*deleting  ");
}

#[test]
fn itemize_upstream_unchanged_file_pattern() {
    // upstream: log.c:735-744 - all-dots collapse to spaces
    let event = make_event(
        ClientEventKind::MetadataReused,
        false,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    assert_eq!(format_itemized_changes(&event, false), ".f         ");
}

#[test]
fn itemize_upstream_content_and_time_update_pattern() {
    // Upstream rsync: >fcst...... for a file with content+size+time changes
    let cs = LocalCopyChangeSet::new()
        .with_checksum_changed(true)
        .with_size_changed(true)
        .with_time_change(Some(TimeChange::Modified));
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    assert_eq!(format_itemized_changes(&event, false), ">fcst......");
}

#[test]
fn itemize_upstream_time_only_update_pattern() {
    // Upstream rsync: >f..t...... for a file with only time change
    let cs = LocalCopyChangeSet::new().with_time_change(Some(TimeChange::Modified));
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    assert_eq!(format_itemized_changes(&event, false), ">f..t......");
}

#[test]
fn itemize_upstream_transfer_time_pattern() {
    // Upstream rsync: >f..T...... when times not preserved (capital T)
    let cs = LocalCopyChangeSet::new().with_time_change(Some(TimeChange::TransferTime));
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        cs,
    );
    assert_eq!(format_itemized_changes(&event, false), ">f..T......");
}

#[test]
fn itemize_new_file_ignores_change_set_values() {
    // When created=true, all positions 2-10 should be '+' regardless of change_set
    let cs = LocalCopyChangeSet::new()
        .with_checksum_changed(true)
        .with_size_changed(true)
        .with_permissions_changed(true);
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        cs,
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result, ">f+++++++++",
        "created=true should override change_set with '+': {result:?}"
    );
}

#[test]
fn itemize_position_8_no_time_change_shows_dot() {
    // When neither access_time nor create_time changed, position 8 should be '.'
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(8),
        Some('.'),
        "position 8 should be '.' with no time changes: {result:?}"
    );
}

#[test]
fn itemize_delete_has_eleven_char_length() {
    // upstream: log.c:697 - "*deleting  " padded to 11 chars like YXcstpoguax
    let event = make_event(
        ClientEventKind::EntryDeleted,
        false,
        None,
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(result, "*deleting  ");
    assert_eq!(result.len(), 11, "delete format should be 11 characters");
}

#[test]
fn itemize_missing_data_shows_question_marks() {
    // upstream: log.c:730-734 - ITEM_MISSING_DATA fills positions 2-10 with '?'
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new().with_missing_data(true),
    );
    assert_eq!(format_itemized_changes(&event, false), ">f?????????");
}

#[test]
fn itemize_missing_data_overrides_individual_changes() {
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new()
            .with_missing_data(true)
            .with_checksum_changed(true)
            .with_size_changed(true),
    );
    assert_eq!(format_itemized_changes(&event, false), ">f?????????");
}

#[test]
fn itemize_missing_data_length_is_eleven() {
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new().with_missing_data(true),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(result.len(), 11);
}

#[test]
fn itemize_symlink_size_always_dot_even_when_size_changed() {
    // upstream: log.c:706 - c[3] = '.' unconditionally for symlinks
    // After dots-to-spaces collapse, '.' becomes ' ' when all subsequent positions are also dots
    let event = make_event(
        ClientEventKind::SymlinkCopied,
        false,
        Some(ClientEntryKind::Symlink),
        LocalCopyChangeSet::new().with_size_changed(true),
    );
    let result = format_itemized_changes(&event, false);
    assert_ne!(
        result.chars().nth(3),
        Some('s'),
        "symlink should never report size change: {result:?}"
    );
}

#[test]
fn itemize_symlink_size_dot_with_multiple_changes() {
    // Size must stay '.' for symlinks even when other attributes change
    let event = make_event(
        ClientEventKind::SymlinkCopied,
        false,
        Some(ClientEntryKind::Symlink),
        LocalCopyChangeSet::new()
            .with_size_changed(true)
            .with_checksum_changed(true)
            .with_permissions_changed(true),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(result, "cLc..p.....");
}

#[test]
fn itemize_symlink_time_transfer_time_shows_uppercase_t() {
    // upstream: log.c:708-710 - 'T' when symlink times not preserved
    let event = make_event(
        ClientEventKind::SymlinkCopied,
        false,
        Some(ClientEntryKind::Symlink),
        LocalCopyChangeSet::new().with_time_change(Some(TimeChange::TransferTime)),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(4),
        Some('T'),
        "symlink time should be 'T' for TransferTime: {result:?}"
    );
}

#[test]
fn itemize_symlink_time_modified_shows_lowercase_t() {
    // upstream: log.c:710 - 't' when preserve_mtimes and receiver_symlink_times
    let event = make_event(
        ClientEventKind::SymlinkCopied,
        false,
        Some(ClientEntryKind::Symlink),
        LocalCopyChangeSet::new().with_time_change(Some(TimeChange::Modified)),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(4),
        Some('t'),
        "symlink time should be 't' for Modified: {result:?}"
    );
}

#[test]
fn itemize_symlink_time_unchanged_shows_dot() {
    let event = make_event(
        ClientEventKind::SymlinkCopied,
        false,
        Some(ClientEntryKind::Symlink),
        LocalCopyChangeSet::new(),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(4),
        Some(' '),
        "symlink time should be space (collapsed) when unchanged: {result:?}"
    );
}

#[test]
fn itemize_symlink_full_pattern_with_transfer_time() {
    // Typical symlink update: size always dot, time 'T', other changes reported
    let event = make_event(
        ClientEventKind::SymlinkCopied,
        false,
        Some(ClientEntryKind::Symlink),
        LocalCopyChangeSet::new()
            .with_time_change(Some(TimeChange::TransferTime))
            .with_owner_changed(true)
            .with_group_changed(true),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(result, "cL..T.og...");
}

#[test]
fn itemize_file_size_still_reports_when_changed() {
    // Verify that non-symlink files still report size changes
    let event = make_event(
        ClientEventKind::DataCopied,
        false,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new().with_size_changed(true),
    );
    let result = format_itemized_changes(&event, false);
    assert_eq!(
        result.chars().nth(3),
        Some('s'),
        "regular file should report size change: {result:?}"
    );
}

fn render_format(format_str: &str, event: &ClientEvent) -> String {
    let format = parse_out_format(std::ffi::OsStr::new(format_str)).unwrap();
    let mut output = Vec::new();
    format
        .render(event, &OutFormatContext::default(), &mut output)
        .unwrap();
    String::from_utf8(output).unwrap()
}

fn render_format_with_context(
    format_str: &str,
    event: &ClientEvent,
    context: &OutFormatContext,
) -> String {
    let format = parse_out_format(std::ffi::OsStr::new(format_str)).unwrap();
    let mut output = Vec::new();
    format.render(event, context, &mut output).unwrap();
    String::from_utf8(output).unwrap()
}

#[test]
fn render_percent_n_shows_filename() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    assert_eq!(render_format("%n", &event), "test.txt\n");
}

#[test]
fn render_percent_n_adds_trailing_slash_for_directory() {
    let metadata = Some(ClientEvent::test_metadata(ClientEntryKind::Directory));
    let event = ClientEvent::for_test(
        PathBuf::from("mydir"),
        ClientEventKind::DirectoryCreated,
        true,
        metadata,
        LocalCopyChangeSet::new(),
    );
    assert_eq!(render_format("%n", &event), "mydir/\n");
}

#[test]
fn render_percent_f_no_trailing_slash_for_directory() {
    let metadata = Some(ClientEvent::test_metadata(ClientEntryKind::Directory));
    let event = ClientEvent::for_test(
        PathBuf::from("mydir"),
        ClientEventKind::DirectoryCreated,
        true,
        metadata,
        LocalCopyChangeSet::new(),
    );
    let rendered = render_format("%f", &event);
    // %f uses render_path(event, false) so should not add trailing slash
    assert!(
        !rendered.trim().ends_with('/'),
        "%%f should not add trailing slash: {rendered:?}"
    );
}

#[test]
fn render_percent_l_shows_file_length() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    // test_metadata sets length to 0
    assert_eq!(render_format("%l", &event), "0\n");
}

#[test]
fn render_percent_b_shows_bytes_transferred() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    // for_test always sets bytes_transferred to 0
    assert_eq!(render_format("%b", &event), "0\n");
}

#[test]
fn render_percent_o_shows_operation() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    assert_eq!(render_format("%o", &event), "copied\n");
}

#[test]
fn render_percent_o_shows_deleted_for_entry_deleted() {
    let event = make_event(
        ClientEventKind::EntryDeleted,
        false,
        None,
        LocalCopyChangeSet::new(),
    );
    assert_eq!(render_format("%o", &event), "deleted\n");
}

#[test]
fn render_percent_p_shows_current_pid() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let rendered = render_format("%p", &event);
    let expected = format!("{}\n", std::process::id());
    assert_eq!(rendered, expected);
}

#[test]
fn render_percent_t_shows_timestamp_in_expected_format() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let rendered = render_format("%t", &event);
    let trimmed = rendered.trim();
    // Upstream format: yyyy/mm/dd-hh:mm:ss
    assert!(
        trimmed.len() == 19,
        "%%t should be 19 chars (yyyy/mm/dd-hh:mm:ss), got {trimmed:?}"
    );
    assert_eq!(&trimmed[4..5], "/", "position 4 should be '/'");
    assert_eq!(&trimmed[7..8], "/", "position 7 should be '/'");
    assert_eq!(&trimmed[10..11], "-", "position 10 should be '-'");
    assert_eq!(&trimmed[13..14], ":", "position 13 should be ':'");
    assert_eq!(&trimmed[16..17], ":", "position 16 should be ':'");
}

#[test]
fn render_percent_m_shows_epoch_when_no_mtime() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    assert_eq!(render_format("%M", &event), "1970/01/01-00:00:00\n");
}

#[test]
fn render_percent_u_upper_shows_uid_zero_for_test_metadata() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    assert_eq!(render_format("%U", &event), "0\n");
}

#[test]
fn render_percent_g_upper_shows_gid_zero_for_test_metadata() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    assert_eq!(render_format("%G", &event), "0\n");
}

#[test]
fn render_percent_b_upper_shows_dashes_for_test_metadata() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    // test_metadata has mode = None => "---------"
    assert_eq!(render_format("%B", &event), "---------\n");
}

#[test]
fn render_percent_l_upper_shows_nothing_for_regular_file() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    // SymlinkTarget returns None for non-symlinks, so nothing is rendered
    // But since it returns None, the placeholder is omitted entirely
    // and the output is just the newline
    assert_eq!(render_format("%L", &event), "\n");
}

#[test]
fn render_itemize_and_filename_combined_no_space() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    // %i%n with no space means the two are concatenated directly
    assert_eq!(render_format("%i%n", &event), ">f+++++++++test.txt\n");
}

#[test]
fn render_itemize_and_filename_with_space() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    // %i %n with a space between them matches upstream -i output
    assert_eq!(render_format("%i %n", &event), ">f+++++++++ test.txt\n");
}

#[test]
fn render_escaped_percent_produces_literal_percent() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    assert_eq!(render_format("%%", &event), "%\n");
}

#[test]
fn render_literal_text_with_escaped_percent() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    assert_eq!(render_format("100%%", &event), "100%\n");
}

#[test]
fn render_multiple_codes_in_format_string() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let rendered = render_format("[%n] %b bytes %o", &event);
    assert_eq!(rendered, "[test.txt] 0 bytes copied\n");
}

#[test]
fn render_literal_prefix_and_suffix_around_placeholder() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let rendered = render_format("<<<%n>>>", &event);
    assert_eq!(rendered, "<<<test.txt>>>\n");
}

#[test]
fn render_remote_host_with_context_populated() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let context = OutFormatContext {
        remote_host: Some("server.example.com".to_owned()),
        remote_address: Some("10.0.0.1".to_owned()),
        module_name: Some("backup".to_owned()),
        module_path: Some("/var/backup".to_owned()),
        is_sender: false,
    };
    assert_eq!(
        render_format_with_context("%h", &event, &context),
        "server.example.com\n"
    );
}

#[test]
fn render_remote_address_with_context_populated() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let context = OutFormatContext {
        remote_host: None,
        remote_address: Some("192.168.1.100".to_owned()),
        module_name: None,
        module_path: None,
        is_sender: false,
    };
    assert_eq!(
        render_format_with_context("%a", &event, &context),
        "192.168.1.100\n"
    );
}

#[test]
fn render_module_name_with_context_populated() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let context = OutFormatContext {
        remote_host: None,
        remote_address: None,
        module_name: Some("data".to_owned()),
        module_path: None,
        is_sender: false,
    };
    assert_eq!(render_format_with_context("%m", &event, &context), "data\n");
}

#[test]
fn render_module_path_with_context_populated() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let context = OutFormatContext {
        remote_host: None,
        remote_address: None,
        module_name: None,
        module_path: Some("/srv/data".to_owned()),
        is_sender: false,
    };
    assert_eq!(
        render_format_with_context("%P", &event, &context),
        "/srv/data\n"
    );
}

#[test]
fn render_all_remote_placeholders_with_full_context() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let context = OutFormatContext {
        remote_host: Some("host".to_owned()),
        remote_address: Some("addr".to_owned()),
        module_name: Some("mod".to_owned()),
        module_path: Some("/path".to_owned()),
        is_sender: false,
    };
    let rendered = render_format_with_context("%h %a %m %P", &event, &context);
    assert_eq!(rendered, "host addr mod /path\n");
}

#[test]
fn emit_out_format_renders_multiple_events() {
    let event1 = ClientEvent::for_test(
        PathBuf::from("alpha.txt"),
        ClientEventKind::DataCopied,
        true,
        Some(ClientEvent::test_metadata(ClientEntryKind::File)),
        LocalCopyChangeSet::new(),
    );
    let event2 = ClientEvent::for_test(
        PathBuf::from("beta.txt"),
        ClientEventKind::DataCopied,
        true,
        Some(ClientEvent::test_metadata(ClientEntryKind::File)),
        LocalCopyChangeSet::new(),
    );
    let events = [event1, event2];
    let format = parse_out_format(std::ffi::OsStr::new("%n")).unwrap();
    let mut output = Vec::new();
    emit_out_format(&events, &format, &OutFormatContext::default(), &mut output).unwrap();
    let rendered = String::from_utf8(output).unwrap();
    assert_eq!(rendered, "alpha.txt\nbeta.txt\n");
}

#[test]
fn emit_out_format_renders_empty_event_list() {
    let events: &[ClientEvent] = &[];
    let format = parse_out_format(std::ffi::OsStr::new("%n")).unwrap();
    let mut output = Vec::new();
    emit_out_format(events, &format, &OutFormatContext::default(), &mut output).unwrap();
    assert!(output.is_empty());
}

#[test]
fn render_output_always_ends_with_newline() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let rendered = render_format("no-newline-in-format", &event);
    assert!(rendered.ends_with('\n'));
}

#[test]
fn render_output_does_not_double_newline_when_format_ends_with_newline() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    // The render function checks if the buffer already ends with '\n'
    // and doesn't add another one.
    let format = parse_out_format(std::ffi::OsStr::new("%n")).unwrap();
    let mut output = Vec::new();
    format
        .render(&event, &OutFormatContext::default(), &mut output)
        .unwrap();
    let rendered = String::from_utf8(output).unwrap();
    // The output should end with exactly one newline
    assert!(rendered.ends_with('\n'));
    assert!(!rendered.ends_with("\n\n"));
}

#[test]
fn render_width_right_aligned_filename() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let rendered = render_format("%20n", &event);
    let trimmed = rendered.trim_end_matches('\n');
    // "test.txt" is 8 chars, padded to 20 right-aligned
    assert_eq!(trimmed.len(), 20);
    assert!(trimmed.ends_with("test.txt"));
    assert!(trimmed.starts_with("            ")); // 12 spaces
}

#[test]
fn render_width_left_aligned_filename() {
    let event = make_event(
        ClientEventKind::DataCopied,
        true,
        Some(ClientEntryKind::File),
        LocalCopyChangeSet::new(),
    );
    let rendered = render_format("%-20n", &event);
    let trimmed = rendered.trim_end_matches('\n');
    assert_eq!(trimmed.len(), 20);
    assert!(trimmed.starts_with("test.txt"));
    assert!(trimmed.ends_with("            ")); // 12 trailing spaces
}

#[test]
fn render_separator_humanized_large_value() {
    let format = PlaceholderFormat::new(None, PlaceholderAlignment::Right, HumanizeMode::Separator);
    assert_eq!(format_numeric_value(1_234_567, &format), "1,234,567");
}

#[test]
fn render_decimal_units_large_value() {
    let format = PlaceholderFormat::new(
        None,
        PlaceholderAlignment::Right,
        HumanizeMode::DecimalUnits,
    );
    assert_eq!(format_numeric_value(5000, &format), "5.00K");
}

#[test]
fn render_binary_units_large_value() {
    let format =
        PlaceholderFormat::new(None, PlaceholderAlignment::Right, HumanizeMode::BinaryUnits);
    assert_eq!(format_numeric_value(2048, &format), "2.00K");
}

#[test]
fn render_decimal_units_below_threshold_falls_back_to_separator() {
    let format = PlaceholderFormat::new(
        None,
        PlaceholderAlignment::Right,
        HumanizeMode::DecimalUnits,
    );
    // Values below 1000 fall back to separator format (which is just the number for small values)
    assert_eq!(format_numeric_value(999, &format), "999");
}

#[test]
fn render_binary_units_below_threshold_falls_back_to_separator() {
    let format =
        PlaceholderFormat::new(None, PlaceholderAlignment::Right, HumanizeMode::BinaryUnits);
    // Values below 1024 fall back to separator format
    assert_eq!(format_numeric_value(1023, &format), "1,023");
}
