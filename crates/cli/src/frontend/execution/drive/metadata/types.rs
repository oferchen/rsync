//! Data types for metadata preservation settings and CLI inputs.

use std::ffi::OsString;

use ::metadata::ChmodModifiers;
use ::metadata::{GroupMapping, UserMapping};

use crate::frontend::execution::chown::ParsedChown;

/// Derived metadata preservation settings used by config construction.
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
    pub(crate) one_file_system: u8,
    pub(crate) chmod_modifiers: Option<ChmodModifiers>,
    pub(crate) user_mapping: Option<UserMapping>,
    pub(crate) group_mapping: Option<GroupMapping>,
}

/// Raw CLI inputs for metadata flag resolution.
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
    pub(crate) one_file_system: Option<u8>,
    pub(crate) chmod: &'a [OsString],
}
