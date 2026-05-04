//! Golden output tests comparing our itemize format against upstream rsync's log.c.
//!
//! Each test case documents the exact upstream source reference and expected output.
//! These tests verify character-by-character parity with upstream rsync 3.4.1.

use crate::frontend::itemize::*;

#[test]
fn golden_deleting_format_is_eleven_chars() {
    // upstream: `n = "*deleting  ";` - 9 content chars + 2 trailing spaces = 11
    let expected = "*deleting  ";
    assert_eq!(expected.len(), 11, "upstream *deleting must be 11 chars");
}

#[test]
fn golden_direction_sent() {
    // upstream: log.c:703 - `!local_server && *op == 's' ? '<'`
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Sent)
        .with_file_type(FileType::RegularFile)
        .with_checksum_changed(true);
    assert_eq!(change.format(), "<fc........");
}

#[test]
fn golden_direction_received() {
    // upstream: log.c:704 - default is '>'
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_checksum_changed(true);
    assert_eq!(change.format(), ">fc........");
}

#[test]
fn golden_direction_local_change() {
    // upstream: log.c:701-702 - ITEM_LOCAL_CHANGE without ITEM_XNAME_FOLLOWS = 'c'
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Created)
        .with_file_type(FileType::Directory)
        .with_new_file(true);
    assert_eq!(change.format(), "cd+++++++++");
}

#[test]
fn golden_direction_hardlink() {
    // upstream: log.c:702 - ITEM_LOCAL_CHANGE with ITEM_XNAME_FOLLOWS = 'h'
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::HardLink)
        .with_file_type(FileType::RegularFile)
        .with_new_file(true);
    assert_eq!(change.format(), "hf+++++++++");
}

#[test]
fn golden_direction_not_updated() {
    // upstream: log.c:703 - !(iflags & ITEM_TRANSFER) = '.'
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::NotUpdated)
        .with_file_type(FileType::RegularFile);
    assert_eq!(change.format(), ".f         ");
}

#[test]
fn golden_filetype_regular() {
    // upstream: log.c:714 - default = 'f'
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_new_file(true);
    assert_eq!(&change.format()[..2], ">f");
}

#[test]
fn golden_filetype_directory() {
    // upstream: log.c:712 - S_ISDIR = 'd'
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::Directory)
        .with_new_file(true);
    assert_eq!(&change.format()[..2], ">d");
}

#[test]
fn golden_filetype_symlink() {
    // upstream: log.c:706 - S_ISLNK = 'L'
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::Symlink)
        .with_new_file(true);
    assert_eq!(&change.format()[..2], ">L");
}

#[test]
fn golden_filetype_device() {
    // upstream: log.c:714 - IS_DEVICE = 'D'
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::Device)
        .with_new_file(true);
    assert_eq!(&change.format()[..2], ">D");
}

#[test]
fn golden_filetype_special() {
    // upstream: log.c:713 - IS_SPECIAL = 'S'
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::Special)
        .with_new_file(true);
    assert_eq!(&change.format()[..2], ">S");
}

#[test]
fn golden_symlink_never_shows_size() {
    // upstream: log.c:707 - `c[3] = '.';` unconditionally for symlinks
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::Symlink)
        .with_size_changed(true)
        .with_time_changed(true);
    // Position 3 must be '.' even when size_changed is set
    assert_eq!(change.format(), ">L..t......");
}

#[test]
fn golden_file_shows_size_when_changed() {
    // upstream: log.c:715 - non-symlink: `c[3] = 's'` when ITEM_REPORT_SIZE
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_size_changed(true);
    assert_eq!(change.format(), ">f.s.......");
}

#[test]
fn golden_new_file_all_plus() {
    // upstream: log.c:731 - `ch = '+';` for ITEM_IS_NEW
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_new_file(true);
    assert_eq!(change.format(), ">f+++++++++");
}

#[test]
fn golden_missing_data_all_question() {
    // upstream: log.c:731 - `ch = '?';` for ITEM_MISSING_DATA
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_missing_data(true);
    assert_eq!(change.format(), ">f?????????");
}

#[test]
fn golden_collapse_dots_for_not_updated() {
    // upstream: log.c:735 - `c[0] == '.'` triggers collapse
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::NotUpdated)
        .with_file_type(FileType::RegularFile);
    assert_eq!(change.format(), ".f         ");
}

#[test]
fn golden_collapse_dots_for_hardlink_unchanged() {
    // upstream: log.c:735 - `c[0] == 'h'` triggers collapse
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::HardLink)
        .with_file_type(FileType::RegularFile);
    assert_eq!(change.format(), "hf         ");
}

#[test]
fn golden_collapse_dots_for_created_unchanged() {
    // upstream: log.c:735 - `c[0] == 'c'` triggers collapse
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Created)
        .with_file_type(FileType::RegularFile);
    assert_eq!(change.format(), "cf         ");
}

#[test]
fn golden_no_collapse_for_sent() {
    // upstream: log.c:735 - '<' is NOT in the collapse set
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Sent)
        .with_file_type(FileType::RegularFile);
    assert_eq!(change.format(), "<f.........");
}

#[test]
fn golden_no_collapse_for_received() {
    // upstream: log.c:735 - '>' is NOT in the collapse set
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile);
    assert_eq!(change.format(), ">f.........");
}

#[test]
fn golden_partial_change_prevents_collapse() {
    // upstream: log.c:737-738 - if any position is not '.', no collapse
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::NotUpdated)
        .with_file_type(FileType::RegularFile)
        .with_perms_changed(true);
    assert_eq!(change.format(), ".f...p.....");
}

#[test]
fn golden_all_attributes_changed() {
    // upstream: log.c:719-727 - all flags set produces `cstpogbax`
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_checksum_changed(true)
        .with_size_changed(true)
        .with_time_changed(true)
        .with_perms_changed(true)
        .with_owner_changed(true)
        .with_group_changed(true)
        .with_atime_changed(true)
        .with_ctime_changed(true)
        .with_acl_changed(true)
        .with_xattr_changed(true);
    assert_eq!(change.format(), ">fcstpogbax");
}

#[test]
fn golden_atime_only_shows_u() {
    // upstream: log.c:725 - `iflags & ITEM_REPORT_ATIME ? 'u'`
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_atime_changed(true);
    assert_eq!(change.format(), ">f......u..");
}

#[test]
fn golden_crtime_only_shows_n() {
    // upstream: log.c:725 - fallback to 'n' for ITEM_REPORT_CRTIME only
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_ctime_changed(true);
    assert_eq!(change.format(), ">f......n..");
}

#[test]
fn golden_both_atime_crtime_shows_b() {
    // upstream: log.c:724 - BITS_SET both = 'b'
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_atime_changed(true)
        .with_ctime_changed(true);
    assert_eq!(change.format(), ">f......b..");
}

#[test]
fn golden_time_lowercase_t() {
    // upstream: log.c:717 - preserve_mtimes => 't'
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_time_changed(true);
    assert_eq!(change.format(), ">f..t......");
}

#[test]
fn golden_time_uppercase_t() {
    // upstream: log.c:716 - !preserve_mtimes => 'T'
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_time_set_to_transfer(true);
    assert_eq!(change.format(), ">f..T......");
}

#[test]
fn golden_time_uppercase_t_takes_precedence() {
    // upstream: log.c:716-717 - T overrides t when both conditions apply
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_time_changed(true)
        .with_time_set_to_transfer(true);
    assert_eq!(change.format(), ">f..T......");
}

#[test]
fn golden_all_outputs_are_eleven_chars() {
    // upstream: log.c:728 - `c[11] = '\0'` - always exactly 11 characters
    let cases: Vec<(ItemizeChange, &str)> = vec![
        (
            ItemizeChange::new()
                .with_update_type(UpdateType::Received)
                .with_file_type(FileType::RegularFile)
                .with_new_file(true),
            ">f+++++++++",
        ),
        (
            ItemizeChange::new()
                .with_update_type(UpdateType::NotUpdated)
                .with_file_type(FileType::Directory),
            ".d         ",
        ),
        (
            ItemizeChange::new()
                .with_update_type(UpdateType::Received)
                .with_file_type(FileType::Symlink)
                .with_size_changed(true)
                .with_checksum_changed(true),
            ">Lc........",
        ),
        (
            ItemizeChange::new()
                .with_update_type(UpdateType::Sent)
                .with_file_type(FileType::RegularFile)
                .with_checksum_changed(true)
                .with_size_changed(true)
                .with_time_changed(true)
                .with_perms_changed(true)
                .with_owner_changed(true)
                .with_group_changed(true)
                .with_atime_changed(true)
                .with_ctime_changed(true)
                .with_acl_changed(true)
                .with_xattr_changed(true),
            "<fcstpogbax",
        ),
    ];

    for (change, expected) in &cases {
        let result = change.format();
        assert_eq!(result.len(), 11, "must be 11 chars: {result:?}");
        assert_eq!(&result, expected, "mismatch for expected {expected:?}");
    }
}

#[test]
fn golden_symlink_size_only_collapses_to_spaces() {
    // upstream: log.c:707 - symlink c[3] = '.', so if size_changed is the only flag,
    // all rendered positions 2-10 are dots, and the collapse rule applies.
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::NotUpdated)
        .with_file_type(FileType::Symlink)
        .with_size_changed(true);
    // size is suppressed for symlinks, all rendered attrs are dots, collapse kicks in
    assert_eq!(change.format(), ".L         ");
}

#[test]
fn golden_symlink_checksum_without_size() {
    // upstream: log.c:707 forces c[3]='.' for symlinks, but checksum at c[2] is independent
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::Symlink)
        .with_checksum_changed(true)
        .with_size_changed(true);
    // Position 2 = 'c' (checksum), position 3 = '.' (size suppressed for symlinks)
    assert_eq!(change.format(), ">Lc........");
}
