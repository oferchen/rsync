#![allow(unsafe_code)]

//! Core types for UID/GID mapping specifications.
//!
//! Defines the matcher, target, and rule primitives used by `--usermap` and
//! `--groupmap`. These correspond to the mapping logic in upstream rsync's
//! `uidlist.c`.

use crate::id_lookup::{lookup_group_by_name, lookup_user_by_name};
use rustix::process::{RawGid, RawUid};
use std::io;
use thiserror::Error;

use super::wildcard::wildcard_matches;

/// Represents the role associated with a name-mapping specification.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MappingKind {
    /// User mapping specified via `--usermap`.
    #[default]
    User,
    /// Group mapping specified via `--groupmap`.
    Group,
}

impl MappingKind {
    /// Returns the command-line flag associated with the mapping kind.
    #[must_use]
    pub const fn flag(self) -> &'static str {
        match self {
            Self::User => "--usermap",
            Self::Group => "--groupmap",
        }
    }
}

/// Error returned when parsing a mapping specification fails.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
#[error("{message}")]
pub struct MappingParseError {
    kind: MappingKind,
    message: String,
}

impl MappingParseError {
    /// Creates a new parse error for the given mapping kind.
    pub(crate) fn new(kind: MappingKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Returns the mapping kind associated with this error.
    #[must_use]
    pub const fn kind(&self) -> MappingKind {
        self.kind
    }
}

/// Matcher half of a mapping rule - determines which IDs/names are affected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MappingMatcher {
    /// Matches any identifier (`*`).
    Any,
    /// Matches a numeric ID range (inclusive).
    IdRange { start: u32, end: u32 },
    /// Matches an exact name string.
    ExactName(String),
    /// Matches a wildcard pattern (supports `*`, `?`, `[...]`).
    Pattern(String),
}

impl MappingMatcher {
    /// Tests whether the given identifier matches this matcher.
    ///
    /// The `name_lookup` closure is called lazily only when the matcher
    /// requires name resolution.
    pub(crate) fn matches<F>(&self, identifier: u32, mut name_lookup: F) -> io::Result<bool>
    where
        F: FnMut() -> io::Result<Option<String>>,
    {
        Ok(match self {
            Self::Any => true,
            Self::IdRange { start, end } => (identifier >= *start) && (identifier <= *end),
            Self::ExactName(expected) => {
                if let Some(name) = name_lookup()? {
                    name == *expected
                } else {
                    false
                }
            }
            Self::Pattern(pattern) => {
                if let Some(name) = name_lookup()? {
                    wildcard_matches(pattern, &name)
                } else {
                    false
                }
            }
        })
    }
}

/// Target half of a mapping rule - the ID or name to map to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MappingTarget {
    /// A numeric UID/GID.
    Id(u32),
    /// A name to be resolved at application time.
    Name(String),
}

impl MappingTarget {
    /// Resolves this target to a UID.
    pub(crate) fn resolve_uid(&self) -> io::Result<RawUid> {
        match self {
            Self::Id(id) => Ok(*id as RawUid),
            Self::Name(name) => match lookup_user_by_name(name.as_bytes())? {
                Some(uid) => Ok(uid),
                None => Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Unknown --usermap name on receiver: {name}"),
                )),
            },
        }
    }

    /// Resolves this target to a GID.
    pub(crate) fn resolve_gid(&self) -> io::Result<RawGid> {
        match self {
            Self::Id(id) => Ok(*id as RawGid),
            Self::Name(name) => match lookup_group_by_name(name.as_bytes())? {
                Some(gid) => Ok(gid),
                None => Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Unknown --groupmap name on receiver: {name}"),
                )),
            },
        }
    }
}

/// A single mapping rule consisting of a matcher and a target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MappingRule {
    /// The matcher that determines which IDs/names this rule applies to.
    pub(crate) matcher: MappingMatcher,
    /// The target ID or name to map to when the matcher succeeds.
    pub(crate) target: MappingTarget,
}
