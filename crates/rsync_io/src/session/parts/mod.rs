//! Decomposed [`SessionHandshake`] helpers shared across binary and legacy flows.
//!
//! The module is split across smaller files to respect the workspace line-count
//! guardrails while preserving the public API. `state` declares the
//! [`SessionHandshakeParts`] enum and constructors, `accessors` provides
//! read-only helpers, `ownership` exposes transformations that move the inner
//! transport or reconstruct higher level abstractions, and `conversions`
//! implements the standard `From`/`TryFrom` traits.

mod accessors;
mod conversions;
mod ownership;
mod state;

pub use state::SessionHandshakeParts;
