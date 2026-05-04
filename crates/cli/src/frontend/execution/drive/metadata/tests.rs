use std::ffi::OsString;

use super::*;

fn default_inputs() -> MetadataInputs<'static> {
    MetadataInputs {
        archive: false,
        parsed_chown: None,
        owner: None,
        group: None,
        executability: None,
        usermap: None,
        groupmap: None,
        perms: None,
        super_mode: None,
        times: None,
        atimes: None,
        crtimes: None,
        omit_dir_times: None,
        omit_link_times: None,
        devices: None,
        specials: None,
        hard_links: None,
        links: None,
        sparse: None,
        copy_links: None,
        copy_unsafe_links: None,
        keep_dirlinks: None,
        relative: None,
        one_file_system: None,
        chmod: &[],
    }
}

#[test]
fn archive_false_defaults_to_no_preservation() {
    let result = compute_metadata_settings(default_inputs()).unwrap();

    assert!(!result.preserve_owner);
    assert!(!result.preserve_group);
    assert!(!result.preserve_permissions);
    assert!(!result.preserve_times);
    assert!(!result.preserve_devices);
    assert!(!result.preserve_specials);
    assert!(!result.preserve_symlinks);
}

#[test]
fn archive_true_enables_preservation() {
    let mut inputs = default_inputs();
    inputs.archive = true;
    let result = compute_metadata_settings(inputs).unwrap();

    assert!(result.preserve_owner);
    assert!(result.preserve_group);
    assert!(result.preserve_permissions);
    assert!(result.preserve_times);
    assert!(result.preserve_devices);
    assert!(result.preserve_specials);
    assert!(result.preserve_symlinks);
}

#[test]
fn explicit_owner_overrides_archive() {
    let mut inputs = default_inputs();
    inputs.archive = true;
    inputs.owner = Some(false);
    assert!(!compute_metadata_settings(inputs).unwrap().preserve_owner);
}

#[test]
fn explicit_group_overrides_archive() {
    let mut inputs = default_inputs();
    inputs.archive = true;
    inputs.group = Some(false);
    assert!(!compute_metadata_settings(inputs).unwrap().preserve_group);
}

#[test]
fn explicit_perms_overrides_archive() {
    let mut inputs = default_inputs();
    inputs.archive = true;
    inputs.perms = Some(false);
    assert!(
        !compute_metadata_settings(inputs)
            .unwrap()
            .preserve_permissions
    );
}

#[test]
fn explicit_times_overrides_archive() {
    let mut inputs = default_inputs();
    inputs.archive = true;
    inputs.times = Some(false);
    assert!(!compute_metadata_settings(inputs).unwrap().preserve_times);
}

#[test]
fn explicit_devices_overrides_archive() {
    let mut inputs = default_inputs();
    inputs.archive = true;
    inputs.devices = Some(false);
    assert!(!compute_metadata_settings(inputs).unwrap().preserve_devices);
}

#[test]
fn explicit_specials_overrides_archive() {
    let mut inputs = default_inputs();
    inputs.archive = true;
    inputs.specials = Some(false);
    assert!(!compute_metadata_settings(inputs).unwrap().preserve_specials);
}

#[test]
fn explicit_links_overrides_archive() {
    let mut inputs = default_inputs();
    inputs.archive = true;
    inputs.links = Some(false);
    assert!(!compute_metadata_settings(inputs).unwrap().preserve_symlinks);
}

#[test]
fn super_mode_enables_owner() {
    let mut inputs = default_inputs();
    inputs.super_mode = Some(true);
    assert!(compute_metadata_settings(inputs).unwrap().preserve_owner);
}

#[test]
fn super_mode_enables_group() {
    let mut inputs = default_inputs();
    inputs.super_mode = Some(true);
    assert!(compute_metadata_settings(inputs).unwrap().preserve_group);
}

#[test]
fn super_mode_enables_perms() {
    let mut inputs = default_inputs();
    inputs.super_mode = Some(true);
    assert!(
        compute_metadata_settings(inputs)
            .unwrap()
            .preserve_permissions
    );
}

#[test]
fn executability_follows_perms_when_enabled() {
    let mut inputs = default_inputs();
    inputs.perms = Some(true);
    assert!(
        compute_metadata_settings(inputs)
            .unwrap()
            .preserve_executability
    );
}

#[test]
fn executability_explicit_without_perms() {
    let mut inputs = default_inputs();
    inputs.executability = Some(true);
    let result = compute_metadata_settings(inputs).unwrap();
    assert!(result.preserve_executability);
    assert!(!result.preserve_permissions);
}

#[test]
fn executability_defaults_to_false() {
    assert!(
        !compute_metadata_settings(default_inputs())
            .unwrap()
            .preserve_executability
    );
}

#[test]
fn atimes_defaults_to_false() {
    assert!(
        !compute_metadata_settings(default_inputs())
            .unwrap()
            .preserve_atimes
    );
}

#[test]
fn atimes_explicit_true() {
    let mut inputs = default_inputs();
    inputs.atimes = Some(true);
    assert!(compute_metadata_settings(inputs).unwrap().preserve_atimes);
}

#[test]
fn crtimes_defaults_to_false() {
    assert!(
        !compute_metadata_settings(default_inputs())
            .unwrap()
            .preserve_crtimes
    );
}

#[test]
fn crtimes_explicit_true() {
    let mut inputs = default_inputs();
    inputs.crtimes = Some(true);
    assert!(compute_metadata_settings(inputs).unwrap().preserve_crtimes);
}

#[test]
fn omit_dir_times_defaults_to_false() {
    assert!(
        !compute_metadata_settings(default_inputs())
            .unwrap()
            .omit_dir_times
    );
}

#[test]
fn omit_dir_times_explicit_true() {
    let mut inputs = default_inputs();
    inputs.omit_dir_times = Some(true);
    assert!(compute_metadata_settings(inputs).unwrap().omit_dir_times);
}

#[test]
fn omit_link_times_defaults_to_false() {
    assert!(
        !compute_metadata_settings(default_inputs())
            .unwrap()
            .omit_link_times
    );
}

#[test]
fn omit_link_times_explicit_true() {
    let mut inputs = default_inputs();
    inputs.omit_link_times = Some(true);
    assert!(compute_metadata_settings(inputs).unwrap().omit_link_times);
}

#[test]
fn hard_links_defaults_to_false() {
    assert!(
        !compute_metadata_settings(default_inputs())
            .unwrap()
            .preserve_hard_links
    );
}

#[test]
fn hard_links_explicit_true() {
    let mut inputs = default_inputs();
    inputs.hard_links = Some(true);
    assert!(
        compute_metadata_settings(inputs)
            .unwrap()
            .preserve_hard_links
    );
}

#[test]
fn copy_links_defaults_to_false() {
    assert!(
        !compute_metadata_settings(default_inputs())
            .unwrap()
            .copy_links
    );
}

#[test]
fn copy_links_explicit_true() {
    let mut inputs = default_inputs();
    inputs.copy_links = Some(true);
    assert!(compute_metadata_settings(inputs).unwrap().copy_links);
}

#[test]
fn copy_unsafe_links_defaults_to_false() {
    assert!(
        !compute_metadata_settings(default_inputs())
            .unwrap()
            .copy_unsafe_links
    );
}

#[test]
fn copy_unsafe_links_explicit_true() {
    let mut inputs = default_inputs();
    inputs.copy_unsafe_links = Some(true);
    assert!(compute_metadata_settings(inputs).unwrap().copy_unsafe_links);
}

#[test]
fn keep_dirlinks_defaults_to_false() {
    assert!(
        !compute_metadata_settings(default_inputs())
            .unwrap()
            .keep_dirlinks
    );
}

#[test]
fn keep_dirlinks_explicit_true() {
    let mut inputs = default_inputs();
    inputs.keep_dirlinks = Some(true);
    assert!(compute_metadata_settings(inputs).unwrap().keep_dirlinks);
}

#[test]
fn sparse_defaults_to_false() {
    assert!(!compute_metadata_settings(default_inputs()).unwrap().sparse);
}

#[test]
fn sparse_explicit_true() {
    let mut inputs = default_inputs();
    inputs.sparse = Some(true);
    assert!(compute_metadata_settings(inputs).unwrap().sparse);
}

#[test]
fn relative_defaults_to_false() {
    assert!(
        !compute_metadata_settings(default_inputs())
            .unwrap()
            .relative
    );
}

#[test]
fn relative_explicit_true() {
    let mut inputs = default_inputs();
    inputs.relative = Some(true);
    assert!(compute_metadata_settings(inputs).unwrap().relative);
}

#[test]
fn one_file_system_defaults_to_zero() {
    assert_eq!(
        compute_metadata_settings(default_inputs())
            .unwrap()
            .one_file_system,
        0
    );
}

#[test]
fn one_file_system_explicit_single() {
    let mut inputs = default_inputs();
    inputs.one_file_system = Some(1);
    assert_eq!(
        compute_metadata_settings(inputs).unwrap().one_file_system,
        1
    );
}

#[test]
fn one_file_system_explicit_double() {
    let mut inputs = default_inputs();
    inputs.one_file_system = Some(2);
    assert_eq!(
        compute_metadata_settings(inputs).unwrap().one_file_system,
        2
    );
}

#[test]
fn no_chmod_returns_none() {
    assert!(
        compute_metadata_settings(default_inputs())
            .unwrap()
            .chmod_modifiers
            .is_none()
    );
}

#[test]
fn valid_chmod_parses() {
    let chmod_specs = [OsString::from("u+rwx")];
    let mut inputs = default_inputs();
    inputs.chmod = &chmod_specs;
    assert!(
        compute_metadata_settings(inputs)
            .unwrap()
            .chmod_modifiers
            .is_some()
    );
}

#[test]
fn invalid_chmod_returns_error() {
    let chmod_specs = [OsString::from("invalid_chmod_spec")];
    let mut inputs = default_inputs();
    inputs.chmod = &chmod_specs;
    assert!(compute_metadata_settings(inputs).is_err());
}

#[test]
fn metadata_settings_all_fields_accessible() {
    let settings = compute_metadata_settings(default_inputs()).unwrap();

    let _ = settings.preserve_owner;
    let _ = settings.preserve_group;
    let _ = settings.preserve_executability;
    let _ = settings.preserve_permissions;
    let _ = settings.preserve_times;
    let _ = settings.preserve_atimes;
    let _ = settings.preserve_crtimes;
    let _ = settings.omit_dir_times;
    let _ = settings.omit_link_times;
    let _ = settings.preserve_devices;
    let _ = settings.preserve_specials;
    let _ = settings.preserve_hard_links;
    let _ = settings.preserve_symlinks;
    let _ = settings.sparse;
    let _ = settings.copy_links;
    let _ = settings.copy_unsafe_links;
    let _ = settings.keep_dirlinks;
    let _ = settings.relative;
    let _ = settings.one_file_system;
    let _ = settings.chmod_modifiers;
    let _ = settings.user_mapping;
    let _ = settings.group_mapping;
}

#[test]
fn metadata_inputs_default_helper() {
    let inputs = default_inputs();
    assert!(!inputs.archive);
    assert!(inputs.parsed_chown.is_none());
    assert!(inputs.owner.is_none());
    assert!(inputs.chmod.is_empty());
}
