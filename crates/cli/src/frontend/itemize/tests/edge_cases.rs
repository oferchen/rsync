use crate::frontend::itemize::*;

// ---- Length is always 11 ----

#[test]
fn format_length_is_always_eleven() {
    let changes = vec![
        ItemizeChange::new(),
        ItemizeChange::new().with_new_file(true),
        ItemizeChange::new()
            .with_update_type(UpdateType::Received)
            .with_file_type(FileType::Directory)
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
    ];

    for change in changes {
        let formatted = change.format();
        assert_eq!(
            formatted.len(),
            11,
            "format should be 11 chars: {formatted:?}"
        );
    }
}

// ---- New file ignores individual change flags ----

#[test]
fn new_file_overrides_individual_flags() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_new_file(true)
        .with_checksum_changed(true)
        .with_size_changed(true)
        .with_time_changed(true);
    assert_eq!(change.format(), ">f+++++++++");
}

// ---- Typical content update pattern ----

#[test]
fn format_typical_content_update() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_checksum_changed(true)
        .with_size_changed(true)
        .with_time_changed(true);
    assert_eq!(change.format(), ">fcst......");
}

// ---- Directory with time update ----

#[test]
fn format_directory_time_update() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::Directory)
        .with_time_changed(true);
    assert_eq!(change.format(), ">d..t......");
}

// ---- Missing data fills attribute positions with '?' ----

#[test]
fn format_missing_data_shows_question_marks() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_missing_data(true);
    assert_eq!(change.format(), ">f?????????");
}

#[test]
fn missing_data_overrides_individual_flags() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_missing_data(true)
        .with_checksum_changed(true)
        .with_size_changed(true)
        .with_time_changed(true);
    assert_eq!(change.format(), ">f?????????");
}

#[test]
fn missing_data_length_is_eleven() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_missing_data(true);
    assert_eq!(change.format().len(), 11);
}

#[test]
fn new_file_takes_precedence_over_missing_data() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_new_file(true)
        .with_missing_data(true);
    assert_eq!(change.format(), ">f+++++++++");
}
