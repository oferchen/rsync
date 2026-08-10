//! Integration coverage for the SSC-3 `ssh_config` compression lookup.
//!
//! These tests drive `SshCommand::has_ssh_compression()` through the
//! `ssh-config-parse` code path by pointing `HOME` (and `USERPROFILE`
//! on Windows) at a temporary directory containing a synthetic
//! `~/.ssh/config`, or by passing a `-F <override>` SSH option. They
//! never touch the real user config.
//!
//! Tests share a process-global `ENV_LOCK` because they mutate
//! environment variables, which are not thread-safe.

#![cfg(feature = "ssh-config-parse")]

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::sync::Mutex;

use rsync_io::ssh::SshCommand;
use tempfile::TempDir;

/// Serialises every test in this binary; environment mutation is not
/// thread-safe in Rust 2024.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// RAII guard that scopes an environment-variable mutation to the
/// surrounding test. Mirrors the inline pattern used by
/// `crates/cli/src/frontend/arguments/env.rs`.
struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &OsStr) -> Self {
        let previous = env::var_os(key);
        // SAFETY: every caller holds `ENV_LOCK`, so no other thread can
        // observe a torn read or invoke a concurrent setenv. set_var is
        // unsafe in Rust 2024 only because of cross-thread races, which
        // the mutex prevents.
        #[allow(unsafe_code)]
        unsafe {
            env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: the surrounding test still holds `ENV_LOCK` when the
        // guard drops, so restoration cannot race a concurrent reader
        // or writer.
        #[allow(unsafe_code)]
        unsafe {
            if let Some(value) = self.previous.take() {
                env::set_var(self.key, value);
            } else {
                env::remove_var(self.key);
            }
        }
    }
}

/// Writes `body` to `dir/.ssh/config`.
fn write_ssh_config(dir: &TempDir, body: &str) {
    let ssh_dir = dir.path().join(".ssh");
    fs::create_dir_all(&ssh_dir).expect("create .ssh");
    fs::write(ssh_dir.join("config"), body).expect("write ssh_config");
}

/// Returns an `SshCommand` carrying no SSH options so the argv check
/// is always inconclusive and the ssh_config lookup is exercised.
fn plain_cmd() -> SshCommand {
    SshCommand::new("example.com")
}

#[test]
fn top_level_compression_yes_in_home_config() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().expect("tempdir");
    write_ssh_config(&home, "Compression yes\n");
    let _home_guard = EnvGuard::set("HOME", home.path().as_os_str());
    let _userprofile_guard = EnvGuard::set("USERPROFILE", home.path().as_os_str());

    assert!(plain_cmd().has_ssh_compression());
}

#[test]
fn host_star_block_compression_yes_in_home_config() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().expect("tempdir");
    write_ssh_config(&home, "Host *\n  Compression yes\n");
    let _home_guard = EnvGuard::set("HOME", home.path().as_os_str());
    let _userprofile_guard = EnvGuard::set("USERPROFILE", home.path().as_os_str());

    assert!(plain_cmd().has_ssh_compression());
}

#[test]
fn per_host_block_does_not_match_unrelated_target() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().expect("tempdir");
    // Per-host `Host foo` blocks now resolve via the SSC-5.b pattern
    // matcher. The target is `example.com` (from `plain_cmd`), which
    // does not match `foo.example.com`, so the block contributes
    // nothing.
    write_ssh_config(&home, "Host foo.example.com\n  Compression yes\n");
    let _home_guard = EnvGuard::set("HOME", home.path().as_os_str());
    let _userprofile_guard = EnvGuard::set("USERPROFILE", home.path().as_os_str());

    assert!(!plain_cmd().has_ssh_compression());
}

#[test]
fn per_host_glob_block_fires_for_matching_target() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().expect("tempdir");
    // SSC-5.b: `Host *.example.com` now resolves against the connection
    // target via the shared SSC-4.b pattern matcher.
    write_ssh_config(&home, "Host *.example.com\n  Compression yes\n");
    let _home_guard = EnvGuard::set("HOME", home.path().as_os_str());
    let _userprofile_guard = EnvGuard::set("USERPROFILE", home.path().as_os_str());

    let command = SshCommand::new("web1.example.com");
    assert!(command.has_ssh_compression());
}

#[test]
fn per_host_negation_blocks_match_for_banned_target() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().expect("tempdir");
    // SSC-5.b: bang-prefixed token causes the pattern-list to fail when
    // the negated token matches the target, even though the positive
    // `*` token would otherwise match.
    write_ssh_config(&home, "Host !banned.example.com *\n  Compression yes\n");
    let _home_guard = EnvGuard::set("HOME", home.path().as_os_str());
    let _userprofile_guard = EnvGuard::set("USERPROFILE", home.path().as_os_str());

    let banned = SshCommand::new("banned.example.com");
    assert!(!banned.has_ssh_compression());

    let allowed = SshCommand::new("ok.example.com");
    assert!(allowed.has_ssh_compression());
}

#[test]
fn compression_no_returns_false() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().expect("tempdir");
    write_ssh_config(&home, "Compression no\n");
    let _home_guard = EnvGuard::set("HOME", home.path().as_os_str());
    let _userprofile_guard = EnvGuard::set("USERPROFILE", home.path().as_os_str());

    assert!(!plain_cmd().has_ssh_compression());
}

#[test]
fn malformed_config_falls_back_to_false() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().expect("tempdir");
    // Lines without a value, stray separators, and unrecognised
    // Compression values must not abort the transfer. The lookup logs
    // and returns false so the caller falls back to argv-only.
    write_ssh_config(
        &home,
        "Compression\n===\nHost\n  Compression yes-but-not-quite\n",
    );
    let _home_guard = EnvGuard::set("HOME", home.path().as_os_str());
    let _userprofile_guard = EnvGuard::set("USERPROFILE", home.path().as_os_str());

    assert!(!plain_cmd().has_ssh_compression());
}

#[test]
fn empty_config_returns_false() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().expect("tempdir");
    write_ssh_config(&home, "");
    let _home_guard = EnvGuard::set("HOME", home.path().as_os_str());
    let _userprofile_guard = EnvGuard::set("USERPROFILE", home.path().as_os_str());

    assert!(!plain_cmd().has_ssh_compression());
}

#[test]
fn missing_config_returns_false() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().expect("tempdir");
    // No .ssh directory under HOME.
    let _home_guard = EnvGuard::set("HOME", home.path().as_os_str());
    let _userprofile_guard = EnvGuard::set("USERPROFILE", home.path().as_os_str());

    assert!(!plain_cmd().has_ssh_compression());
}

#[test]
fn dash_f_override_wins_over_home_config() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().expect("tempdir");
    // `~/.ssh/config` disables compression. The `-F` override is
    // consulted first and flips the answer to true.
    write_ssh_config(&home, "Compression no\n");
    let _home_guard = EnvGuard::set("HOME", home.path().as_os_str());
    let _userprofile_guard = EnvGuard::set("USERPROFILE", home.path().as_os_str());

    let override_dir = TempDir::new().expect("tempdir");
    let override_path = override_dir.path().join("override.config");
    fs::write(&override_path, "Compression yes\n").expect("write override");

    let mut command = plain_cmd();
    command.push_option("-F");
    command.push_option(override_path.as_os_str());

    assert!(command.has_ssh_compression());
}

#[test]
fn dash_f_combined_form_is_honoured() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().expect("tempdir");
    let _home_guard = EnvGuard::set("HOME", home.path().as_os_str());
    let _userprofile_guard = EnvGuard::set("USERPROFILE", home.path().as_os_str());

    let override_dir = TempDir::new().expect("tempdir");
    let override_path = override_dir.path().join("override.config");
    fs::write(&override_path, "Compression yes\n").expect("write override");

    let mut combined = OsString::from("-F");
    combined.push(override_path.as_os_str());

    let mut command = plain_cmd();
    command.push_option(combined);

    assert!(command.has_ssh_compression());
}

#[test]
fn argv_dash_c_still_wins_when_config_disables() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().expect("tempdir");
    write_ssh_config(&home, "Compression no\n");
    let _home_guard = EnvGuard::set("HOME", home.path().as_os_str());
    let _userprofile_guard = EnvGuard::set("USERPROFILE", home.path().as_os_str());

    // The argv path short-circuits the config lookup, so `-C` on the
    // rsync invocation continues to fire the SSC-1 warning even when
    // ssh_config disables compression.
    let mut command = plain_cmd();
    command.push_option("-C");
    assert!(command.has_ssh_compression());

    // Sanity: without -C, the same config keeps the answer false.
    assert!(!plain_cmd().has_ssh_compression());
}

// MED-6: integration tests for Match exec block detection via
// `has_ssh_compression()`. These verify that the full lookup path
// (file read -> parse -> compression check) correctly handles
// ssh_config files containing Match exec blocks.

#[test]
fn match_exec_block_with_compression_yes_returns_false() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().expect("tempdir");
    // A `Match exec` block containing `Compression yes` should not
    // enable the compression flag because the exec condition is not
    // evaluated. The function returns `false` since no other scope
    // enables compression.
    write_ssh_config(
        &home,
        "Match exec /usr/local/bin/check-vpn\n  Compression yes\n",
    );
    let _home_guard = EnvGuard::set("HOME", home.path().as_os_str());
    let _userprofile_guard = EnvGuard::set("USERPROFILE", home.path().as_os_str());

    assert!(!plain_cmd().has_ssh_compression());
}

#[test]
fn match_exec_block_compression_inside_exec_scope() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().expect("tempdir");
    // After `Match exec`, the active block is `MatchExecSkipped` until
    // another `Host` or `Match` directive resets it. So `Compression yes`
    // on the following line is inside the exec block scope and should
    // not contribute to the compression result.
    write_ssh_config(
        &home,
        "Match exec /usr/local/bin/check-vpn\n  ForwardAgent yes\n\
         Compression yes\n",
    );
    let _home_guard = EnvGuard::set("HOME", home.path().as_os_str());
    let _userprofile_guard = EnvGuard::set("USERPROFILE", home.path().as_os_str());

    assert!(!plain_cmd().has_ssh_compression());
}

#[test]
fn match_exec_with_host_star_compression_detected() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().expect("tempdir");
    // The `Host *` block after the exec block should be evaluated
    // normally.
    write_ssh_config(
        &home,
        "Match exec /usr/local/bin/check-vpn\n  Compression yes\n\
         Host *\n  Compression yes\n",
    );
    let _home_guard = EnvGuard::set("HOME", home.path().as_os_str());
    let _userprofile_guard = EnvGuard::set("USERPROFILE", home.path().as_os_str());

    assert!(plain_cmd().has_ssh_compression());
}

#[test]
fn match_exec_with_match_all_compression_detected() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().expect("tempdir");
    // A `Match all` block after a `Match exec` block should be
    // evaluated normally and contribute its compression setting.
    write_ssh_config(
        &home,
        "Match exec /usr/local/bin/check-vpn\n  Compression yes\n\
         Match all\n  Compression yes\n",
    );
    let _home_guard = EnvGuard::set("HOME", home.path().as_os_str());
    let _userprofile_guard = EnvGuard::set("USERPROFILE", home.path().as_os_str());

    assert!(plain_cmd().has_ssh_compression());
}

#[test]
fn match_exec_only_no_other_compression_source() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().expect("tempdir");
    // When the only compression setting is inside a `Match exec` block,
    // has_ssh_compression must return false since we cannot evaluate
    // the exec condition.
    write_ssh_config(
        &home,
        "Host *\n  ServerAliveInterval 60\n\n\
         Match exec \"test -f /etc/vpn.conf\"\n  Compression yes\n",
    );
    let _home_guard = EnvGuard::set("HOME", home.path().as_os_str());
    let _userprofile_guard = EnvGuard::set("USERPROFILE", home.path().as_os_str());

    assert!(!plain_cmd().has_ssh_compression());
}

#[test]
fn dash_f_override_with_match_exec_block() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = TempDir::new().expect("tempdir");
    write_ssh_config(&home, "Compression no\n");
    let _home_guard = EnvGuard::set("HOME", home.path().as_os_str());
    let _userprofile_guard = EnvGuard::set("USERPROFILE", home.path().as_os_str());

    // The `-F` override file contains a `Match exec` block with
    // `Compression yes`. Since `-F` is consulted first and it only
    // has compression inside the exec block, the result should be false.
    let override_dir = TempDir::new().expect("tempdir");
    let override_path = override_dir.path().join("override.config");
    fs::write(
        &override_path,
        "Match exec /usr/local/bin/check-vpn\n  Compression yes\n",
    )
    .expect("write override");

    let mut command = plain_cmd();
    command.push_option("-F");
    command.push_option(override_path.as_os_str());

    assert!(!command.has_ssh_compression());
}
