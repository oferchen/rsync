//! Helpers for identifying which branded frontend binary was invoked.

use std::ffi::OsStr;

use rsync_core::branding::{self, Brand};

/// Represents the supported program name variants recognised by the frontend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProgramName {
    /// The upstream-compatible `rsync` invocation.
    Rsync,
    /// The branded `oc-rsync` invocation.
    OcRsync,
}

impl ProgramName {
    /// Returns the canonical binary name associated with the [`ProgramName`].
    #[inline]
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Rsync => Brand::Upstream.client_program_name(),
            Self::OcRsync => Brand::Oc.client_program_name(),
        }
    }

    /// Returns the [`Brand`] that matches this [`ProgramName`].
    #[inline]
    #[must_use]
    pub(crate) const fn brand(self) -> Brand {
        match self {
            Self::Rsync => Brand::Upstream,
            Self::OcRsync => Brand::Oc,
        }
    }
}

/// Detects which program name variant is in use based on the provided string.
#[must_use]
pub(crate) fn detect_program_name(program: Option<&OsStr>) -> ProgramName {
    match branding::detect_brand(program) {
        Brand::Oc => ProgramName::OcRsync,
        Brand::Upstream => ProgramName::Rsync,
    }
}
