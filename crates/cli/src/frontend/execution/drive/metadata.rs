#![deny(unsafe_code)]

use std::ffi::OsString;

use ::metadata::ChmodModifiers;
use ::metadata::{GroupMapping, UserMapping};
#[cfg(unix)]
use ::metadata::{MappingKind, NameMapping};

use crate::frontend::execution::chown::ParsedChown;

/// Derived metadata preservation settings used by both config and fallback logic.
pub(crate) struct MetadataSettings {
    pub(crate) preserve_owner: bool,
    pub(crate) preserve_group: bool,
    pub(crate) preserve_executability: bool,
    pub(crate) preserve_permissions: bool,
    pub(crate) preserve_times: bool,
    pub(crate) preserve_atimes: bool,
    pub(crate) preserve_crtimes: bool,
    pub(crate) omit_dir_times: bool,
    pub(crate) omit_link_times: bool,
    pub(crate) preserve_devices: bool,
    pub(crate) preserve_specials: bool,
    pub(crate) preserve_hard_links: bool,
    pub(crate) preserve_symlinks: bool,
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
    pub(crate) executability: Option<bool>,
    pub(crate) usermap: Option<&'a OsString>,
    pub(crate) groupmap: Option<&'a OsString>,
    pub(crate) perms: Option<bool>,
    pub(crate) super_mode: Option<bool>,
    pub(crate) times: Option<bool>,
    pub(crate) atimes: Option<bool>,
    pub(crate) crtimes: Option<bool>,
    pub(crate) omit_dir_times: Option<bool>,
    pub(crate) omit_link_times: Option<bool>,
    pub(crate) devices: Option<bool>,
    pub(crate) specials: Option<bool>,
    pub(crate) hard_links: Option<bool>,
    pub(crate) links: Option<bool>,
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
                return Err(core::rsync_error!(1, formatted).with_role(core::message::Role::Client));
            }
        }
    }

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
        chmod_modifiers: modifiers,
        user_mapping,
        group_mapping,
    })
}

/// Strategy-style parser interface so we can swap mapping behavior per-platform
/// (Unix implementation vs. Windows "unsupported" implementation).
trait MappingParser<M> {
    fn parse(
        &self,
        value: &OsString,
        parsed_chown: Option<&ParsedChown>,
    ) -> Result<M, core::message::Message>;
}

#[cfg(unix)]
struct UnixUserMappingParser;

#[cfg(unix)]
impl MappingParser<UserMapping> for UnixUserMappingParser {
    fn parse(
        &self,
        value: &OsString,
        parsed_chown: Option<&ParsedChown>,
    ) -> Result<UserMapping, core::message::Message> {
        if parsed_chown.and_then(|parsed| parsed.owner()).is_some() {
            return Err(core::rsync_error!(
                1,
                "--usermap conflicts with prior --chown user specification"
            )
            .with_role(core::message::Role::Client));
        }

        parse_mapping_impl(value, MappingKind::User)
    }
}

#[cfg(unix)]
struct UnixGroupMappingParser;

#[cfg(unix)]
impl MappingParser<GroupMapping> for UnixGroupMappingParser {
    fn parse(
        &self,
        value: &OsString,
        parsed_chown: Option<&ParsedChown>,
    ) -> Result<GroupMapping, core::message::Message> {
        if parsed_chown.and_then(|parsed| parsed.group()).is_some() {
            return Err(core::rsync_error!(
                1,
                "--groupmap conflicts with prior --chown group specification"
            )
            .with_role(core::message::Role::Client));
        }

        parse_mapping_impl(value, MappingKind::Group)
    }
}

#[cfg(windows)]
struct UnsupportedUserMappingParser;

#[cfg(windows)]
impl MappingParser<UserMapping> for UnsupportedUserMappingParser {
    fn parse(
        &self,
        value: &OsString,
        parsed_chown: Option<&ParsedChown>,
    ) -> Result<UserMapping, core::message::Message> {
        let _ = (value, parsed_chown);

        Err(core::rsync_error!(
            1,
            "--usermap is not supported on Windows builds of oc-rsync"
        )
        .with_role(core::message::Role::Client))
    }
}

#[cfg(windows)]
struct UnsupportedGroupMappingParser;

#[cfg(windows)]
impl MappingParser<GroupMapping> for UnsupportedGroupMappingParser {
    fn parse(
        &self,
        value: &OsString,
        parsed_chown: Option<&ParsedChown>,
    ) -> Result<GroupMapping, core::message::Message> {
        let _ = (value, parsed_chown);

        Err(core::rsync_error!(
            1,
            "--groupmap is not supported on Windows builds of oc-rsync"
        )
        .with_role(core::message::Role::Client))
    }
}

fn parse_user_mapping(
    value: &OsString,
    parsed_chown: Option<&ParsedChown>,
) -> Result<UserMapping, core::message::Message> {
    #[cfg(unix)]
    let parser = UnixUserMappingParser;

    #[cfg(windows)]
    let parser = UnsupportedUserMappingParser;

    parser.parse(value, parsed_chown)
}

fn parse_group_mapping(
    value: &OsString,
    parsed_chown: Option<&ParsedChown>,
) -> Result<GroupMapping, core::message::Message> {
    #[cfg(unix)]
    let parser = UnixGroupMappingParser;

    #[cfg(windows)]
    let parser = UnsupportedGroupMappingParser;

    parser.parse(value, parsed_chown)
}

#[cfg(unix)]
fn parse_mapping_impl<M>(value: &OsString, kind: MappingKind) -> Result<M, core::message::Message>
where
    M: From<NameMapping>,
{
    let spec = value.to_string_lossy();
    let trimmed = spec.trim();

    if trimmed.is_empty() {
        return Err(core::rsync_error!(
            1,
            format!("{} requires a non-empty mapping specification", kind.flag())
        )
        .with_role(core::message::Role::Client));
    }

    match NameMapping::parse(kind, trimmed) {
        Ok(mapping) => Ok(M::from(mapping)),
        Err(error) => {
            Err(core::rsync_error!(1, error.to_string()).with_role(core::message::Role::Client))
        }
    }
}

#[cfg(test)]
mod tests {
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

    // ==================== archive mode tests ====================

    #[test]
    fn archive_false_defaults_to_no_preservation() {
        let inputs = default_inputs();
        let result = compute_metadata_settings(inputs).unwrap();

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

    // ==================== explicit flag override tests ====================

    #[test]
    fn explicit_owner_overrides_archive() {
        let mut inputs = default_inputs();
        inputs.archive = true;
        inputs.owner = Some(false);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(!result.preserve_owner);
    }

    #[test]
    fn explicit_group_overrides_archive() {
        let mut inputs = default_inputs();
        inputs.archive = true;
        inputs.group = Some(false);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(!result.preserve_group);
    }

    #[test]
    fn explicit_perms_overrides_archive() {
        let mut inputs = default_inputs();
        inputs.archive = true;
        inputs.perms = Some(false);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(!result.preserve_permissions);
    }

    #[test]
    fn explicit_times_overrides_archive() {
        let mut inputs = default_inputs();
        inputs.archive = true;
        inputs.times = Some(false);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(!result.preserve_times);
    }

    #[test]
    fn explicit_devices_overrides_archive() {
        let mut inputs = default_inputs();
        inputs.archive = true;
        inputs.devices = Some(false);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(!result.preserve_devices);
    }

    #[test]
    fn explicit_specials_overrides_archive() {
        let mut inputs = default_inputs();
        inputs.archive = true;
        inputs.specials = Some(false);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(!result.preserve_specials);
    }

    #[test]
    fn explicit_links_overrides_archive() {
        let mut inputs = default_inputs();
        inputs.archive = true;
        inputs.links = Some(false);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(!result.preserve_symlinks);
    }

    // ==================== super_mode tests ====================

    #[test]
    fn super_mode_enables_owner() {
        let mut inputs = default_inputs();
        inputs.super_mode = Some(true);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(result.preserve_owner);
    }

    #[test]
    fn super_mode_enables_group() {
        let mut inputs = default_inputs();
        inputs.super_mode = Some(true);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(result.preserve_group);
    }

    #[test]
    fn super_mode_enables_perms() {
        let mut inputs = default_inputs();
        inputs.super_mode = Some(true);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(result.preserve_permissions);
    }

    // ==================== executability tests ====================

    #[test]
    fn executability_follows_perms_when_enabled() {
        let mut inputs = default_inputs();
        inputs.perms = Some(true);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(result.preserve_executability);
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
        let inputs = default_inputs();
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(!result.preserve_executability);
    }

    // ==================== atime/crtime tests ====================

    #[test]
    fn atimes_defaults_to_false() {
        let inputs = default_inputs();
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(!result.preserve_atimes);
    }

    #[test]
    fn atimes_explicit_true() {
        let mut inputs = default_inputs();
        inputs.atimes = Some(true);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(result.preserve_atimes);
    }

    #[test]
    fn crtimes_defaults_to_false() {
        let inputs = default_inputs();
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(!result.preserve_crtimes);
    }

    #[test]
    fn crtimes_explicit_true() {
        let mut inputs = default_inputs();
        inputs.crtimes = Some(true);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(result.preserve_crtimes);
    }

    // ==================== omit times tests ====================

    #[test]
    fn omit_dir_times_defaults_to_false() {
        let inputs = default_inputs();
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(!result.omit_dir_times);
    }

    #[test]
    fn omit_dir_times_explicit_true() {
        let mut inputs = default_inputs();
        inputs.omit_dir_times = Some(true);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(result.omit_dir_times);
    }

    #[test]
    fn omit_link_times_defaults_to_false() {
        let inputs = default_inputs();
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(!result.omit_link_times);
    }

    #[test]
    fn omit_link_times_explicit_true() {
        let mut inputs = default_inputs();
        inputs.omit_link_times = Some(true);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(result.omit_link_times);
    }

    // ==================== link handling tests ====================

    #[test]
    fn hard_links_defaults_to_false() {
        let inputs = default_inputs();
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(!result.preserve_hard_links);
    }

    #[test]
    fn hard_links_explicit_true() {
        let mut inputs = default_inputs();
        inputs.hard_links = Some(true);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(result.preserve_hard_links);
    }

    #[test]
    fn copy_links_defaults_to_false() {
        let inputs = default_inputs();
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(!result.copy_links);
    }

    #[test]
    fn copy_links_explicit_true() {
        let mut inputs = default_inputs();
        inputs.copy_links = Some(true);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(result.copy_links);
    }

    #[test]
    fn copy_unsafe_links_defaults_to_false() {
        let inputs = default_inputs();
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(!result.copy_unsafe_links);
    }

    #[test]
    fn copy_unsafe_links_explicit_true() {
        let mut inputs = default_inputs();
        inputs.copy_unsafe_links = Some(true);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(result.copy_unsafe_links);
    }

    #[test]
    fn keep_dirlinks_defaults_to_false() {
        let inputs = default_inputs();
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(!result.keep_dirlinks);
    }

    #[test]
    fn keep_dirlinks_explicit_true() {
        let mut inputs = default_inputs();
        inputs.keep_dirlinks = Some(true);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(result.keep_dirlinks);
    }

    // ==================== sparse tests ====================

    #[test]
    fn sparse_defaults_to_false() {
        let inputs = default_inputs();
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(!result.sparse);
    }

    #[test]
    fn sparse_explicit_true() {
        let mut inputs = default_inputs();
        inputs.sparse = Some(true);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(result.sparse);
    }

    // ==================== relative and one_file_system tests ====================

    #[test]
    fn relative_defaults_to_false() {
        let inputs = default_inputs();
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(!result.relative);
    }

    #[test]
    fn relative_explicit_true() {
        let mut inputs = default_inputs();
        inputs.relative = Some(true);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(result.relative);
    }

    #[test]
    fn one_file_system_defaults_to_false() {
        let inputs = default_inputs();
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(!result.one_file_system);
    }

    #[test]
    fn one_file_system_explicit_true() {
        let mut inputs = default_inputs();
        inputs.one_file_system = Some(true);
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(result.one_file_system);
    }

    // ==================== chmod tests ====================

    #[test]
    fn no_chmod_returns_none() {
        let inputs = default_inputs();
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(result.chmod_modifiers.is_none());
    }

    #[test]
    fn valid_chmod_parses() {
        let chmod_specs = [OsString::from("u+rwx")];
        let mut inputs = default_inputs();
        inputs.chmod = &chmod_specs;
        let result = compute_metadata_settings(inputs).unwrap();

        assert!(result.chmod_modifiers.is_some());
    }

    #[test]
    fn invalid_chmod_returns_error() {
        let chmod_specs = [OsString::from("invalid_chmod_spec")];
        let mut inputs = default_inputs();
        inputs.chmod = &chmod_specs;
        let result = compute_metadata_settings(inputs);

        assert!(result.is_err());
    }

    // ==================== MetadataSettings struct tests ====================

    #[test]
    fn metadata_settings_all_fields_accessible() {
        let inputs = default_inputs();
        let settings = compute_metadata_settings(inputs).unwrap();

        // Just verify all fields are accessible
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

    // ==================== MetadataInputs struct tests ====================

    #[test]
    fn metadata_inputs_default_helper() {
        let inputs = default_inputs();
        assert!(!inputs.archive);
        assert!(inputs.parsed_chown.is_none());
        assert!(inputs.owner.is_none());
        assert!(inputs.chmod.is_empty());
    }
}
