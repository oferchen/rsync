#![cfg(target_os = "windows")]

//! Windows-specific metadata helpers.
//!
//! Houses native NTFS metadata facilities that have no upstream rsync
//! analogue. Upstream rsync delegates Windows-specific behaviour to Cygwin,
//! which emulates a POSIX surface on top of Win32 (for example by treating
//! every reparse point as a symbolic link). Native `oc-rsync` interacts with
//! the NTFS layer directly, so this module provides primitives that bridge
//! Win32 semantics to the cross-platform metadata layer.
//!
//! Submodules:
//! - [`reparse`] classifies NTFS reparse points into their concrete kinds
//!   (symlink, junction, mount-point, OneDrive/cloud placeholder, AF_UNIX
//!   socket, or an opaque tag value) so higher layers can decide how to
//!   transfer each kind without losing information.

/// NTFS reparse-point classifier (symlink, junction, mount-point,
/// cloud placeholder, WSL `AF_UNIX` socket, opaque tag).
pub mod reparse;

pub use reparse::{ReparseKind, classify_reparse_point};
