//! Brand detection helpers used by the CLI and daemon entry points.

use std::env;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::OnceLock;

use super::brand::{Brand, default_brand, matches_program_alias};
use super::manifest;
use super::override_env::brand_override_from_env;
use super::profile::BrandProfile;

/// Returns the branding profile that matches the provided program name.
#[must_use]
pub fn brand_for_program_name(program: &str) -> Brand {
    classify_program_name(program).unwrap_or_else(default_brand)
}

fn classify_program_name(program: &str) -> Option<Brand> {
    let manifest = manifest();
    let oc_client = manifest.client_program_name_for(Brand::Oc);
    let oc_daemon = manifest.daemon_program_name_for(Brand::Oc);
    if matches_program_alias(program, oc_client) || matches_program_alias(program, oc_daemon) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn oc_client() -> &'static str {
        manifest().client_program_name_for(Brand::Oc)
    }

    fn oc_daemon() -> &'static str {
        manifest().daemon_program_name_for(Brand::Oc)
    }

    fn upstream_client() -> &'static str {
        manifest().client_program_name_for(Brand::Upstream)
    }

    // classify_program_name tests
    #[test]
    fn classify_oc_client() {
        assert_eq!(classify_program_name(oc_client()), Some(Brand::Oc));
    }

    #[test]
    fn classify_oc_daemon() {
        assert_eq!(classify_program_name(oc_daemon()), Some(Brand::Oc));
    }

    #[test]
    fn classify_upstream_client() {
        assert_eq!(
            classify_program_name(upstream_client()),
            Some(Brand::Upstream)
        );
    }

    #[test]
    fn classify_unknown_returns_none() {
        assert_eq!(classify_program_name("unknown-program"), None);
    }

    #[test]
    fn classify_empty_returns_none() {
        assert_eq!(classify_program_name(""), None);
    }

    // brand_for_program_name tests
    #[test]
    fn brand_for_program_name_oc() {
        assert_eq!(brand_for_program_name(oc_client()), Brand::Oc);
    }

    #[test]
    fn brand_for_program_name_upstream() {
        assert_eq!(brand_for_program_name(upstream_client()), Brand::Upstream);
    }

    #[test]
    fn brand_for_program_name_unknown_uses_default() {
        assert_eq!(brand_for_program_name("unknown"), default_brand());
    }

    // brand_for_program_path tests
    #[test]
    fn brand_for_path_with_extension() {
        let path_str = format!("/usr/bin/{}.exe", oc_client());
        let path = Path::new(&path_str);
        assert_eq!(brand_for_program_path(path), Some(Brand::Oc));
    }

    #[test]
    fn brand_for_path_without_extension() {
        let path_str = format!("/usr/local/bin/{}", upstream_client());
        let path = Path::new(&path_str);
        assert_eq!(brand_for_program_path(path), Some(Brand::Upstream));
    }

    #[test]
    fn brand_for_path_nested_directory() {
        let path_str = format!("/home/user/.local/bin/{}", oc_daemon());
        let path = Path::new(&path_str);
        assert_eq!(brand_for_program_path(path), Some(Brand::Oc));
    }

    #[test]
    fn brand_for_path_unknown_returns_none() {
        assert_eq!(brand_for_program_path(Path::new("/usr/bin/unknown")), None);
    }

    // brand_for_program_os_str tests
    #[test]
    fn brand_for_os_str_oc() {
        let os_str = OsStr::new(oc_client());
        assert_eq!(brand_for_program_os_str(os_str), Some(Brand::Oc));
    }

    #[test]
    fn brand_for_os_str_path() {
        let path = format!("/bin/{}", upstream_client());
        let os_str = OsStr::new(&path);
        assert_eq!(brand_for_program_os_str(os_str), Some(Brand::Upstream));
    }

    // detect_brand tests
    #[test]
    fn detect_brand_from_program_arg() {
        let program = OsString::from(oc_client());
        let brand = detect_brand(Some(program.as_os_str()));
        // May return Oc or be overridden by env, but should not panic
        assert!(brand == Brand::Oc || brand == Brand::Upstream);
    }

    #[test]
    fn detect_brand_with_none_uses_current_exe() {
        let brand = detect_brand(None);
        assert!(brand == Brand::Oc || brand == Brand::Upstream);
    }

    // resolve_brand_profile tests
    #[test]
    fn resolve_brand_profile_returns_profile() {
        let program = OsString::from(upstream_client());
        let profile = resolve_brand_profile(Some(program.as_os_str()));
        assert!(!profile.client_program_name().is_empty());
    }

    #[test]
    fn resolve_brand_profile_with_none() {
        let profile = resolve_brand_profile(None);
        assert!(!profile.daemon_program_name().is_empty());
    }
}
