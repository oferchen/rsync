use std::path::{Path, PathBuf};

use super::types::{LinkDestEntry, LocalCopyOptions, ReferenceDirectory};

impl LocalCopyOptions {
    /// Adds a directory that should be consulted when creating hard links for matching files.
    #[must_use]
    #[doc(alias = "--link-dest")]
    pub fn with_link_dest(mut self, path: PathBuf) -> Self {
        if !path.as_os_str().is_empty() {
            self.link_dests.push(LinkDestEntry::new(path));
        }
        self
    }

    /// Extends the link-destination list with additional directories.
    #[must_use]
    #[doc(alias = "--link-dest")]
    pub fn extend_link_dests<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        for path in paths.into_iter() {
            let path = path.into();
            if !path.as_os_str().is_empty() {
                self.link_dests.push(LinkDestEntry::new(path));
            }
        }
        self
    }

    /// Enables or disables hard-link preservation for identical inodes.
    #[must_use]
    #[doc(alias = "--hard-links")]
    pub const fn hard_links(mut self, preserve: bool) -> Self {
        self.preserve_hard_links = preserve;
        self
    }

    /// Returns `true` when hard-link preservation is enabled.
    #[must_use]
    pub const fn hard_links_enabled(&self) -> bool {
        self.preserve_hard_links
    }

    /// Appends a reference directory consulted for `--compare-dest`,
    /// `--copy-dest`, and `--link-dest` handling.
    #[must_use]
    pub fn push_reference_directory(mut self, reference: ReferenceDirectory) -> Self {
        self.reference_directories.push(reference);
        self
    }

    /// Extends the reference directory list with the provided entries.
    #[must_use]
    pub fn extend_reference_directories<I>(mut self, references: I) -> Self
    where
        I: IntoIterator<Item = ReferenceDirectory>,
    {
        self.reference_directories.extend(references);
        self
    }

    /// Returns the ordered list of reference directories consulted during copy execution.
    pub fn reference_directories(&self) -> &[ReferenceDirectory] {
        &self.reference_directories
    }

    /// Returns the configured link-destination entries.
    #[must_use]
    pub(crate) fn link_dest_entries(&self) -> &[LinkDestEntry] {
        &self.link_dests
    }
}

impl LinkDestEntry {
    pub(crate) fn new(path: PathBuf) -> Self {
        let is_relative = !path.is_absolute();
        Self { path, is_relative }
    }

    pub(crate) fn resolve(&self, destination_root: &Path, relative: &Path) -> PathBuf {
        let base = if self.is_relative {
            destination_root.join(&self.path)
        } else {
            self.path.clone()
        };
        base.join(relative)
    }
}
