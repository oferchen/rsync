#![deny(unsafe_code)]

use std::ffi::{OsStr, OsString};

pub const CLIENT_FALLBACK_ENV: &str = "OC_RSYNC_FALLBACK";
pub const DAEMON_FALLBACK_ENV: &str = "OC_RSYNC_DAEMON_FALLBACK";
pub const DAEMON_AUTO_DELEGATE_ENV: &str = "OC_RSYNC_DAEMON_AUTO_DELEGATE";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FallbackOverride {
    Disabled,
}

impl FallbackOverride {
    #[must_use]
    pub fn resolve_or_default(self, _default: &OsStr) -> Option<OsString> {
        match self {
            FallbackOverride::Disabled => None,
        }
    }
}

#[must_use]
pub fn interpret_override_value(_value: &OsStr) -> FallbackOverride {
    FallbackOverride::Disabled
}

#[must_use]
pub fn fallback_override(_name: &str) -> Option<FallbackOverride> {
    Some(FallbackOverride::Disabled)
}

#[must_use]
pub fn fallback_binary_candidates() -> Vec<OsString> {
    Vec::new()
}

#[must_use]
pub fn fallback_binary_available(_binary: &OsStr) -> bool {
    false
}

#[must_use]
pub fn fallback_binary_path(_binary: &OsStr) -> Option<OsString> {
    None
}

#[must_use]
pub fn fallback_binary_is_self(_binary: &OsStr) -> bool {
    false
}

#[must_use]
pub fn describe_missing_fallback_binary(_binary: &OsStr) -> String {
    "fallback delegation is disabled".to_string()
}
