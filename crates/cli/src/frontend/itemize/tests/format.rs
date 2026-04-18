use crate::frontend::itemize::*;

#[test]
fn format_unchanged_file() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::NotUpdated)
        .with_file_type(FileType::RegularFile);
    assert_eq!(change.format(), ".f         ");
}

#[test]
fn format_new_file() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_new_file(true);
    assert_eq!(change.format(), ">f+++++++++");
}

#[test]
fn format_updated_file_checksum_and_size() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_checksum_changed(true)
        .with_size_changed(true);
    assert_eq!(change.format(), ">fcs.......");
}

#[test]
fn format_sent_file() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Sent)
        .with_file_type(FileType::RegularFile);
    assert_eq!(change.format(), "<f.........");
}

#[test]
fn format_directory() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::NotUpdated)
        .with_file_type(FileType::Directory);
    assert_eq!(change.format(), ".d         ");
}

#[test]
fn format_symlink() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::NotUpdated)
        .with_file_type(FileType::Symlink);
    assert_eq!(change.format(), ".L         ");
}

#[test]
fn format_all_flags_changed() {
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
fn format_time_changed() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_time_changed(true);
    assert_eq!(change.format(), ">f..t......");
}

#[test]
fn format_perms_changed() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_perms_changed(true);
    assert_eq!(change.format(), ">f...p.....");
}

#[test]
fn format_owner_and_group_changed() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_owner_changed(true)
        .with_group_changed(true);
    assert_eq!(change.format(), ">f....og...");
}

#[test]
fn format_acl_and_xattr_changed() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_acl_changed(true)
        .with_xattr_changed(true);
    assert_eq!(change.format(), ">f.......ax");
}

#[test]
fn format_created_file_all_new() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Created)
        .with_file_type(FileType::RegularFile)
        .with_new_file(true);
    assert_eq!(change.format(), "cf+++++++++");
}

#[test]
fn builder_pattern_works() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::Directory)
        .with_time_changed(true);
    assert_eq!(change.format(), ">d..t......");
}

#[test]
fn format_device_file() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Created)
        .with_file_type(FileType::Device)
        .with_new_file(true);
    assert_eq!(change.format(), "cD+++++++++");
}

#[test]
fn format_special_file() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Created)
        .with_file_type(FileType::Special)
        .with_new_file(true);
    assert_eq!(change.format(), "cS+++++++++");
}

#[test]
fn format_time_set_to_transfer() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_time_set_to_transfer(true);
    assert_eq!(change.format(), ">f..T......");
}

#[test]
fn time_set_to_transfer_overrides_time_changed() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_time_changed(true)
        .with_time_set_to_transfer(true);
    assert_eq!(change.format(), ">f..T......");
}

#[test]
fn format_atime_changed_only() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_atime_changed(true);
    assert_eq!(change.format(), ">f......u..");
}

#[test]
fn format_ctime_changed_only() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_ctime_changed(true);
    assert_eq!(change.format(), ">f......n..");
}

#[test]
fn format_both_atime_and_ctime_changed() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_atime_changed(true)
        .with_ctime_changed(true);
    assert_eq!(change.format(), ">f......b..");
}

#[test]
fn format_hard_link() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::HardLink)
        .with_file_type(FileType::RegularFile)
        .with_new_file(true);
    assert_eq!(change.format(), "hf+++++++++");
}

#[test]
fn format_message_type() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Message)
        .with_file_type(FileType::RegularFile);
    assert_eq!(change.format(), "*f.........");
}

#[test]
fn format_itemize_standalone_works() {
    let change = ItemizeChange::new()
        .with_update_type(UpdateType::Received)
        .with_file_type(FileType::RegularFile)
        .with_checksum_changed(true)
        .with_size_changed(true)
        .with_time_changed(true);
    assert_eq!(format_itemize(&change), ">fcst......");
}
