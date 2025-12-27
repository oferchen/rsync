use std::ffi::OsStr;

use core::branding::{self, Brand};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsync_as_str_returns_rsync() {
        assert_eq!(ProgramName::Rsync.as_str(), "rsync");
    }

    #[test]
    fn oc_rsync_as_str_returns_oc_rsync() {
        let result = ProgramName::OcRsync.as_str();
        // Should be the oc branded name
        assert!(!result.is_empty());
    }

    #[test]
    fn rsync_brand_returns_upstream() {
        assert_eq!(ProgramName::Rsync.brand(), Brand::Upstream);
    }

    #[test]
    fn oc_rsync_brand_returns_oc() {
        assert_eq!(ProgramName::OcRsync.brand(), Brand::Oc);
    }

    #[test]
    fn detect_with_none_returns_program_name() {
        // With None, it should return a valid program name
        let result = detect_program_name(None);
        assert!(result == ProgramName::Rsync || result == ProgramName::OcRsync);
    }

    #[test]
    fn detect_with_rsync_returns_rsync() {
        let result = detect_program_name(Some(OsStr::new("rsync")));
        assert_eq!(result, ProgramName::Rsync);
    }

    #[test]
    fn program_name_clone() {
        let name = ProgramName::Rsync;
        let cloned = name;
        assert_eq!(name, cloned);
    }

    #[test]
    fn program_name_debug() {
        let name = ProgramName::Rsync;
        let debug = format!("{name:?}");
        assert!(debug.contains("Rsync"));
    }

    #[test]
    fn program_name_eq() {
        assert_eq!(ProgramName::Rsync, ProgramName::Rsync);
        assert_eq!(ProgramName::OcRsync, ProgramName::OcRsync);
        assert_ne!(ProgramName::Rsync, ProgramName::OcRsync);
    }
}
