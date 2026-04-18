use crate::frontend::itemize::*;

#[test]
fn update_type_sent_is_less_than() {
    assert_eq!(UpdateType::Sent.as_char(), '<');
}

#[test]
fn update_type_received_is_greater_than() {
    assert_eq!(UpdateType::Received.as_char(), '>');
}

#[test]
fn update_type_created_is_c() {
    assert_eq!(UpdateType::Created.as_char(), 'c');
}

#[test]
fn update_type_hard_link_is_h() {
    assert_eq!(UpdateType::HardLink.as_char(), 'h');
}

#[test]
fn update_type_not_updated_is_dot() {
    assert_eq!(UpdateType::NotUpdated.as_char(), '.');
}

#[test]
fn update_type_message_is_star() {
    assert_eq!(UpdateType::Message.as_char(), '*');
}

#[test]
fn file_type_regular_file_is_f() {
    assert_eq!(FileType::RegularFile.as_char(), 'f');
}

#[test]
fn file_type_directory_is_d() {
    assert_eq!(FileType::Directory.as_char(), 'd');
}

#[test]
fn file_type_symlink_is_l_upper() {
    assert_eq!(FileType::Symlink.as_char(), 'L');
}

#[test]
fn file_type_device_is_d_upper() {
    assert_eq!(FileType::Device.as_char(), 'D');
}

#[test]
fn file_type_special_is_s_upper() {
    assert_eq!(FileType::Special.as_char(), 'S');
}

#[test]
fn new_creates_unchanged_regular_file() {
    let change = ItemizeChange::new();
    assert_eq!(change.update_type, UpdateType::NotUpdated);
    assert_eq!(change.file_type, FileType::RegularFile);
    assert!(!change.new_file);
    assert!(change.is_unchanged());
}

#[test]
fn default_is_same_as_new() {
    assert_eq!(ItemizeChange::default(), ItemizeChange::new());
}

#[test]
fn is_unchanged_returns_true_for_no_changes() {
    let change = ItemizeChange::new();
    assert!(change.is_unchanged());
}

#[test]
fn is_unchanged_returns_false_for_new_file() {
    let change = ItemizeChange::new().with_new_file(true);
    assert!(!change.is_unchanged());
}

#[test]
fn is_unchanged_returns_false_when_checksum_changed() {
    let change = ItemizeChange::new().with_checksum_changed(true);
    assert!(!change.is_unchanged());
}

#[test]
fn is_unchanged_returns_false_when_any_flag_set() {
    let change = ItemizeChange::new().with_acl_changed(true);
    assert!(!change.is_unchanged());
}

#[test]
fn is_unchanged_returns_false_when_missing_data() {
    let change = ItemizeChange::new().with_missing_data(true);
    assert!(!change.is_unchanged());
}
