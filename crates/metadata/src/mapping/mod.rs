#![allow(unsafe_code)]

//! UID/GID mapping for `--usermap` and `--groupmap` options.
//!
//! Parses comma-separated mapping specifications of the form `source:target`
//! where the source can be a name, numeric ID, numeric range, or wildcard
//! pattern and the target is a name or numeric ID. Rules are evaluated in
//! order - first match wins. This mirrors the mapping logic in upstream
//! rsync's `uidlist.c`.

mod name_mapping;
mod parse;
#[cfg(test)]
mod tests;
mod types;
mod user_group;
mod wildcard;

pub use name_mapping::NameMapping;
pub use types::{MappingKind, MappingParseError};
pub use user_group::{GroupMapping, UserMapping};
