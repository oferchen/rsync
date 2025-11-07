#![deny(unsafe_code)]

use std::ffi::OsString;

use oc_rsync_meta::ChmodModifiers;
use oc_rsync_meta::{GroupMapping, MappingKind, NameMapping, UserMapping};

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
    pub(crate) user_mapping: Option<UserMapping>,
    pub(crate) group_mapping: Option<GroupMapping>,
}

pub(crate) struct MetadataInputs<'a> {
    pub(crate) archive: bool,
    pub(crate) parsed_chown: Option<&'a ParsedChown>,
    pub(crate) owner: Option<bool>,
    pub(crate) group: Option<bool>,
    pub(crate) usermap: Option<&'a OsString>,
    pub(crate) groupmap: Option<&'a OsString>,
    pub(crate) perms: Option<bool>,
    pub(crate) super_mode: Option<bool>,
    pub(crate) times: Option<bool>,
    pub(crate) omit_dir_times: Option<bool>,
    pub(crate) omit_link_times: Option<bool>,
    pub(crate) devices: Option<bool>,
    pub(crate) specials: Option<bool>,
    pub(crate) hard_links: Option<bool>,
    pub(crate) sparse: Option<bool>,
    pub(crate) copy_links: Option<bool>,
    pub(crate) copy_unsafe_links: Option<bool>,
    pub(crate) keep_dirlinks: Option<bool>,
    pub(crate) relative: Option<bool>,
    pub(crate) one_file_system: Option<bool>,
    pub(crate) chmod: &'a [OsString],
}

/// Computes metadata preservation flags according to upstream semantics.
pub(crate) fn compute_metadata_settings(
    inputs: MetadataInputs<'_>,
) -> Result<MetadataSettings, oc_rsync_core::message::Message> {
    let MetadataInputs {
        archive,
        parsed_chown,
        owner,
        group,
        perms,
        usermap,
        groupmap,
        super_mode,
        times,
        omit_dir_times,
        omit_link_times,
        devices,
        specials,
        hard_links,
        sparse,
        copy_links,
        copy_unsafe_links,
        keep_dirlinks,
        relative,
        one_file_system,
        chmod,
    } = inputs;
    let user_mapping = match usermap {
        Some(value) => Some(parse_user_mapping(value, parsed_chown)?),
        None => None,
    };
    let group_mapping = match groupmap {
        Some(value) => Some(parse_group_mapping(value, parsed_chown)?),
        None => None,
    };
    let preserve_owner =
        if parsed_chown.and_then(|value| value.owner()).is_some() || user_mapping.is_some() {
            true
        } else if let Some(value) = owner {
            value
        } else if super_mode == Some(true) {
            true
        } else {
            archive
        };

    let preserve_group =
        if parsed_chown.and_then(|value| value.group()).is_some() || group_mapping.is_some() {
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
                let formatted =
                    format!("failed to parse --chmod specification '{spec_text}': {error}");
                return Err(oc_rsync_core::rsync_error!(1, formatted)
                    .with_role(oc_rsync_core::message::Role::Client));
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
        user_mapping,
        group_mapping,
    })
}

fn parse_user_mapping(
    value: &OsString,
    parsed_chown: Option<&ParsedChown>,
) -> Result<UserMapping, oc_rsync_core::message::Message> {
    if parsed_chown.and_then(|parsed| parsed.owner()).is_some() {
        return Err(oc_rsync_core::rsync_error!(
            1,
            "--usermap conflicts with prior --chown user specification"
        )
        .with_role(oc_rsync_core::message::Role::Client));
    }

    parse_mapping(value, MappingKind::User)
}

fn parse_group_mapping(
    value: &OsString,
    parsed_chown: Option<&ParsedChown>,
) -> Result<GroupMapping, oc_rsync_core::message::Message> {
    if parsed_chown.and_then(|parsed| parsed.group()).is_some() {
        return Err(oc_rsync_core::rsync_error!(
            1,
            "--groupmap conflicts with prior --chown group specification"
        )
        .with_role(oc_rsync_core::message::Role::Client));
    }

    parse_mapping(value, MappingKind::Group)
}

fn parse_mapping<M>(
    value: &OsString,
    kind: MappingKind,
) -> Result<M, oc_rsync_core::message::Message>
where
    M: From<NameMapping>,
{
    let spec = value.to_string_lossy();
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err(oc_rsync_core::rsync_error!(
            1,
            format!("{} requires a non-empty mapping specification", kind.flag())
        )
        .with_role(oc_rsync_core::message::Role::Client));
    }

    match NameMapping::parse(kind, trimmed) {
        Ok(mapping) => Ok(M::from(mapping)),
        Err(error) => Err(oc_rsync_core::rsync_error!(1, error.to_string())
            .with_role(oc_rsync_core::message::Role::Client)),
    }
}
