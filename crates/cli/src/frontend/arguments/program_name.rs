use std::ffi::OsStr;

use oc_rsync_core::branding::{self, Brand};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProgramName {
    Rsync,
    OcRsync,
}

impl ProgramName {
    #[inline]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Rsync => Brand::Upstream.client_program_name(),
            Self::OcRsync => Brand::Oc.client_program_name(),
        }
    }

    #[inline]
    pub(crate) const fn brand(self) -> Brand {
        match self {
            Self::Rsync => Brand::Upstream,
            Self::OcRsync => Brand::Oc,
        }
    }
}

pub(crate) fn detect_program_name(program: Option<&OsStr>) -> ProgramName {
    match branding::detect_brand(program) {
        Brand::Oc => ProgramName::OcRsync,
        Brand::Upstream => ProgramName::Rsync,
    }
}
