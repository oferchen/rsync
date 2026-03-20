//! Daemon configuration file parsing for rsyncd.conf.
//!
//! This module provides a standalone API for parsing rsync daemon configuration
//! files matching upstream rsync 3.4.1 format. The configuration consists of
//! global parameters followed by per-module sections.
//!
//! # Format
//!
//! ```ini
//! # Global parameters
//! port = 873
//! motd file = /etc/rsyncd.motd
//! log file = /var/log/rsyncd.log
//!
//! [module_name]
//! path = /data/module_name
//! comment = Public files
//! read only = yes
//! ```
//!
//! # Example
//!
//! ```no_run
//! use daemon::rsyncd_config::{RsyncdConfig, ConfigError};
//! use std::path::Path;
//!
//! # fn example() -> Result<(), ConfigError> {
//! let config = RsyncdConfig::from_file(Path::new("/etc/rsyncd.conf"))?;
//!
//! // Access global settings
//! println!("Port: {}", config.global().port());
//!
//! // Find a module
//! if let Some(module) = config.get_module("mymodule") {
//!     println!("Module path: {}", module.path().display());
//! }
//! # Ok(())
//! # }
//! ```

mod parser;
mod sections;
mod validation;

#[cfg(test)]
mod tests;

pub use sections::{GlobalConfig, ModuleConfig};
pub use validation::ConfigError;

use std::fs;
use std::path::Path;

use parser::Parser;

/// Parsed representation of a complete `rsyncd.conf` file.
///
/// Combines the global parameter section with zero or more per-module
/// sections. Obtain via [`RsyncdConfig::from_file`] or [`RsyncdConfig::parse`].
#[derive(Clone, Debug, PartialEq)]
pub struct RsyncdConfig {
    pub(crate) global: GlobalConfig,
    pub(crate) modules: Vec<ModuleConfig>,
}

impl RsyncdConfig {
    /// Parses a configuration file from the given path.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or contains invalid syntax.
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let contents = fs::read_to_string(path).map_err(|e| ConfigError::io_error(path, e))?;
        Self::parse(&contents, path)
    }

    /// Parses configuration from a string.
    ///
    /// # Errors
    ///
    /// Returns an error if the input contains invalid syntax.
    pub fn parse(input: &str, path: &Path) -> Result<Self, ConfigError> {
        let mut parser = Parser::new(input, path);
        parser.parse()
    }

    /// Returns the global configuration.
    pub fn global(&self) -> &GlobalConfig {
        &self.global
    }

    /// Returns all module configurations.
    pub fn modules(&self) -> &[ModuleConfig] {
        &self.modules
    }

    /// Finds a module by name.
    pub fn get_module(&self, name: &str) -> Option<&ModuleConfig> {
        self.modules.iter().find(|m| m.name == name)
    }
}
