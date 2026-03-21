//! Metadata preservation options and attribute flags.
//!
//! This module defines [`MetadataOptions`] for controlling which file attributes
//! are preserved during transfers, and [`AttrsFlags`] for fine-grained control
//! over time attribute application. Setter methods, accessor methods, and
//! attribute flag logic are split into focused submodules.

mod accessors;
mod attrs_flags;
mod setters;

#[cfg(test)]
mod tests;

pub use attrs_flags::AttrsFlags;

use crate::chmod::ChmodModifiers;
use crate::{GroupMapping, UserMapping};

/// Options that control metadata preservation during copy operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataOptions {
    pub(crate) preserve_owner: bool,
    pub(crate) preserve_group: bool,
    pub(crate) preserve_executability: bool,
    pub(crate) preserve_permissions: bool,
    pub(crate) preserve_times: bool,
    pub(crate) preserve_atimes: bool,
    pub(crate) preserve_crtimes: bool,
    pub(crate) numeric_ids: bool,
    pub(crate) fake_super: bool,
    pub(crate) owner_override: Option<u32>,
    pub(crate) group_override: Option<u32>,
    pub(crate) chmod: Option<ChmodModifiers>,
    pub(crate) user_mapping: Option<UserMapping>,
    pub(crate) group_mapping: Option<GroupMapping>,
}

impl MetadataOptions {
    /// Creates a new [`MetadataOptions`] value with defaults applied.
    ///
    /// By default the options preserve permissions and timestamps while leaving
    /// ownership disabled so callers can opt-in as needed.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            preserve_owner: false,
            preserve_group: false,
            preserve_executability: false,
            preserve_permissions: true,
            preserve_times: true,
            preserve_atimes: false,
            preserve_crtimes: false,
            numeric_ids: false,
            fake_super: false,
            owner_override: None,
            group_override: None,
            chmod: None,
            user_mapping: None,
            group_mapping: None,
        }
    }
}

impl Default for MetadataOptions {
    fn default() -> Self {
        Self::new()
    }
}
