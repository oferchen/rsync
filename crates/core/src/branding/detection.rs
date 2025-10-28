//! Brand detection helpers used by the CLI and daemon entry points.

use std::env;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::OnceLock;

use super::brand::{Brand, default_brand, matches_program_alias};
use super::constants::{
    OC_CLIENT_PROGRAM_NAME, OC_DAEMON_PROGRAM_NAME, UPSTREAM_CLIENT_PROGRAM_NAME,
    UPSTREAM_DAEMON_PROGRAM_NAME,
};
use super::override_env::brand_override_from_env;
use super::profile::BrandProfile;

/// Returns the branding profile that matches the provided program name.
#[must_use]
pub fn brand_for_program_name(program: &str) -> Brand {
    if matches_program_alias(program, OC_CLIENT_PROGRAM_NAME)
        || matches_program_alias(program, OC_DAEMON_PROGRAM_NAME)
    {
        Brand::Oc
    } else if matches_program_alias(program, UPSTREAM_CLIENT_PROGRAM_NAME)
        || matches_program_alias(program, UPSTREAM_DAEMON_PROGRAM_NAME)
    {
        Brand::Upstream
    } else {
        default_brand()
    }
}

fn brand_for_program_path(path: &Path) -> Option<Brand> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(brand_for_program_name)
}

fn brand_for_program_os_str(program: &OsStr) -> Option<Brand> {
    brand_for_program_path(Path::new(program))
}

fn brand_from_current_executable() -> Brand {
    static CURRENT_EXE_BRAND: OnceLock<Brand> = OnceLock::new();

    *CURRENT_EXE_BRAND.get_or_init(|| {
        env::current_exe()
            .ok()
            .and_then(|path| brand_for_program_path(&path))
            .unwrap_or_else(default_brand)
    })
}

/// Detects the [`Brand`] associated with an invocation argument.
#[must_use]
pub fn detect_brand(program: Option<&OsStr>) -> Brand {
    if let Some(brand) = brand_override_from_env() {
        return brand;
    }

    if let Some(brand) = program.and_then(brand_for_program_os_str) {
        return brand;
    }

    brand_from_current_executable()
}

/// Returns the [`BrandProfile`] resolved from the provided program identifier.
#[must_use]
pub fn resolve_brand_profile(program: Option<&OsStr>) -> BrandProfile {
    detect_brand(program).profile()
}
