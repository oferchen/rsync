//! Environment-variable overrides for [`SpillPolicy`].
//!
//! Operators can tune the spill backend at runtime without recompiling by
//! exporting one or more of the variables documented on
//! [`apply_env_overrides`]. Each variable is read independently; absent
//! variables leave the corresponding field unchanged. Invalid values are
//! logged via `tracing::warn!` (when the `tracing` feature is enabled) and
//! the field is left at its previous value - never a panic.

use std::env;
use std::path::PathBuf;

use super::policy::{SpillCompression, SpillPolicy};

/// Environment variable that overrides [`SpillPolicy::dir`].
///
/// Accepts any non-empty path. The path is not validated here - the spill
/// backend surfaces directory errors at construction time.
pub const ENV_SPILL_DIR: &str = "OC_RSYNC_SPILL_DIR";

/// Environment variable that overrides [`SpillPolicy::threshold_bytes`].
///
/// Parses as `u64`. Non-numeric or out-of-range values are ignored.
pub const ENV_SPILL_THRESHOLD_BYTES: &str = "OC_RSYNC_SPILL_THRESHOLD_BYTES";

/// Environment variable that overrides [`SpillPolicy::compression`].
///
/// Accepts `none`, `zstd`, or `zstd:N` (where `N` is the integer level).
/// Any `zstd*` value is rejected when the `spill-compression` feature is
/// disabled at build time.
pub const ENV_SPILL_COMPRESSION: &str = "OC_RSYNC_SPILL_COMPRESSION";

/// Environment variable that enables in-memory-only mode.
///
/// Truthy values (`1`, `true`, `yes` - case-insensitive) set
/// [`SpillPolicy::in_memory_only`] to `true`, preventing the reorder
/// buffer from writing to disk. Any other non-empty value is logged and
/// ignored.
pub const ENV_NO_SPILL: &str = "OC_RSYNC_NO_SPILL";

/// Applies environment-variable overrides in place.
///
/// Reads the variables documented on [`ENV_SPILL_DIR`],
/// [`ENV_SPILL_THRESHOLD_BYTES`], [`ENV_SPILL_COMPRESSION`], and
/// [`ENV_NO_SPILL`]; each missing variable leaves the matching field
/// untouched. Invalid values emit a `tracing::warn!` (under the `tracing`
/// feature) and leave the field unchanged. The function never panics.
pub fn apply_env_overrides(policy: &mut SpillPolicy) {
    apply_dir(policy);
    apply_threshold(policy);
    apply_compression(policy);
    apply_no_spill(policy);
}

fn apply_dir(policy: &mut SpillPolicy) {
    let Ok(val) = env::var(ENV_SPILL_DIR) else {
        return;
    };
    if val.is_empty() {
        warn_invalid(ENV_SPILL_DIR, &val, "value is empty");
        return;
    }
    policy.dir = Some(PathBuf::from(val));
}

fn apply_threshold(policy: &mut SpillPolicy) {
    let Ok(val) = env::var(ENV_SPILL_THRESHOLD_BYTES) else {
        return;
    };
    match val.parse::<u64>() {
        Ok(parsed) => policy.threshold_bytes = Some(parsed),
        Err(_) => warn_invalid(ENV_SPILL_THRESHOLD_BYTES, &val, "expected unsigned integer"),
    }
}

fn apply_no_spill(policy: &mut SpillPolicy) {
    let Ok(val) = env::var(ENV_NO_SPILL) else {
        return;
    };
    match val.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => policy.in_memory_only = true,
        "" => {} // empty string = unset
        other => warn_invalid(ENV_NO_SPILL, other, "expected '1', 'true', or 'yes'"),
    }
}

fn apply_compression(policy: &mut SpillPolicy) {
    let Ok(raw) = env::var(ENV_SPILL_COMPRESSION) else {
        return;
    };
    match parse_compression(&raw) {
        Ok(comp) => policy.compression = comp,
        Err(reason) => warn_invalid(ENV_SPILL_COMPRESSION, &raw, reason),
    }
}

fn parse_compression(raw: &str) -> Result<SpillCompression, &'static str> {
    let lower = raw.trim().to_ascii_lowercase();
    if lower == "none" {
        return Ok(SpillCompression::None);
    }
    if let Some(rest) = lower.strip_prefix("zstd") {
        return parse_zstd(rest);
    }
    Err("expected 'none', 'zstd', or 'zstd:N'")
}

#[cfg(feature = "spill-compression")]
fn parse_zstd(rest: &str) -> Result<SpillCompression, &'static str> {
    if rest.is_empty() {
        return Ok(SpillCompression::Zstd { level: 0 });
    }
    let Some(level_str) = rest.strip_prefix(':') else {
        return Err("expected 'zstd' or 'zstd:N'");
    };
    let level = level_str
        .parse::<i32>()
        .map_err(|_| "expected integer zstd level")?;
    Ok(SpillCompression::Zstd { level })
}

#[cfg(not(feature = "spill-compression"))]
fn parse_zstd(_rest: &str) -> Result<SpillCompression, &'static str> {
    Err("zstd compression requires the 'spill-compression' build feature")
}

#[cfg(feature = "tracing")]
fn warn_invalid(name: &str, value: &str, reason: &str) {
    tracing::warn!(
        env_var = name,
        value = value,
        reason = reason,
        "invalid spill-policy env var; leaving field unchanged"
    );
}

#[cfg(not(feature = "tracing"))]
fn warn_invalid(_name: &str, _value: &str, _reason: &str) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes env mutation across this module's tests so concurrent
    /// nextest threads do not race on shared process state.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// RAII guard that sets/removes an env var and restores it on drop.
    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        #[allow(unsafe_code)]
        fn set(key: &'static str, value: &str) -> Self {
            let original = env::var(key).ok();
            // SAFETY: serialized via ENV_LOCK held by the caller for the
            // duration of the test.
            unsafe { env::set_var(key, value) };
            Self { key, original }
        }

        #[allow(unsafe_code)]
        fn remove(key: &'static str) -> Self {
            let original = env::var(key).ok();
            // SAFETY: see set() above.
            unsafe { env::remove_var(key) };
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            // SAFETY: see EnvGuard::set() above.
            match &self.original {
                Some(val) => unsafe { env::set_var(self.key, val) },
                None => unsafe { env::remove_var(self.key) },
            }
        }
    }

    /// Removes every env var owned by this module so individual tests start
    /// from a clean slate regardless of the surrounding shell environment.
    fn reset_env() -> Vec<EnvGuard> {
        vec![
            EnvGuard::remove(ENV_SPILL_DIR),
            EnvGuard::remove(ENV_SPILL_THRESHOLD_BYTES),
            EnvGuard::remove(ENV_SPILL_COMPRESSION),
            EnvGuard::remove(ENV_NO_SPILL),
        ]
    }

    #[test]
    fn no_env_vars_leaves_policy_unchanged() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guards = reset_env();
        let mut policy = SpillPolicy::default();
        let original = policy.clone();
        apply_env_overrides(&mut policy);
        assert_eq!(policy, original);
    }

    #[test]
    fn env_spill_dir_sets_dir() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guards = reset_env();
        let _set = EnvGuard::set(ENV_SPILL_DIR, "/var/tmp/oc-rsync-spill");
        let mut policy = SpillPolicy::default();
        apply_env_overrides(&mut policy);
        assert_eq!(
            policy.dir.as_deref(),
            Some(PathBuf::from("/var/tmp/oc-rsync-spill").as_path())
        );
    }

    #[test]
    fn env_spill_dir_empty_leaves_unchanged() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guards = reset_env();
        let _set = EnvGuard::set(ENV_SPILL_DIR, "");
        let mut policy = SpillPolicy::default().with_dir(PathBuf::from("/keep/me"));
        apply_env_overrides(&mut policy);
        assert_eq!(
            policy.dir.as_deref(),
            Some(PathBuf::from("/keep/me").as_path())
        );
    }

    #[test]
    fn env_spill_threshold_sets_value() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guards = reset_env();
        let _set = EnvGuard::set(ENV_SPILL_THRESHOLD_BYTES, "131072");
        let mut policy = SpillPolicy::default();
        apply_env_overrides(&mut policy);
        assert_eq!(policy.threshold_bytes, Some(131_072));
    }

    #[test]
    fn env_spill_threshold_invalid_leaves_unchanged() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guards = reset_env();
        let _set = EnvGuard::set(ENV_SPILL_THRESHOLD_BYTES, "not_a_number");
        let mut policy = SpillPolicy::with_threshold(4096);
        apply_env_overrides(&mut policy);
        assert_eq!(policy.threshold_bytes, Some(4096));
    }

    #[test]
    fn env_spill_threshold_negative_leaves_unchanged() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guards = reset_env();
        let _set = EnvGuard::set(ENV_SPILL_THRESHOLD_BYTES, "-1");
        let mut policy = SpillPolicy::with_threshold(2048);
        apply_env_overrides(&mut policy);
        assert_eq!(policy.threshold_bytes, Some(2048));
    }

    #[test]
    fn env_spill_compression_none() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guards = reset_env();
        let _set = EnvGuard::set(ENV_SPILL_COMPRESSION, "none");
        let mut policy = SpillPolicy::default();
        apply_env_overrides(&mut policy);
        assert_eq!(policy.compression, SpillCompression::None);
    }

    #[cfg(feature = "spill-compression")]
    #[test]
    fn env_spill_compression_zstd_default_level() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guards = reset_env();
        let _set = EnvGuard::set(ENV_SPILL_COMPRESSION, "zstd");
        let mut policy = SpillPolicy::default();
        apply_env_overrides(&mut policy);
        assert_eq!(policy.compression, SpillCompression::Zstd { level: 0 });
    }

    #[cfg(feature = "spill-compression")]
    #[test]
    fn env_spill_compression_zstd_explicit_level() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guards = reset_env();
        let _set = EnvGuard::set(ENV_SPILL_COMPRESSION, "zstd:7");
        let mut policy = SpillPolicy::default();
        apply_env_overrides(&mut policy);
        assert_eq!(policy.compression, SpillCompression::Zstd { level: 7 });
    }

    #[cfg(not(feature = "spill-compression"))]
    #[test]
    fn env_spill_compression_zstd_without_feature_is_noop() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guards = reset_env();
        let _set = EnvGuard::set(ENV_SPILL_COMPRESSION, "zstd:3");
        let mut policy = SpillPolicy::default();
        apply_env_overrides(&mut policy);
        assert_eq!(policy.compression, SpillCompression::None);
    }

    #[test]
    fn env_spill_compression_invalid_leaves_unchanged() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guards = reset_env();
        let _set = EnvGuard::set(ENV_SPILL_COMPRESSION, "lz4");
        let mut policy = SpillPolicy::default();
        let original = policy.clone();
        apply_env_overrides(&mut policy);
        assert_eq!(policy.compression, original.compression);
    }

    #[test]
    fn from_env_returns_default_when_unset() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guards = reset_env();
        assert_eq!(SpillPolicy::from_env(), SpillPolicy::default());
    }

    #[test]
    fn from_env_applies_overrides() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guards = reset_env();
        let _dir = EnvGuard::set(ENV_SPILL_DIR, "/tmp/spill-from-env");
        let _thresh = EnvGuard::set(ENV_SPILL_THRESHOLD_BYTES, "65536");
        let policy = SpillPolicy::from_env();
        assert_eq!(policy.threshold_bytes, Some(65_536));
        assert_eq!(
            policy.dir.as_deref(),
            Some(PathBuf::from("/tmp/spill-from-env").as_path())
        );
    }

    #[test]
    fn env_no_spill_truthy_one() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guards = reset_env();
        let _set = EnvGuard::set(ENV_NO_SPILL, "1");
        let mut policy = SpillPolicy::default();
        apply_env_overrides(&mut policy);
        assert!(policy.in_memory_only);
    }

    #[test]
    fn env_no_spill_truthy_true() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guards = reset_env();
        let _set = EnvGuard::set(ENV_NO_SPILL, "true");
        let mut policy = SpillPolicy::default();
        apply_env_overrides(&mut policy);
        assert!(policy.in_memory_only);
    }

    #[test]
    fn env_no_spill_truthy_yes_case_insensitive() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guards = reset_env();
        let _set = EnvGuard::set(ENV_NO_SPILL, "YES");
        let mut policy = SpillPolicy::default();
        apply_env_overrides(&mut policy);
        assert!(policy.in_memory_only);
    }

    #[test]
    fn env_no_spill_invalid_leaves_unchanged() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guards = reset_env();
        let _set = EnvGuard::set(ENV_NO_SPILL, "maybe");
        let mut policy = SpillPolicy::default();
        apply_env_overrides(&mut policy);
        assert!(!policy.in_memory_only);
    }

    #[test]
    fn env_no_spill_empty_leaves_unchanged() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guards = reset_env();
        let _set = EnvGuard::set(ENV_NO_SPILL, "");
        let mut policy = SpillPolicy::default();
        apply_env_overrides(&mut policy);
        assert!(!policy.in_memory_only);
    }

    #[test]
    fn env_no_spill_absent_leaves_unchanged() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guards = reset_env();
        let mut policy = SpillPolicy::default();
        apply_env_overrides(&mut policy);
        assert!(!policy.in_memory_only);
    }
}
