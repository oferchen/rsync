//! Brand detection helpers used by the CLI and daemon entry points.

use std::env;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::OnceLock;

use super::brand::{Brand, default_brand, matches_program_alias};
use super::manifest;
use super::override_env::brand_override_from_env;
use super::profile::BrandProfile;
use crate::workspace;

/// Returns the branding profile that matches the provided program name.
#[must_use]
pub fn brand_for_program_name(program: &str) -> Brand {
    classify_program_name(program).unwrap_or_else(default_brand)
}

fn classify_program_name(program: &str) -> Option<Brand> {
    let manifest = manifest();
    let oc_client = manifest.client_program_name_for(Brand::Oc);
    let oc_daemon = manifest.daemon_program_name_for(Brand::Oc);
    let oc_wrapper = workspace::metadata().daemon_wrapper_program_name();
    if matches_program_alias(program, oc_client)
        || matches_program_alias(program, oc_daemon)
        || matches_program_alias(program, oc_wrapper)
    {
        return Some(Brand::Oc);
    }

    let upstream_client = manifest.client_program_name_for(Brand::Upstream);
    let upstream_daemon = manifest.daemon_program_name_for(Brand::Upstream);
    if matches_program_alias(program, upstream_client)
        || matches_program_alias(program, upstream_daemon)
    {
        return Some(Brand::Upstream);
    }

    None
}

fn brand_for_program_path(path: &Path) -> Option<Brand> {
    if let Some(brand) = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(classify_program_name)
    {
        return Some(brand);
    }

    path.file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(classify_program_name)
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
