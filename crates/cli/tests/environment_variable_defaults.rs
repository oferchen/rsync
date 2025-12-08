//! Tests for environment variable influence on argument defaults.
//!
//! Validates that environment variables correctly set default values for
//! options when not explicitly specified on the command line.

use cli::test_utils::parse_args;
use std::env;

// Helper to ensure environment cleanup even if tests panic
struct EnvGuard {
    key: &'static str,
    old_value: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let old_value = env::var_os(key);
        // SAFETY: Test environment, no other threads accessing this variable
        unsafe {
            env::set_var(key, value);
        }
        Self { key, old_value }
    }

    fn remove(key: &'static str) -> Self {
        let old_value = env::var_os(key);
        // SAFETY: Test environment, no other threads accessing this variable
        unsafe {
            env::remove_var(key);
        }
        Self { key, old_value }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: Test environment cleanup, no other threads accessing this variable
        unsafe {
            match &self.old_value {
                Some(value) => env::set_var(self.key, value),
                None => env::remove_var(self.key),
            }
        }
    }
}

// ============================================================================
// RSYNC_PROTECT_ARGS Environment Variable
// ============================================================================

#[test]
fn test_rsync_protect_args_env_enables_protect_args() {
    let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "1");
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.protect_args,
        Some(true),
        "RSYNC_PROTECT_ARGS should enable protect_args by default"
    );
}

#[test]
fn test_rsync_protect_args_empty_enables_protect_args() {
    let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "");
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.protect_args,
        Some(true),
        "RSYNC_PROTECT_ARGS='' should enable protect_args"
    );
}

#[test]
fn test_rsync_protect_args_zero_disables_protect_args() {
    let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "0");
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.protect_args,
        Some(false),
        "RSYNC_PROTECT_ARGS=0 should disable protect_args"
    );
}

#[test]
fn test_protect_args_flag_overrides_env() {
    let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "0");
    let args = parse_args(["oc-rsync", "--protect-args", "src", "dest"]).unwrap();
    assert_eq!(
        args.protect_args,
        Some(true),
        "--protect-args should override RSYNC_PROTECT_ARGS env"
    );
}

#[test]
fn test_no_protect_args_flag_overrides_env() {
    let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "1");
    let args = parse_args(["oc-rsync", "--no-protect-args", "src", "dest"]).unwrap();
    assert_eq!(
        args.protect_args,
        Some(false),
        "--no-protect-args should override RSYNC_PROTECT_ARGS env"
    );
}

#[test]
fn test_no_rsync_protect_args_env() {
    let _guard = EnvGuard::remove("RSYNC_PROTECT_ARGS");
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    // Without environment variable and without explicit flag, protect_args defaults to None
    assert!(
        args.protect_args.is_none() || args.protect_args == Some(true),
        "Without RSYNC_PROTECT_ARGS, protect_args should use default behavior"
    );
}

// ============================================================================
// RSYNC_RSH Environment Variable
// ============================================================================

#[test]
fn test_rsync_rsh_env_sets_remote_shell() {
    let _guard = EnvGuard::set("RSYNC_RSH", "/usr/bin/ssh");
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.remote_shell,
        Some("/usr/bin/ssh".into()),
        "RSYNC_RSH should set remote_shell default"
    );
}

#[test]
fn test_rsync_rsh_env_with_options() {
    let _guard = EnvGuard::set("RSYNC_RSH", "ssh -p 2222");
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.remote_shell,
        Some("ssh -p 2222".into()),
        "RSYNC_RSH should preserve shell options"
    );
}

#[test]
fn test_rsync_rsh_empty_ignored() {
    let _guard = EnvGuard::set("RSYNC_RSH", "");
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.remote_shell, None,
        "Empty RSYNC_RSH should be ignored"
    );
}

#[test]
fn test_rsh_flag_overrides_rsync_rsh_env() {
    let _guard = EnvGuard::set("RSYNC_RSH", "/usr/bin/ssh");
    let args = parse_args(["oc-rsync", "-e", "rsh", "src", "dest"]).unwrap();
    assert_eq!(
        args.remote_shell,
        Some("rsh".into()),
        "-e flag should override RSYNC_RSH env"
    );
}

#[test]
fn test_no_rsync_rsh_env() {
    let _guard = EnvGuard::remove("RSYNC_RSH");
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.remote_shell, None,
        "Without RSYNC_RSH, remote_shell should be None"
    );
}

// ============================================================================
// RSYNC_PARTIAL_DIR Environment Variable
// ============================================================================

#[test]
fn test_rsync_partial_dir_env_sets_partial_dir() {
    let _guard = EnvGuard::set("RSYNC_PARTIAL_DIR", "/tmp/.rsync-partial");
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.partial_dir,
        Some("/tmp/.rsync-partial".into()),
        "RSYNC_PARTIAL_DIR should set partial_dir default"
    );
}

#[test]
fn test_rsync_partial_dir_env_enables_partial() {
    let _guard = EnvGuard::set("RSYNC_PARTIAL_DIR", "/tmp/.rsync-partial");
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert!(
        args.partial,
        "RSYNC_PARTIAL_DIR should implicitly enable partial mode"
    );
}

#[test]
fn test_rsync_partial_dir_empty_ignored() {
    let _guard = EnvGuard::set("RSYNC_PARTIAL_DIR", "");
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.partial_dir, None,
        "Empty RSYNC_PARTIAL_DIR should be ignored"
    );
}

#[test]
fn test_partial_dir_flag_overrides_rsync_partial_dir_env() {
    let _guard = EnvGuard::set("RSYNC_PARTIAL_DIR", "/tmp/.rsync-partial");
    let args = parse_args(["oc-rsync", "--partial-dir=/var/tmp", "src", "dest"]).unwrap();
    assert_eq!(
        args.partial_dir,
        Some("/var/tmp".into()),
        "--partial-dir flag should override RSYNC_PARTIAL_DIR env"
    );
}

#[test]
fn test_no_partial_clears_rsync_partial_dir_env() {
    let _guard = EnvGuard::set("RSYNC_PARTIAL_DIR", "/tmp/.rsync-partial");
    let args = parse_args(["oc-rsync", "--no-partial", "src", "dest"]).unwrap();
    assert_eq!(
        args.partial_dir, None,
        "--no-partial should clear partial_dir from RSYNC_PARTIAL_DIR env"
    );
    assert!(!args.partial, "--no-partial should disable partial mode");
}

#[test]
fn test_no_rsync_partial_dir_env() {
    let _guard = EnvGuard::remove("RSYNC_PARTIAL_DIR");
    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();
    assert_eq!(
        args.partial_dir, None,
        "Without RSYNC_PARTIAL_DIR, partial_dir should be None"
    );
}

// ============================================================================
// Multiple Environment Variables Interaction
// ============================================================================

#[test]
fn test_multiple_env_vars_together() {
    let _guard1 = EnvGuard::set("RSYNC_PROTECT_ARGS", "1");
    let _guard2 = EnvGuard::set("RSYNC_RSH", "ssh");
    let _guard3 = EnvGuard::set("RSYNC_PARTIAL_DIR", "/tmp");

    let args = parse_args(["oc-rsync", "src", "dest"]).unwrap();

    assert_eq!(args.protect_args, Some(true));
    assert_eq!(args.remote_shell, Some("ssh".into()));
    assert_eq!(args.partial_dir, Some("/tmp".into()));
    assert!(args.partial);
}

#[test]
fn test_flags_override_all_env_vars() {
    let _guard1 = EnvGuard::set("RSYNC_PROTECT_ARGS", "1");
    let _guard2 = EnvGuard::set("RSYNC_RSH", "ssh");
    let _guard3 = EnvGuard::set("RSYNC_PARTIAL_DIR", "/tmp");

    let args = parse_args([
        "oc-rsync",
        "--no-protect-args",
        "-e", "rsh",
        "--partial-dir=/var",
        "src",
        "dest",
    ])
    .unwrap();

    assert_eq!(args.protect_args, Some(false), "Flag should override RSYNC_PROTECT_ARGS");
    assert_eq!(args.remote_shell, Some("rsh".into()), "Flag should override RSYNC_RSH");
    assert_eq!(args.partial_dir, Some("/var".into()), "Flag should override RSYNC_PARTIAL_DIR");
}
