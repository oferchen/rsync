//! Helpers for rendering [`Message`](crate::message::Message) values as vectored slices.
//!
//! The [`MessageSegments`] type exposes the low-level representation that the
//! logging subsystem streams into stdout/stderr. This submodule keeps the
//! implementation split across focussed files so that each area – iterators,
//! vectored I/O, and buffer-friendly helpers – remains under the workspace line
//! limits while still providing comprehensive rustdoc coverage.

mod base;
mod buffer;
mod error;
mod io;

pub use base::MessageSegments;
pub use error::CopyToSliceError;

// The public API is re-exported from [`super`] so callers only interact with
// `crate::message::segments` rather than the internal file layout. The helper
// modules above provide `impl MessageSegments` blocks that extend the core
// behaviour defined in [`base`].
