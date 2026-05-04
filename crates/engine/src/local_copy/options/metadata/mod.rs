//! Metadata preservation setters, accessors, and privilege helpers for
//! [`LocalCopyOptions`](super::types::LocalCopyOptions).
//!
//! Setter methods consume `self` and return a modified instance for fluent
//! configuration chains. Accessor methods borrow `&self` and return the
//! current value. The split follows the single-responsibility pattern used
//! throughout the options module hierarchy.

mod accessors;
mod setters;

#[cfg(test)]
mod tests;
