#![deny(unsafe_code)]

//! # Overview
//!
//! `oc_rsync_core::version` centralises the workspace version constants and
//! feature-detection helpers that drive the `--version` output of the Rust
//! `rsync` binaries. The module mirrors upstream rsync 3.4.1 by exposing the
//! canonical base version while appending the `-rust` suffix that brands this
//! implementation.
//!
//! # Design
//!
//! The module publishes lightweight enums and helper functions:
//!
//! - [`RUST_VERSION`](crate::version::RUST_VERSION) holds the `3.4.1-rust`
//!   identifier rendered by user-visible banners.
//! - [`compiled_features`](crate::version::compiled_features) inspects Cargo
//!   feature flags and returns the set of optional capabilities enabled at build
//!   time.
//! - [`compiled_features_static`](crate::version::compiled_features_static)
//!   exposes a zero-allocation view for repeated inspections of the compiled
//!   feature set.
//! - [`CompiledFeature`](crate::version::CompiledFeature) enumerates optional
//!   capabilities and provides label helpers such as
//!   [`crate::version::CompiledFeature::label`] and
//!   [`crate::version::CompiledFeature::from_label`] for parsing user-provided
//!   strings.
//! - [`VersionInfoReport`](crate::version::VersionInfoReport) renders the full
//!   `--version` text, including capability sections and checksum/compressor
//!   listings, so the CLI can display upstream-identical banners branded for
//!   `rsync`.
//!
//! This structure keeps other crates free of conditional compilation logic
//! while avoiding string duplication across the workspace.
//!
//! # Invariants
//!
//! - [`RUST_VERSION`](crate::version::RUST_VERSION) always embeds the upstream
//!   base release so diagnostics and CLI output remain aligned with rsync 3.4.1.
//! - [`compiled_features`](crate::version::compiled_features) never invents
//!   capabilities: it only reports flags that were explicitly enabled when
//!   compiling `rsync-core`.
//!
//! # Errors
//!
//! The module exposes
//! [`ParseCompiledFeatureError`](crate::version::ParseCompiledFeatureError)
//! when parsing a [`crate::version::CompiledFeature`] from a string fails. All
//! other helpers return constants or eagerly evaluate into owned collections.
//!
//! # Examples
//!
//! Retrieve the compiled feature list for the current build. Optional
//! capabilities appear when their corresponding Cargo features are enabled at
//! compile time.
//!
//! ```
//! use oc_rsync_core::version::{compiled_features, CompiledFeature, RUST_VERSION};
//!
//! assert_eq!(RUST_VERSION, env!("CARGO_PKG_VERSION"));
//! let features = compiled_features();
//! #[cfg(feature = "xattr")]
//! assert!(features.contains(&CompiledFeature::Xattr));
//! #[cfg(not(feature = "xattr"))]
//! assert!(features.is_empty());
//! ```
//!
//! # See also
//!
//! - [`crate::message`] uses [`crate::version::RUST_VERSION`] when rendering
//!   error trailers.
//! - Future CLI modules rely on [`crate::version::compiled_features`] and
//!   [`crate::version::VersionInfoReport`] to mirror upstream `--version`
//!   capability listings while advertising the Rust-branded binary name.

mod constants;
mod features;
mod metadata;
mod report;
mod secluded_args;

pub use constants::{
    BUILD_TOOLCHAIN, COPYRIGHT_NOTICE, COPYRIGHT_START_YEAR, DAEMON_PROGRAM_NAME,
    HIGHEST_PROTOCOL_VERSION, LATEST_COPYRIGHT_YEAR, OC_DAEMON_PROGRAM_NAME,
    OC_DAEMON_WRAPPER_PROGRAM_NAME, OC_PROGRAM_NAME, PROGRAM_NAME, RUST_VERSION, SOURCE_URL,
    SUBPROTOCOL_VERSION, UPSTREAM_BASE_VERSION, build_revision,
};
pub use features::{
    COMPILED_FEATURE_BITMAP, CompiledFeature, CompiledFeaturesDisplay, CompiledFeaturesIter,
    ParseCompiledFeatureError, StaticCompiledFeatures, StaticCompiledFeaturesIter,
    compiled_feature_labels, compiled_features, compiled_features_display, compiled_features_iter,
    compiled_features_static,
};
pub use metadata::{
    VersionMetadata, daemon_version_metadata, oc_daemon_version_metadata, oc_version_metadata,
    version_metadata, version_metadata_for_client_brand, version_metadata_for_daemon_brand,
    version_metadata_for_program,
};
pub use report::{VersionInfoConfig, VersionInfoConfigBuilder, VersionInfoReport};
pub use secluded_args::{ParseSecludedArgsModeError, SecludedArgsMode};

#[cfg(test)]
mod tests;
