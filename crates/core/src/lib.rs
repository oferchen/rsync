#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `rsync_core` exposes workspace-wide facilities that are shared between the
//! client, daemon, and transport binaries. The crate focuses on user-visible
//! message formatting and source-location remapping so diagnostics match
//! upstream rsync while referencing the Rust sources.
//!
//! # Design
//!
//! The current surface consists of the [`message`] module. It implements
//! [`Message`] together with helpers such as [`message::message_source`] for
//! capturing repo-relative source locations. Higher layers construct messages
//! through this API to ensure trailer roles and version suffixes are formatted
//! consistently.
//!
//! # Invariants
//!
//! - Message trailers always include the `3.4.1-rust` version string.
//! - Source locations are normalised to repo-relative POSIX-style paths, even on
//!   Windows builds.
//! - Errors never allocate excessively: formatting a [`Message`] touches only
//!   the stored payload and metadata.
//!
//! # Errors
//!
//! The crate does not define new error types. Instead, it provides utilities
//! that propagate upstream rsync error codes via [`Message::error`].
//!
//! # Examples
//!
//! Create an error message using the helper APIs and render it into the
//! canonical user-facing form.
//!
//! ```
//! use rsync_core::{message::Message, message::Role, message_source};
//!
//! let rendered = Message::error(23, "delta-transfer failure")
//!     .with_role(Role::Sender)
//!     .with_source(message_source!())
//!     .to_string();
//!
//! assert!(rendered.contains("rsync error: delta-transfer failure (code 23)"));
//! assert!(rendered.contains("[sender=3.4.1-rust]"));
//! ```
//!
//! # See also
//!
//! - [`rsync_core::message::strings`] exposes upstream-aligned exit-code wording
//!   so higher layers render identical diagnostics.
//! - [`rsync_protocol`] for the negotiation helpers that feed protocol numbers
//!   into user-facing diagnostics.
//! - [`rsync_transport`] for replaying transport wrappers that emit these
//!   messages when negotiation fails.

/// Message formatting utilities shared across workspace binaries.
pub mod message;
