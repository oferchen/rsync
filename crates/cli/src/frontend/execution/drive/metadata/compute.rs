//! Resolution of CLI metadata flags into derived preservation settings.
//!
//! Implements upstream rsync's precedence rules: explicit flags override
//! `--super`, which overrides `--archive`, which defaults to off.

use ::metadata::ChmodModifiers;

use super::mapping::{parse_group_mapping, parse_user_mapping};
use super::types::{MetadataInputs, MetadataSettings};

/// Computes metadata preservation flags according to upstream semantics.
///
/// Precedence (highest to lowest):
/// 1. Explicit per-flag options (`--owner`, `--no-owner`, etc.)
/// 2. `--chown` / `--usermap` / `--groupmap` imply ownership preservation
/// 3. `--super` implies owner, group, and permissions
/// 4. `--archive` sets the baseline for most flags
pub(crate) fn compute_metadata_settings(
    inputs: MetadataInputs<'_>,
) -> Result<MetadataSettings, core::message::Message> {
    let MetadataInputs {
        archive,
        parsed_chown,
        owner,
        group,
        executability,
        perms,
        usermap,
        groupmap,
        super_mode,
        times,
        atimes,
        crtimes,
        omit_dir_times,
        omit_link_times,
        devices,
        specials,
        hard_links,
        links,
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

    let preserve_executability = if preserve_permissions {
        true
    } else {
        executability.unwrap_or(false)
    };

    let preserve_times = times.unwrap_or(archive);
    let preserve_atimes = atimes.unwrap_or(false);
    let preserve_crtimes = crtimes.unwrap_or(false);
    let omit_dir_times_setting = omit_dir_times.unwrap_or(false);
    let omit_link_times_setting = omit_link_times.unwrap_or(false);
    let preserve_devices = devices.unwrap_or(archive);
    let preserve_specials = specials.unwrap_or(archive);
    let preserve_hard_links = hard_links.unwrap_or(false);
    let preserve_symlinks = if let Some(value) = links {
        value
    } else {
        archive
    };
    let sparse_setting = sparse.unwrap_or(false);
    let copy_links_setting = copy_links.unwrap_or(false);
    let copy_unsafe_links_setting = copy_unsafe_links.unwrap_or(false);
    let keep_dirlinks_flag = keep_dirlinks.unwrap_or(false);
    let relative_paths = relative.unwrap_or(false);
    let one_file_system_setting = one_file_system.unwrap_or(0);

    let chmod_modifiers = parse_chmod_specs(chmod)?;

    Ok(MetadataSettings {
        preserve_owner,
        preserve_group,
        preserve_executability,
        preserve_permissions,
        preserve_times,
        preserve_atimes,
        preserve_crtimes,
        omit_dir_times: omit_dir_times_setting,
        omit_link_times: omit_link_times_setting,
        preserve_devices,
        preserve_specials,
        preserve_hard_links,
        preserve_symlinks,
        sparse: sparse_setting,
        copy_links: copy_links_setting,
        copy_unsafe_links: copy_unsafe_links_setting,
        keep_dirlinks: keep_dirlinks_flag,
        relative: relative_paths,
        one_file_system: one_file_system_setting,
        chmod_modifiers,
        user_mapping,
        group_mapping,
    })
}

/// Parses and merges all `--chmod` specifications into a single modifier set.
fn parse_chmod_specs(
    specs: &[std::ffi::OsString],
) -> Result<Option<ChmodModifiers>, core::message::Message> {
    let mut modifiers: Option<ChmodModifiers> = None;
    for spec in specs {
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
                return Err(core::rsync_error!(1, formatted).with_role(core::message::Role::Client));
            }
        }
    }
    Ok(modifiers)
}
