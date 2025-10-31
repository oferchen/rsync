#![deny(unsafe_code)]

use std::ffi::OsString;

use rsync_meta::ChmodModifiers;

use crate::frontend::execution::chown::ParsedChown;

/// Derived metadata preservation settings used by both config and fallback logic.
pub(crate) struct MetadataSettings {
    pub(crate) preserve_owner: bool,
    pub(crate) preserve_group: bool,
    pub(crate) preserve_permissions: bool,
    pub(crate) preserve_times: bool,
    pub(crate) omit_dir_times: bool,
    pub(crate) omit_link_times: bool,
    pub(crate) preserve_devices: bool,
    pub(crate) preserve_specials: bool,
    pub(crate) preserve_hard_links: bool,
    pub(crate) sparse: bool,
    pub(crate) copy_links: bool,
    pub(crate) copy_unsafe_links: bool,
    pub(crate) keep_dirlinks: bool,
    pub(crate) relative: bool,
    pub(crate) one_file_system: bool,
    pub(crate) chmod_modifiers: Option<ChmodModifiers>,
}

/// Computes metadata preservation flags according to upstream semantics.
pub(crate) fn compute_metadata_settings(
    archive: bool,
    parsed_chown: Option<&ParsedChown>,
    owner: Option<bool>,
    group: Option<bool>,
    perms: Option<bool>,
    super_mode: Option<bool>,
    times: Option<bool>,
    omit_dir_times: Option<bool>,
    omit_link_times: Option<bool>,
    devices: Option<bool>,
    specials: Option<bool>,
    hard_links: Option<bool>,
    sparse: Option<bool>,
    copy_links: Option<bool>,
    copy_unsafe_links: Option<bool>,
    keep_dirlinks: Option<bool>,
    relative: Option<bool>,
    one_file_system: Option<bool>,
    chmod: &[OsString],
) -> Result<MetadataSettings, rsync_core::message::Message> {
    let preserve_owner = if parsed_chown.and_then(|value| value.owner()).is_some() {
        true
    } else if let Some(value) = owner {
        value
    } else if super_mode == Some(true) {
        true
    } else {
        archive
    };

    let preserve_group = if parsed_chown.and_then(|value| value.group()).is_some() {
        true
    } else if let Some(value) = group {
        value
    } else if super_mode == Some(true) {
        true
    } else {
        archive
    };

    let preserve_permissions = if let Some(value) = perms {
        value
    } else if super_mode == Some(true) {
        true
    } else {
        archive
    };

    let preserve_times = times.unwrap_or(archive);
    let omit_dir_times_setting = omit_dir_times.unwrap_or(false);
    let omit_link_times_setting = omit_link_times.unwrap_or(false);
    let preserve_devices = devices.unwrap_or(archive);
    let preserve_specials = specials.unwrap_or(archive);
    let preserve_hard_links = hard_links.unwrap_or(false);
    let sparse_setting = sparse.unwrap_or(false);
    let copy_links_setting = copy_links.unwrap_or(false);
    let copy_unsafe_links_setting = copy_unsafe_links.unwrap_or(false);
    let keep_dirlinks_flag = keep_dirlinks.unwrap_or(false);
    let relative_paths = relative.unwrap_or(false);
    let one_file_system_setting = one_file_system.unwrap_or(false);

    let mut modifiers: Option<ChmodModifiers> = None;
    for spec in chmod {
        let spec_text = spec.to_string_lossy();
        let trimmed = spec_text.trim();
        match ChmodModifiers::parse(trimmed) {
            Ok(parsed) => {
                if let Some(existing) = &mut modifiers {
                    existing.extend(parsed);
                } else {
                    modifiers = Some(parsed);
                }
            }
            Err(error) => {
                let formatted = format!(
                    "failed to parse --chmod specification '{}': {}",
                    spec_text, error
                );
                return Err(rsync_core::rsync_error!(1, formatted)
                    .with_role(rsync_core::message::Role::Client));
            }
        }
    }

    Ok(MetadataSettings {
        preserve_owner,
        preserve_group,
        preserve_permissions,
        preserve_times,
        omit_dir_times: omit_dir_times_setting,
        omit_link_times: omit_link_times_setting,
        preserve_devices,
        preserve_specials,
        preserve_hard_links,
        sparse: sparse_setting,
        copy_links: copy_links_setting,
        copy_unsafe_links: copy_unsafe_links_setting,
        keep_dirlinks: keep_dirlinks_flag,
        relative: relative_paths,
        one_file_system: one_file_system_setting,
        chmod_modifiers: modifiers,
    })
}
