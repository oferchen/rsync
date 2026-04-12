#![deny(unsafe_code)]

//! Daemon configuration builders.
//!
//! This module encapsulates the immutable configuration handed to the daemon
//! runtime together with a builder that callers can use to assemble the final
//! argument vector. Keeping the types isolated from the main runtime keeps the
//! large daemon state machine manageable while enforcing consistent branding
//! and default-path behaviour across the workspace.

use std::ffi::OsString;

use core::branding::Brand;
use platform::signal::SignalFlags;

/// Configuration describing the requested daemon operation.
#[derive(Clone, Debug)]
pub struct DaemonConfig {
    brand: Brand,
    arguments: Vec<OsString>,
    load_default_paths: bool,
    /// External signal flags injected by the Windows Service dispatcher.
    ///
    /// When running as a Windows service, the SCM control handler sets these
    /// flags in response to stop/shutdown/paramchange events. When `None`,
    /// the daemon registers its own platform signal handlers on startup.
    signal_flags: Option<SignalFlags>,
}

impl PartialEq for DaemonConfig {
    fn eq(&self, other: &Self) -> bool {
        self.brand == other.brand
            && self.arguments == other.arguments
            && self.load_default_paths == other.load_default_paths
    }
}

impl Eq for DaemonConfig {}

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

    /// Returns externally injected signal flags, if present.
    ///
    /// When running as a Windows service, the SCM control handler writes to
    /// these flags. The daemon accept loop uses them instead of registering
    /// its own platform signal handlers.
    #[must_use]
    pub fn take_signal_flags(&mut self) -> Option<SignalFlags> {
        self.signal_flags.take()
    }

    /// Reports whether any daemon-specific arguments were provided.
    #[must_use]
    pub const fn has_runtime_request(&self) -> bool {
        !self.arguments.is_empty()
    }
}

/// Builder used to assemble a [`DaemonConfig`].
#[derive(Clone, Debug)]
pub struct DaemonConfigBuilder {
    brand: Brand,
    arguments: Vec<OsString>,
    load_default_paths: bool,
    signal_flags: Option<SignalFlags>,
}

impl PartialEq for DaemonConfigBuilder {
    fn eq(&self, other: &Self) -> bool {
        self.brand == other.brand
            && self.arguments == other.arguments
            && self.load_default_paths == other.load_default_paths
    }
}

impl Eq for DaemonConfigBuilder {}

impl Default for DaemonConfigBuilder {
    fn default() -> Self {
        Self {
            brand: Brand::Oc,
            arguments: Vec::new(),
            load_default_paths: true,
            signal_flags: None,
        }
    }
}

impl DaemonConfigBuilder {
    /// Selects the branding profile that should be used for this configuration.
    #[must_use]
    pub const fn brand(mut self, brand: Brand) -> Self {
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
    pub const fn disable_default_paths(mut self) -> Self {
        self.load_default_paths = false;
        self
    }

    /// Injects external signal flags from the Windows Service dispatcher.
    ///
    /// When set, the daemon accept loop uses these flags instead of
    /// registering its own platform signal handlers. The SCM control handler
    /// writes to these flags in response to stop/shutdown/paramchange events.
    #[must_use]
    pub fn signal_flags(mut self, flags: SignalFlags) -> Self {
        self.signal_flags = Some(flags);
        self
    }

    /// Finalises the builder and constructs the [`DaemonConfig`].
    #[must_use]
    pub fn build(self) -> DaemonConfig {
        DaemonConfig {
            brand: self.brand,
            arguments: self.arguments,
            load_default_paths: self.load_default_paths,
            signal_flags: self.signal_flags,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod daemon_config_tests {
        use super::*;

        #[test]
        fn builder_default() {
            let config = DaemonConfig::builder().build();

            assert_eq!(config.brand(), Brand::Oc);
            assert!(config.arguments().is_empty());
            assert!(config.load_default_paths());
            assert!(!config.has_runtime_request());
        }

        #[test]
        fn builder_with_brand() {
            let config = DaemonConfig::builder().brand(Brand::Upstream).build();

            assert_eq!(config.brand(), Brand::Upstream);
        }

        #[test]
        fn builder_with_arguments() {
            let config = DaemonConfig::builder()
                .arguments(["--port", "8873", "--once"])
                .build();

            assert_eq!(config.arguments().len(), 3);
            assert_eq!(config.arguments()[0], "--port");
            assert_eq!(config.arguments()[1], "8873");
            assert_eq!(config.arguments()[2], "--once");
            assert!(config.has_runtime_request());
        }

        #[test]
        fn builder_disable_default_paths() {
            let config = DaemonConfig::builder().disable_default_paths().build();

            assert!(!config.load_default_paths());
        }

        #[test]
        fn builder_chained() {
            let config = DaemonConfig::builder()
                .brand(Brand::Upstream)
                .arguments(["--config", "/etc/rsyncd.conf"])
                .disable_default_paths()
                .build();

            assert_eq!(config.brand(), Brand::Upstream);
            assert_eq!(config.arguments().len(), 2);
            assert!(!config.load_default_paths());
            assert!(config.has_runtime_request());
        }

        #[test]
        fn clone_and_eq() {
            let config = DaemonConfig::builder()
                .brand(Brand::Oc)
                .arguments(["--once"])
                .build();
            let cloned = config.clone();

            assert_eq!(config, cloned);
        }

        #[test]
        fn debug_format() {
            let config = DaemonConfig::builder().build();
            let debug = format!("{config:?}");

            assert!(debug.contains("DaemonConfig"));
            assert!(debug.contains("brand"));
        }

        #[test]
        fn builder_default_has_no_signal_flags() {
            let mut config = DaemonConfig::builder().build();
            assert!(config.take_signal_flags().is_none());
        }

        #[test]
        fn builder_with_signal_flags() {
            let flags = SignalFlags::new();
            let mut config = DaemonConfig::builder().signal_flags(flags).build();
            let taken = config.take_signal_flags();
            assert!(taken.is_some());
        }

        #[test]
        fn take_signal_flags_returns_none_on_second_call() {
            let flags = SignalFlags::new();
            let mut config = DaemonConfig::builder().signal_flags(flags).build();
            assert!(config.take_signal_flags().is_some());
            assert!(config.take_signal_flags().is_none());
        }

        #[test]
        fn signal_flags_share_atomics_with_original() {
            use std::sync::atomic::Ordering;
            let flags = SignalFlags::new();
            let shutdown = flags.shutdown.clone();
            let mut config = DaemonConfig::builder().signal_flags(flags).build();
            let taken = config.take_signal_flags().unwrap();
            shutdown.store(true, Ordering::Relaxed);
            assert!(taken.shutdown.load(Ordering::Relaxed));
        }
    }

    mod daemon_config_builder_tests {
        use super::*;

        #[test]
        fn default() {
            let builder = DaemonConfigBuilder::default();
            let config = builder.build();

            assert_eq!(config.brand(), Brand::Oc);
            assert!(config.load_default_paths());
        }

        #[test]
        fn clone_and_eq() {
            let builder = DaemonConfigBuilder::default();
            let cloned = builder.clone();

            assert_eq!(builder, cloned);
        }

        #[test]
        fn debug_format() {
            let builder = DaemonConfigBuilder::default();
            let debug = format!("{builder:?}");

            assert!(debug.contains("DaemonConfigBuilder"));
        }

        #[test]
        fn arguments_from_vec() {
            let args = vec!["--help".to_owned(), "--version".to_owned()];
            let config = DaemonConfig::builder().arguments(args).build();

            assert_eq!(config.arguments().len(), 2);
        }

        #[test]
        fn arguments_from_osstrings() {
            let args = vec![OsString::from("--once")];
            let config = DaemonConfig::builder().arguments(args).build();

            assert_eq!(config.arguments().len(), 1);
            assert_eq!(config.arguments()[0], "--once");
        }

        #[test]
        fn signal_flags_chained_with_other_options() {
            let flags = SignalFlags::new();
            let mut config = DaemonConfig::builder()
                .brand(Brand::Upstream)
                .arguments(["--port", "8873"])
                .signal_flags(flags)
                .build();

            assert_eq!(config.brand(), Brand::Upstream);
            assert_eq!(config.arguments().len(), 2);
            assert!(config.take_signal_flags().is_some());
        }
    }
}
