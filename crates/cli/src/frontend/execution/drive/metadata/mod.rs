//! Metadata preservation flag resolution for CLI-driven transfers.
//!
//! Translates raw CLI options into the derived [`MetadataSettings`] consumed
//! by config construction. Handles archive-mode expansion, `--super` escalation,
//! and platform-specific user/group mapping parsing.

mod compute;
mod mapping;
mod types;

pub(crate) use compute::compute_metadata_settings;
pub(crate) use types::{MetadataInputs, MetadataSettings};

#[cfg(test)]
mod tests;
