use super::super::types::LocalCopyOptions;
use super::accessors::{effective_am_root, is_effective_root};
use ::metadata::CopyAsIds;

#[test]
fn owner_preservation() {
    let options = LocalCopyOptions::new().owner(true);
    assert!(options.preserve_owner());
}

#[test]
fn owner_override() {
    let options = LocalCopyOptions::new().with_owner_override(Some(1000));
    assert_eq!(options.owner_override(), Some(1000));
}

#[test]
fn group_preservation() {
    let options = LocalCopyOptions::new().group(true);
    assert!(options.preserve_group());
}

#[test]
fn group_override() {
    let options = LocalCopyOptions::new().with_group_override(Some(1000));
    assert_eq!(options.group_override(), Some(1000));
}

#[test]
fn executability_preservation() {
    let options = LocalCopyOptions::new().executability(true);
    assert!(options.preserve_executability());
}

#[test]
fn permissions_preservation() {
    let options = LocalCopyOptions::new().permissions(true);
    assert!(options.preserve_permissions());
}

#[test]
fn times_preservation() {
    let options = LocalCopyOptions::new().times(true);
    assert!(options.preserve_times());
}

#[test]
fn atimes_preservation() {
    let options = LocalCopyOptions::new().atimes(true);
    assert!(options.preserve_atimes());
}

#[test]
fn atimes_disabled_by_default() {
    let options = LocalCopyOptions::new();
    assert!(!options.preserve_atimes());
}

#[test]
fn crtimes_preservation() {
    let options = LocalCopyOptions::new().crtimes(true);
    assert!(options.preserve_crtimes());
}

#[test]
fn crtimes_defaults_to_false() {
    let options = LocalCopyOptions::new();
    assert!(!options.preserve_crtimes());
}

#[test]
fn omit_dir_times() {
    let options = LocalCopyOptions::new().omit_dir_times(true);
    assert!(options.omit_dir_times_enabled());
}

#[test]
fn omit_link_times() {
    let options = LocalCopyOptions::new().omit_link_times(true);
    assert!(options.omit_link_times_enabled());
}

#[test]
fn numeric_ids() {
    let options = LocalCopyOptions::new().numeric_ids(true);
    assert!(options.numeric_ids_enabled());
}

#[test]
fn chmod_none_by_default() {
    let options = LocalCopyOptions::new();
    assert!(options.chmod().is_none());
}

#[test]
fn user_mapping_none_by_default() {
    let options = LocalCopyOptions::new();
    assert!(options.user_mapping().is_none());
}

#[test]
fn group_mapping_none_by_default() {
    let options = LocalCopyOptions::new();
    assert!(options.group_mapping().is_none());
}

#[test]
fn super_mode_none_by_default() {
    let options = LocalCopyOptions::new();
    assert_eq!(options.super_mode_setting(), None);
}

#[test]
fn super_mode_set_true() {
    let options = LocalCopyOptions::new().super_mode(Some(true));
    assert_eq!(options.super_mode_setting(), Some(true));
    assert!(options.am_root());
}

#[test]
fn super_mode_set_false() {
    let options = LocalCopyOptions::new().super_mode(Some(false));
    assert_eq!(options.super_mode_setting(), Some(false));
    assert!(!options.am_root());
}

#[test]
fn super_mode_none_defers_to_euid() {
    let options = LocalCopyOptions::new();
    // am_root() should reflect whether we are actually root
    let expected = is_effective_root();
    assert_eq!(options.am_root(), expected);
}

#[test]
fn fake_super_disabled_by_default() {
    let options = LocalCopyOptions::new();
    assert!(!options.fake_super_enabled());
}

#[test]
fn fake_super_round_trip() {
    let options = LocalCopyOptions::new().fake_super(true);
    assert!(options.fake_super_enabled());

    let disabled = options.fake_super(false);
    assert!(!disabled.fake_super_enabled());
}

// upstream: options.c:89 - am_root tri-state, -1 is "fake-super"
// upstream: syscall.c do_mknod() - am_root<0 sentinel substitutes placeholder
#[test]
fn effective_am_root_super_true_fake_super_true_is_false() {
    // --super --fake-super: upstream demotes am_root to -1 regardless of --super.
    assert!(!effective_am_root(Some(true), true));
}

#[test]
fn effective_am_root_super_true_fake_super_false_is_true() {
    // --super alone keeps the privileged path active.
    assert!(effective_am_root(Some(true), false));
}

#[test]
fn effective_am_root_super_false_fake_super_true_is_false() {
    // --no-super --fake-super: privileged ops disabled, xattr placeholder used.
    assert!(!effective_am_root(Some(false), true));
}

#[test]
fn effective_am_root_super_false_fake_super_false_is_false() {
    // --no-super alone disables privileged path even when running as root.
    assert!(!effective_am_root(Some(false), false));
}

#[test]
fn effective_am_root_super_none_fake_super_true_is_false() {
    // --fake-super alone: forces am_root false even when EUID==0.
    assert!(!effective_am_root(None, true));
}

#[test]
fn effective_am_root_super_none_fake_super_false_defers_to_euid() {
    // No flags: defers to the effective UID check.
    assert_eq!(effective_am_root(None, false), is_effective_root());
}

#[test]
fn fake_super_demotes_am_root_when_super_explicit() {
    // Integration check via the public LocalCopyOptions::am_root() accessor.
    let options = LocalCopyOptions::new()
        .super_mode(Some(true))
        .fake_super(true);
    assert!(!options.am_root());
}

#[test]
fn copy_as_none_by_default() {
    let options = LocalCopyOptions::new();
    assert!(options.copy_as_ids().is_none());
}

#[test]
fn copy_as_round_trip() {
    let ids = CopyAsIds {
        uid: 1000,
        gid: Some(1001),
    };
    let options = LocalCopyOptions::new().with_copy_as(Some(ids));
    let stored = options.copy_as_ids().expect("copy_as_ids should be Some");
    assert_eq!(stored.uid, 1000);
    assert_eq!(stored.gid, Some(1001));
}

#[test]
fn copy_as_without_group() {
    let ids = CopyAsIds {
        uid: 65534,
        gid: None,
    };
    let options = LocalCopyOptions::new().with_copy_as(Some(ids));
    let stored = options.copy_as_ids().unwrap();
    assert_eq!(stored.uid, 65534);
    assert_eq!(stored.gid, None);
}

#[test]
fn copy_as_clear() {
    let ids = CopyAsIds {
        uid: 1000,
        gid: Some(1001),
    };
    let options = LocalCopyOptions::new()
        .with_copy_as(Some(ids))
        .with_copy_as(None);
    assert!(options.copy_as_ids().is_none());
}
