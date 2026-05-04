//! Setter methods for staging, backup, link-dest, and reference directory options.

use std::ffi::OsString;
use std::path::PathBuf;

use super::LocalCopyOptionsBuilder;
use crate::local_copy::options::types::{LinkDestEntry, ReferenceDirectory};

impl LocalCopyOptionsBuilder {
    /// Enables partial file handling.
    #[must_use]
    pub fn partial(mut self, enabled: bool) -> Self {
        self.partial = enabled;
        self
    }

    /// Sets the partial directory.
    #[must_use]
    pub fn partial_dir<P: Into<PathBuf>>(mut self, dir: Option<P>) -> Self {
        self.partial_dir = dir.map(Into::into);
        if self.partial_dir.is_some() {
            self.partial = true;
        }
        self
    }

    /// Sets the temp directory.
    #[must_use]
    pub fn temp_dir<P: Into<PathBuf>>(mut self, dir: Option<P>) -> Self {
        self.temp_dir = dir.map(Into::into);
        self
    }

    /// Enables delay-updates mode.
    ///
    /// When enabled and no explicit `partial_dir` has been set, the staging
    /// directory defaults to `.~tmp~` matching upstream rsync behaviour.
    #[must_use]
    pub fn delay_updates(mut self, enabled: bool) -> Self {
        self.delay_updates = enabled;
        if enabled {
            self.partial = true;
            if self.partial_dir.is_none() {
                self.partial_dir = Some(PathBuf::from(
                    crate::local_copy::options::staging::DELAY_UPDATES_PARTIAL_DIR,
                ));
            }
        }
        self
    }

    /// Enables inplace mode.
    #[must_use]
    pub fn inplace(mut self, enabled: bool) -> Self {
        self.inplace = enabled;
        self
    }

    /// Enables append mode.
    #[must_use]
    pub fn append(mut self, enabled: bool) -> Self {
        self.append = enabled;
        if !enabled {
            self.append_verify = false;
        }
        self
    }

    /// Enables append-verify mode.
    #[must_use]
    pub fn append_verify(mut self, enabled: bool) -> Self {
        if enabled {
            self.append = true;
            self.append_verify = true;
        } else {
            self.append_verify = false;
        }
        self
    }

    /// Enables event collection.
    #[must_use]
    pub fn collect_events(mut self, enabled: bool) -> Self {
        self.collect_events = enabled;
        self
    }

    /// Enables hard link preservation.
    #[must_use]
    pub fn hard_links(mut self, enabled: bool) -> Self {
        self.preserve_hard_links = enabled;
        self
    }

    /// Adds a link-dest directory.
    #[must_use]
    pub fn link_dest<P: Into<PathBuf>>(mut self, path: P) -> Self {
        let path = path.into();
        if !path.as_os_str().is_empty() {
            self.link_dests.push(LinkDestEntry::new(path));
        }
        self
    }

    /// Extends link-dest directories.
    #[must_use]
    pub fn link_dests<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        for path in paths {
            let path = path.into();
            if !path.as_os_str().is_empty() {
                self.link_dests.push(LinkDestEntry::new(path));
            }
        }
        self
    }

    /// Adds a reference directory.
    #[must_use]
    pub fn reference_directory(mut self, reference: ReferenceDirectory) -> Self {
        self.reference_directories.push(reference);
        self
    }

    /// Extends reference directories.
    #[must_use]
    pub fn reference_directories<I>(mut self, references: I) -> Self
    where
        I: IntoIterator<Item = ReferenceDirectory>,
    {
        self.reference_directories.extend(references);
        self
    }

    /// Enables backup mode.
    #[must_use]
    pub fn backup(mut self, enabled: bool) -> Self {
        self.backup = enabled;
        self
    }

    /// Sets the backup directory.
    #[must_use]
    pub fn backup_dir<P: Into<PathBuf>>(mut self, dir: Option<P>) -> Self {
        self.backup_dir = dir.map(Into::into);
        if self.backup_dir.is_some() {
            self.backup = true;
        }
        self
    }

    /// Sets the backup suffix.
    #[must_use]
    pub fn backup_suffix<S: Into<OsString>>(mut self, suffix: Option<S>) -> Self {
        match suffix {
            Some(s) => {
                self.backup_suffix = s.into();
                self.backup = true;
            }
            None => {
                self.backup_suffix = OsString::from("~");
            }
        }
        self
    }
}
