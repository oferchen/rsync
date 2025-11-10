use super::brand::matches_program_alias;
use super::*;
use ::core::str::FromStr;
use std::env;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

fn env_lock() -> &'static Mutex<()> {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_LOCK.get_or_init(|| Mutex::new(()))
}

fn acquire_env_lock() -> MutexGuard<'static, ()> {
    env_lock()
        .lock()
        .expect("environment lock poisoned during test")
}

#[allow(unsafe_code)]
fn set_env_var(key: &'static str, value: impl AsRef<OsStr>) {
    unsafe {
        env::set_var(key, value);
    }
    super::override_env::reset_brand_override_cache();
}

#[allow(unsafe_code)]
fn remove_env_var(key: &'static str) {
    unsafe {
        env::remove_var(key);
    }
    super::override_env::reset_brand_override_cache();
}

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn set<V>(key: &'static str, value: V) -> Self
    where
        V: AsRef<OsStr>,
    {
        let lock = acquire_env_lock();
        let previous = env::var_os(key);
        set_env_var(key, value);
        Self {
            key,
            previous,
            _lock: lock,
        }
    }

    fn remove(key: &'static str) -> Self {
        let lock = acquire_env_lock();
        let previous = env::var_os(key);
        remove_env_var(key);
        Self {
            key,
            previous,
            _lock: lock,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            set_env_var(self.key, previous);
        } else {
            remove_env_var(self.key);
        }
    }
}

#[test]
fn program_names_are_consistent() {
    assert_eq!(client_program_name(), upstream_client_program_name());
    assert_eq!(daemon_program_name(), upstream_daemon_program_name());
    assert_eq!(oc_client_program_name(), OC_CLIENT_PROGRAM_NAME);
    assert_eq!(oc_daemon_program_name(), OC_DAEMON_PROGRAM_NAME);
}

#[test]
fn oc_paths_match_expected_locations() {
    assert_eq!(oc_daemon_config_dir(), Path::new(OC_DAEMON_CONFIG_DIR));
    assert_eq!(oc_daemon_config_path(), Path::new(OC_DAEMON_CONFIG_PATH));
    assert_eq!(oc_daemon_secrets_path(), Path::new(OC_DAEMON_SECRETS_PATH));
    assert_eq!(
        legacy_daemon_config_path(),
        Path::new(LEGACY_DAEMON_CONFIG_PATH)
    );
    assert_eq!(
        legacy_daemon_config_dir(),
        Path::new(LEGACY_DAEMON_CONFIG_DIR)
    );
    assert_eq!(
        legacy_daemon_secrets_path(),
        Path::new(LEGACY_DAEMON_SECRETS_PATH)
    );
}

#[test]
fn profile_helpers_align_with_functions() {
    assert_eq!(
        upstream_profile().client_program_name(),
        upstream_client_program_name()
    );
    assert_eq!(
        upstream_profile().daemon_program_name(),
        upstream_daemon_program_name()
    );
    assert_eq!(oc_profile().client_program_name(), oc_client_program_name());
    assert_eq!(oc_profile().daemon_program_name(), oc_daemon_program_name());
}

#[test]
fn brand_profiles_match_expected_programs() {
    let upstream = Brand::Upstream.profile();
    assert_eq!(upstream.client_program_name(), UPSTREAM_CLIENT_PROGRAM_NAME);
    assert_eq!(upstream.daemon_program_name(), UPSTREAM_DAEMON_PROGRAM_NAME);

    let oc = Brand::Oc.profile();
    assert_eq!(oc.client_program_name(), OC_CLIENT_PROGRAM_NAME);
    assert_eq!(oc.daemon_program_name(), OC_DAEMON_PROGRAM_NAME);
    assert_eq!(oc.daemon_config_dir(), Path::new(OC_DAEMON_CONFIG_DIR));
}

#[test]
fn oc_brand_uses_single_binary() {
    let oc = Brand::Oc.profile();
    assert_eq!(
        oc.client_program_name(),
        oc.daemon_program_name(),
        "the oc brand must resolve to a single binary name",
    );
    assert_eq!(
        oc.client_program_name(),
        OC_CLIENT_PROGRAM_NAME,
        "oc client program name must match metadata",
    );
}

#[test]
fn brand_profile_alias_detection_handles_client_and_daemon_programs() {
    let upstream = Brand::Upstream.profile();
    assert!(upstream.matches_daemon_program_alias(OsStr::new("rsync")));
    assert!(upstream.matches_daemon_program_alias(OsStr::new("/usr/bin/RSYNC.EXE")));
    assert!(upstream.matches_daemon_program_alias(OsStr::new("rsync")));
    assert!(upstream.matches_client_program_alias(OsStr::new("rsync")));
    assert!(upstream.matches_client_program_alias(OsStr::new("/usr/local/bin/rsync-3.4.1")));

    let oc = Brand::Oc.profile();
    assert!(oc.matches_client_program_alias(OsStr::new("oc-rsync")));
    assert!(oc.matches_daemon_program_alias(OsStr::new("oc-rsync")));
}

#[test]
fn detect_brand_matches_invocation_argument() {
    let _guard = EnvGuard::remove(BRAND_OVERRIDE_ENV);
    assert_eq!(detect_brand(Some(OsStr::new("rsync"))), Brand::Upstream);
    assert_eq!(
        detect_brand(Some(OsStr::new("/usr/bin/oc-rsync"))),
        Brand::Oc
    );
    assert_eq!(
        detect_brand(Some(OsStr::new("/usr/bin/oc-rsync-3.4.1"))),
        Brand::Oc
    );
    assert_eq!(
        detect_brand(Some(OsStr::new("/usr/local/bin/rsync-3.4.1"))),
        Brand::Upstream
    );
}

#[test]
fn detect_brand_supports_windows_extensions() {
    let _guard = EnvGuard::remove(BRAND_OVERRIDE_ENV);
    assert_eq!(detect_brand(Some(OsStr::new("oc-rsync.exe"))), Brand::Oc);
    let upstream = detect_brand(Some(OsStr::new("RSYNC.EXE")));
    assert_eq!(upstream, Brand::Upstream);
    assert_eq!(
        detect_brand(Some(OsStr::new("/opt/tools/oc-rsync-3.4.1.EXE"))),
        Brand::Oc
    );
    let debug = detect_brand(Some(OsStr::new("C:/rsync/bin/rsync-debug.COM")));
    assert_eq!(debug, Brand::Upstream);
}

#[test]
fn detect_brand_recognises_debug_suffixes_without_digits() {
    let _guard = EnvGuard::remove(BRAND_OVERRIDE_ENV);

    assert_eq!(detect_brand(Some(OsStr::new("oc-rsync-debug"))), Brand::Oc);
    assert_eq!(
        detect_brand(Some(OsStr::new("rsync-debug"))),
        Brand::Upstream
    );
    assert_eq!(detect_brand(Some(OsStr::new("oc-rsync_dbg"))), Brand::Oc);
    assert_eq!(
        detect_brand(Some(OsStr::new("rsync.devel"))),
        Brand::Upstream
    );
}

#[test]
fn matches_program_alias_accepts_windows_extensions() {
    let alias = format!("{}", oc_client_program_name());
    assert!(matches_program_alias("rsync.exe", "rsync"));
    assert!(matches_program_alias("RSYNC.EXE", "rsync"));
    assert!(matches_program_alias("oc-rsync.EXE", "oc-rsync"));
    assert!(matches_program_alias("OC-RSYNC.EXE", &alias));
    assert!(!matches_program_alias("rsync.exe", &alias));
}

#[test]
fn program_alias_rejects_unrecognised_alpha_suffix() {
    assert!(!matches_program_alias("oc-rsync-malicious", "oc-rsync"));
    assert!(!matches_program_alias("rsync-preview", "rsync"));
}

#[test]
fn resolve_brand_profile_delegates_to_detect_brand() {
    let _guard = EnvGuard::remove(BRAND_OVERRIDE_ENV);

    let oc = resolve_brand_profile(Some(OsStr::new("oc-rsync")));
    assert_eq!(oc.client_program_name(), Brand::Oc.client_program_name());

    let upstream = resolve_brand_profile(Some(OsStr::new("rsync")));
    assert_eq!(
        upstream.daemon_program_name(),
        Brand::Upstream.daemon_program_name()
    );
}

#[test]
fn helper_accessors_forward_to_manifest() {
    let manifest = manifest();

    assert_eq!(rust_version(), manifest.rust_version());
    assert_eq!(upstream_version(), manifest.upstream_version());
    assert_eq!(protocol_version(), manifest.protocol_version());
    assert_eq!(source_url(), manifest.source_url());
    assert_eq!(source_line(), format!("Source: {}", manifest.source_url()));
    assert_eq!(build_revision(), manifest.build_revision());
    assert_eq!(build_toolchain(), manifest.build_toolchain());
    assert_eq!(oc_summary(), manifest.oc_summary());
    assert_eq!(upstream_summary(), manifest.upstream_summary());
}

#[test]
fn detect_brand_falls_back_to_current_executable() {
    let _guard = EnvGuard::remove(BRAND_OVERRIDE_ENV);
    let expected = env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(brand_for_program_name)
        })
        .unwrap_or_else(default_brand);
    assert_eq!(detect_brand(None), expected);
}

#[test]
fn config_search_orders_match_brand_expectations() {
    assert_eq!(
        Brand::Oc.config_path_candidate_strs(),
        [OC_DAEMON_CONFIG_PATH, LEGACY_DAEMON_CONFIG_PATH]
    );
    assert_eq!(
        Brand::Upstream.config_path_candidate_strs(),
        [LEGACY_DAEMON_CONFIG_PATH, OC_DAEMON_CONFIG_PATH]
    );

    let oc_paths = Brand::Oc.config_path_candidates();
    assert_eq!(oc_paths[0], Path::new(OC_DAEMON_CONFIG_PATH));
    assert_eq!(oc_paths[1], Path::new(LEGACY_DAEMON_CONFIG_PATH));
    let upstream_paths = Brand::Upstream.config_path_candidates();
    assert_eq!(upstream_paths[0], Path::new(LEGACY_DAEMON_CONFIG_PATH));
    assert_eq!(upstream_paths[1], Path::new(OC_DAEMON_CONFIG_PATH));
}

#[test]
fn config_directories_match_brand_profiles() {
    assert_eq!(
        Brand::Oc.daemon_config_dir(),
        Path::new(OC_DAEMON_CONFIG_DIR)
    );
    assert_eq!(Brand::Oc.daemon_config_dir_str(), OC_DAEMON_CONFIG_DIR);
    assert_eq!(
        Brand::Upstream.daemon_config_dir(),
        Path::new(LEGACY_DAEMON_CONFIG_DIR)
    );
    assert_eq!(
        Brand::Upstream.daemon_config_dir_str(),
        LEGACY_DAEMON_CONFIG_DIR
    );
}

#[test]
fn config_paths_match_brand_profiles() {
    assert_eq!(
        Brand::Oc.daemon_config_path(),
        Path::new(OC_DAEMON_CONFIG_PATH)
    );
    assert_eq!(Brand::Oc.daemon_config_path_str(), OC_DAEMON_CONFIG_PATH);
    assert_eq!(
        Brand::Upstream.daemon_config_path(),
        Path::new(LEGACY_DAEMON_CONFIG_PATH)
    );
    assert_eq!(
        Brand::Upstream.daemon_config_path_str(),
        LEGACY_DAEMON_CONFIG_PATH
    );
}

#[test]
fn secrets_search_orders_match_brand_expectations() {
    assert_eq!(
        Brand::Oc.secrets_path_candidate_strs(),
        [OC_DAEMON_SECRETS_PATH, LEGACY_DAEMON_SECRETS_PATH]
    );
    assert_eq!(
        Brand::Upstream.secrets_path_candidate_strs(),
        [LEGACY_DAEMON_SECRETS_PATH, OC_DAEMON_SECRETS_PATH]
    );

    let oc_paths = Brand::Oc.secrets_path_candidates();
    assert_eq!(oc_paths[0], Path::new(OC_DAEMON_SECRETS_PATH));
    assert_eq!(oc_paths[1], Path::new(LEGACY_DAEMON_SECRETS_PATH));
    let upstream_paths = Brand::Upstream.secrets_path_candidates();
    assert_eq!(upstream_paths[0], Path::new(LEGACY_DAEMON_SECRETS_PATH));
    assert_eq!(upstream_paths[1], Path::new(OC_DAEMON_SECRETS_PATH));

    assert_eq!(
        Brand::Oc.daemon_secrets_path(),
        Path::new(OC_DAEMON_SECRETS_PATH)
    );
    assert_eq!(Brand::Oc.daemon_secrets_path_str(), OC_DAEMON_SECRETS_PATH);
    assert_eq!(
        Brand::Upstream.daemon_secrets_path(),
        Path::new(LEGACY_DAEMON_SECRETS_PATH)
    );
    assert_eq!(
        Brand::Upstream.daemon_secrets_path_str(),
        LEGACY_DAEMON_SECRETS_PATH
    );
}

#[test]
fn detect_brand_respects_oc_override_environment_variable() {
    let _guard = EnvGuard::set(BRAND_OVERRIDE_ENV, OsStr::new("oc"));
    assert_eq!(detect_brand(Some(OsStr::new("rsync"))), Brand::Oc);
    assert_eq!(detect_brand(None), Brand::Oc);
}

#[test]
fn detect_brand_respects_upstream_override_environment_variable() {
    let _guard = EnvGuard::set(BRAND_OVERRIDE_ENV, OsStr::new("upstream"));
    assert_eq!(detect_brand(Some(OsStr::new("oc-rsync"))), Brand::Upstream);
    assert_eq!(detect_brand(None), Brand::Upstream);
}

#[test]
fn detect_brand_ignores_invalid_override_environment_variable() {
    let _guard = EnvGuard::set(BRAND_OVERRIDE_ENV, OsStr::new("invalid"));
    assert_eq!(detect_brand(Some(OsStr::new("oc-rsync"))), Brand::Oc);
}

#[test]
fn brand_from_str_accepts_aliases() {
    let daemon_alias = format!("{}", oc_client_program_name());
    let upper_daemon_alias = daemon_alias.to_ascii_uppercase();
    assert_eq!(Brand::from_str("oc").unwrap(), Brand::Oc);
    assert_eq!(Brand::from_str(&upper_daemon_alias).unwrap(), Brand::Oc);
    assert_eq!(Brand::from_str("oc_rsync").unwrap(), Brand::Oc);
    assert_eq!(Brand::from_str("OC.RSYNC").unwrap(), Brand::Oc);
    assert_eq!(Brand::from_str(" rsync-3.4.1 ").unwrap(), Brand::Upstream);
    assert_eq!(Brand::from_str("rsync_3.4.1").unwrap(), Brand::Upstream);
    assert_eq!(Brand::from_str("RSYNC.3.4.1").unwrap(), Brand::Upstream);
    assert_eq!(Brand::from_str("RSYNC").unwrap(), Brand::Upstream);
}

#[test]
fn brand_from_str_rejects_unknown_values() {
    assert!(Brand::from_str("").is_err());
    assert!(Brand::from_str("unknown").is_err());
    assert!(Brand::from_str("ocrsync").is_err());
}

#[test]
fn brand_label_matches_expected() {
    assert_eq!(Brand::Oc.label(), "oc");
    assert_eq!(Brand::Upstream.label(), "upstream");
}

#[test]
fn brand_display_renders_label() {
    assert_eq!(Brand::Oc.to_string(), "oc");
    assert_eq!(Brand::Upstream.to_string(), "upstream");
}
