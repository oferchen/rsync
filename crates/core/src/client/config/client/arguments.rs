use super::*;

impl ClientConfig {
    /// Returns the raw transfer arguments provided by the caller.
    #[must_use]
    pub fn transfer_args(&self) -> &[OsString] {
        &self.transfer_args
    }

    /// Returns the ordered reference directories supplied via `--compare-dest`,
    /// `--copy-dest`, or `--link-dest`.
    #[must_use]
    #[doc(alias = "--compare-dest")]
    #[doc(alias = "--copy-dest")]
    #[doc(alias = "--link-dest")]
    pub fn reference_directories(&self) -> &[ReferenceDirectory] {
        &self.reference_directories
    }

    /// Reports whether transfers should be listed without mutating the destination.
    #[must_use]
    #[doc(alias = "--list-only")]
    pub const fn list_only(&self) -> bool {
        self.list_only
    }

    /// Reports whether a transfer was explicitly requested.
    #[must_use]
    pub fn has_transfer_request(&self) -> bool {
        !self.transfer_args.is_empty()
    }
}
