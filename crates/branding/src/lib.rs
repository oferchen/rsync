#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]

mod generated;

pub mod branding;
pub mod validation;
pub mod workspace;

pub use generated::{BUILD_REVISION, BUILD_TOOLCHAIN};

/// Returns the sanitized build revision embedded in the binaries.
#[must_use]
pub const fn build_revision() -> &'static str {
    BUILD_REVISION
}

/// Returns the human-readable toolchain description rendered by version banners.
#[must_use]
pub const fn build_toolchain() -> &'static str {
    BUILD_TOOLCHAIN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_helpers_expose_generated_constants() {
        assert_eq!(build_toolchain(), BUILD_TOOLCHAIN);
        assert_eq!(build_revision(), BUILD_REVISION);
    }
}
