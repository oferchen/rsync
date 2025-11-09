#![allow(unsafe_code)]

use super::*;
use crate::test_env::{EnvGuard, ENV_LOCK};
use rsync_core::branding::Brand;
use rsync_core::fallback::{CLIENT_FALLBACK_ENV, DAEMON_FALLBACK_ENV};
use std::ffi::{OsStr, OsString};

#[test]
fn parse_daemon_option_extracts_option_payload() {
    assert_eq!(parse_daemon_option("OPTION --list"), Some("--list"));
    assert_eq!(parse_daemon_option("option --max-verbosity"), Some("--max-verbosity"));
}

#[test]
fn parse_daemon_option_rejects_invalid_values() {
    assert!(parse_daemon_option("HELLO there").is_none());
    assert!(parse_daemon_option("OPTION   ").is_none());
}

#[test]
fn canonical_option_trims_prefix_and_normalises_case() {
    assert_eq!(canonical_option("--Delete"), "delete");
    assert_eq!(canonical_option(" -P --info"), "p");
    assert_eq!(canonical_option("   CHECKSUM=md5"), "checksum");
}

#[test]
fn configured_fallback_binary_defaults_to_upstream_branding() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _daemon_guard = EnvGuard::remove(DAEMON_FALLBACK_ENV);
    let _client_guard = EnvGuard::remove(CLIENT_FALLBACK_ENV);
    let expected = OsString::from(Brand::Upstream.client_program_name());
    assert_eq!(configured_fallback_binary(), Some(expected));
}

#[test]
fn configured_fallback_binary_prefers_daemon_override() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let override_path = OsString::from("/opt/oc-rsync");
    let _client_guard = EnvGuard::remove(CLIENT_FALLBACK_ENV);
    let _daemon_guard = EnvGuard::set(DAEMON_FALLBACK_ENV, &override_path);
    assert_eq!(configured_fallback_binary(), Some(override_path));
}

#[test]
fn configured_fallback_binary_allows_default_keyword() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");
    let _daemon_guard = EnvGuard::remove(DAEMON_FALLBACK_ENV);
    let _client_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("default"));
    let expected = OsString::from(Brand::Upstream.client_program_name());
    assert_eq!(configured_fallback_binary(), Some(expected));
}
