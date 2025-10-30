#![deny(unsafe_code)]

//! Daemon configuration builders.
//!
//! This module encapsulates the immutable configuration handed to the daemon
//! runtime together with a builder that callers can use to assemble the final
//! argument vector. Keeping the types isolated from the main runtime keeps the
//! large daemon state machine manageable while enforcing consistent branding
//! and default-path behaviour across the workspace.

use std::ffi::OsString;

use rsync_core::branding::Brand;

/// Configuration describing the requested daemon operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonConfig {
    brand: Brand,
    arguments: Vec<OsString>,
    load_default_paths: bool,
}

impl DaemonConfig {
    /// Creates a new [`DaemonConfigBuilder`].
    #[must_use]
    pub fn builder() -> DaemonConfigBuilder {
        DaemonConfigBuilder::default()
    }

    /// Returns the raw arguments supplied to the daemon.
    #[must_use]
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    /// Returns the branding profile associated with the daemon invocation.
    #[must_use]
    pub const fn brand(&self) -> Brand {
        self.brand
    }

    /// Indicates whether default configuration and secrets paths should be
    /// consulted when parsing runtime options.
    #[must_use]
    pub const fn load_default_paths(&self) -> bool {
        self.load_default_paths
    }

    /// Reports whether any daemon-specific arguments were provided.
    #[must_use]
    pub fn has_runtime_request(&self) -> bool {
        !self.arguments.is_empty()
    }
}

/// Builder used to assemble a [`DaemonConfig`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonConfigBuilder {
    brand: Brand,
    arguments: Vec<OsString>,
    load_default_paths: bool,
}

impl Default for DaemonConfigBuilder {
    fn default() -> Self {
        Self {
            brand: Brand::Oc,
            arguments: Vec::new(),
            load_default_paths: true,
        }
    }
}

impl DaemonConfigBuilder {
    /// Selects the branding profile that should be used for this configuration.
    #[must_use]
    pub fn brand(mut self, brand: Brand) -> Self {
        self.brand = brand;
        self
    }

    /// Supplies the arguments that should be forwarded to the daemon loop once implemented.
    #[must_use]
    pub fn arguments<I, S>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.arguments = arguments.into_iter().map(Into::into).collect();
        self
    }

    /// Skips discovery of default configuration and secrets paths.
    #[must_use]
    pub fn disable_default_paths(mut self) -> Self {
        self.load_default_paths = false;
        self
    }

    /// Finalises the builder and constructs the [`DaemonConfig`].
    #[must_use]
    pub fn build(self) -> DaemonConfig {
        DaemonConfig {
            brand: self.brand,
            arguments: self.arguments,
            load_default_paths: self.load_default_paths,
        }
    }
}
