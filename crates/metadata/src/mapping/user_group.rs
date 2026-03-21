#![allow(unsafe_code)]

//! Typed wrappers for user and group mappings.
//!
//! [`UserMapping`] and [`GroupMapping`] provide type-safe facades over
//! [`NameMapping`], ensuring that user mappings only resolve UIDs and group
//! mappings only resolve GIDs.

use rustix::process::{RawGid, RawUid};
use std::io;

use super::name_mapping::NameMapping;
use super::types::{MappingKind, MappingParseError};

/// Parsed `--usermap` rules.
///
/// Wraps a [`NameMapping`] configured for user ID resolution.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UserMapping(NameMapping);

impl UserMapping {
    /// Parses a `--usermap` specification.
    pub fn parse(spec: &str) -> Result<Self, MappingParseError> {
        NameMapping::parse(MappingKind::User, spec).map(Self)
    }

    /// Applies the mapping to the supplied UID.
    pub(crate) fn map_uid(&self, uid: RawUid) -> io::Result<Option<RawUid>> {
        self.0.map_uid(uid)
    }

    /// Reports whether the mapping contains any rules.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Parsed `--groupmap` rules.
///
/// Wraps a [`NameMapping`] configured for group ID resolution.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GroupMapping(NameMapping);

impl GroupMapping {
    /// Parses a `--groupmap` specification.
    pub fn parse(spec: &str) -> Result<Self, MappingParseError> {
        NameMapping::parse(MappingKind::Group, spec).map(Self)
    }

    /// Applies the mapping to the supplied GID.
    pub(crate) fn map_gid(&self, gid: RawGid) -> io::Result<Option<RawGid>> {
        self.0.map_gid(gid)
    }

    /// Reports whether the mapping contains any rules.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<NameMapping> for UserMapping {
    fn from(mapping: NameMapping) -> Self {
        Self(mapping)
    }
}

impl From<NameMapping> for GroupMapping {
    fn from(mapping: NameMapping) -> Self {
        Self(mapping)
    }
}
