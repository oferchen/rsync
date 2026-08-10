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
    /// When true, the destination file was newly created during this transfer.
    /// upstream: rsync.c:dest_mode() uses `exists` parameter to distinguish
    /// between new and existing files for permission computation.
    pub(crate) destination_is_new: bool,
    /// When true, `--keep-dirlinks` is active: dest-side symlinks pointing to
    /// real directories are followed instead of being replaced.
    ///
    /// upstream: generator.c:1344 - `link_stat(fname, &sx.st, keep_dirlinks && is_dir)`
    /// resolves symlinked dest dirs at stat time, so subsequent chmod/chown
    /// operations land on the canonical real path. We mirror that by bypassing
    /// the dirfd-anchored sandbox in `secure_chmod_at` when this flag is set:
    /// the user has explicitly opted into following dest-side symlinks, which
    /// is incompatible with `secure_open_dir`'s ELOOP/ENOTDIR rejection of
    /// symlinked parents.
    pub(crate) keep_dirlinks: bool,
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
            destination_is_new: false,
            keep_dirlinks: false,
        }
    }
}

impl Default for MetadataOptions {
    fn default() -> Self {
        Self::new()
    }
}
