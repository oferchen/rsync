//! Platform-specific user and group mapping parsers.
//!
//! Uses a strategy pattern to swap mapping behavior per platform:
//! Unix resolves names via the system database; Windows returns an
//! unsupported error.

use std::ffi::OsString;

use ::metadata::{GroupMapping, UserMapping};
#[cfg(unix)]
use ::metadata::{MappingKind, NameMapping};

use crate::frontend::execution::chown::ParsedChown;

/// Strategy interface for parsing user/group mapping specifications.
trait MappingParser<M> {
    fn parse(
        &self,
        value: &OsString,
        parsed_chown: Option<&ParsedChown>,
    ) -> Result<M, core::message::Message>;
}

// ---------------------------------------------------------------------------
// Unix implementations
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Windows implementations
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Parses a `--usermap` specification into a [`UserMapping`].
pub(super) fn parse_user_mapping(
    value: &OsString,
    parsed_chown: Option<&ParsedChown>,
) -> Result<UserMapping, core::message::Message> {
    #[cfg(unix)]
    let parser = UnixUserMappingParser;

    #[cfg(windows)]
    let parser = UnsupportedUserMappingParser;

    parser.parse(value, parsed_chown)
}

/// Parses a `--groupmap` specification into a [`GroupMapping`].
pub(super) fn parse_group_mapping(
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
