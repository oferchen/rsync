#![cfg(not(unix))]

//! User/group mapping stubs for Windows.
//!
//! Windows lacks POSIX user/group concepts, so ownership mapping options
//! (`--usermap`, `--groupmap`, `--chown`) are not supported. This module
//! provides placeholder types that return errors when parsing is attempted.
//!
//! # Platform Behavior
//!
//! This matches upstream rsync behavior where ownership-related options
//! are unavailable on platforms without POSIX user databases.

/// Kind of mapping requested (Windows stub).
///
/// On Windows, no mapping types are supported since the platform lacks
/// POSIX-style user/group ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingKind {
    /// No mapping supported on this platform.
    None,
}

/// Error returned when a mapping string cannot be parsed.
///
/// On Windows, this is always returned since user/group mapping
/// is not supported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingParseError;

/// User mapping placeholder for Windows.
///
/// This type exists for API compatibility but cannot be constructed
/// since user mapping is not supported on Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMapping;

/// Group mapping placeholder for Windows.
///
/// This type exists for API compatibility but cannot be constructed
/// since group mapping is not supported on Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMapping;

/// Name mapping placeholder for Windows.
///
/// This type exists for API compatibility but cannot be constructed
/// since name mapping is not supported on Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameMapping;
