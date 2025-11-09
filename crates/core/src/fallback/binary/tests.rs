use super::{
    availability, describe_missing_fallback_binary, fallback_binary_available,
    fallback_binary_candidates,
};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::File;
#[cfg(not(unix))]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tempfile::{NamedTempFile, TempDir};

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set_os(key: &'static str, value: &OsStr) -> Self {
        let previous = env::var_os(key);
        #[allow(unsafe_code)]
        unsafe {
            env::set_var(key, value);
        }
        Self { key, previous }
    }

    #[cfg(windows)]
    fn set(key: &'static str, value: &str) -> Self {
        let previous = env::var_os(key);
        #[allow(unsafe_code)]
        unsafe {
            env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            #[allow(unsafe_code)]
            unsafe {
                env::set_var(self.key, previous);
            }
        } else {
            #[allow(unsafe_code)]
            unsafe {
                env::remove_var(self.key);
            }
        }
    }
}

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[test]
fn describe_missing_fallback_binary_lists_single_env() {
    let message =
        describe_missing_fallback_binary(OsStr::new("/usr/bin/rsync"), &["OC_RSYNC_FALLBACK"]);
    assert!(message.contains("install upstream rsync"));
    assert!(message.contains("OC_RSYNC_FALLBACK"));
    assert!(!message.contains(","));
}

#[test]
fn describe_missing_fallback_binary_lists_multiple_envs() {
    let message = describe_missing_fallback_binary(
        OsStr::new("/usr/bin/rsync"),
        &["OC_RSYNC_DAEMON_FALLBACK", "OC_RSYNC_FALLBACK"],
    );
    assert!(message.contains("OC_RSYNC_DAEMON_FALLBACK"));
    assert!(message.contains("OC_RSYNC_FALLBACK"));
    assert!(message.contains("or"));
}

#[test]
fn fallback_binary_available_detects_executable() {
    #[allow(unused_mut)]
    let mut temp = NamedTempFile::new().expect("tempfile");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = temp.as_file().metadata().expect("metadata").permissions();
        permissions.set_mode(0o755);
        temp.as_file().set_permissions(permissions).expect("chmod");
    }

    #[cfg(not(unix))]
    {
        writeln!(temp, "echo ok").expect("write");
    }

    assert!(fallback_binary_available(temp.path().as_os_str()));
}

#[cfg(unix)]
fn identity(euid: u32, egid: u32, groups: &[u32]) -> super::unix::UnixProcessIdentity {
    super::unix::UnixProcessIdentity::for_tests(euid, egid, groups)
}

#[cfg(unix)]
#[test]
fn unix_mode_respects_owner_permissions() {
    let subject = identity(1_000, 1_000, &[]);
    assert!(super::unix::unix_mode_allows_execution(
        0o700, 1_000, 2_000, &subject
    ));
    assert!(!super::unix::unix_mode_allows_execution(
        0o070, 2_000, 2_000, &subject
    ));
}

#[cfg(unix)]
#[test]
fn unix_mode_respects_group_membership() {
    let subject = identity(2_000, 100, &[200, 300]);
    assert!(super::unix::unix_mode_allows_execution(
        0o070, 3_000, 200, &subject
    ));
    assert!(!super::unix::unix_mode_allows_execution(
        0o070, 3_000, 500, &subject
    ));
}

#[cfg(unix)]
#[test]
fn unix_mode_considers_other_permissions() {
    let subject = identity(2_000, 100, &[]);
    assert!(super::unix::unix_mode_allows_execution(
        0o001, 3_000, 500, &subject
    ));
    assert!(!super::unix::unix_mode_allows_execution(
        0o000, 3_000, 500, &subject
    ));
}

#[cfg(unix)]
#[test]
fn unix_mode_treats_root_as_universal_executor() {
    let root = identity(0, 0, &[]);
    assert!(super::unix::unix_mode_allows_execution(
        0o100, 3_000, 500, &root
    ));
    assert!(!super::unix::unix_mode_allows_execution(
        0o000, 3_000, 500, &root
    ));
}

#[test]
fn fallback_binary_available_detects_removal() {
    #[allow(unused_mut)]
    let mut temp = NamedTempFile::new().expect("tempfile");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = temp.as_file().metadata().expect("metadata").permissions();
        permissions.set_mode(0o755);
        temp.as_file().set_permissions(permissions).expect("chmod");
    }

    let os_path = temp.path().as_os_str().to_os_string();

    assert!(fallback_binary_available(os_path.as_os_str()));

    drop(temp);

    assert!(!fallback_binary_available(os_path.as_os_str()));
}

#[test]
fn fallback_binary_available_rejects_missing_file() {
    let missing = Path::new("/nonexistent/path/to/binary");
    assert!(!fallback_binary_available(missing.as_os_str()));
}

#[test]
fn fallback_binary_available_respects_path_changes() {
    let _lock = env_lock().lock().expect("lock env");

    let temp_dir = TempDir::new().expect("tempdir");
    let binary_name = if cfg!(windows) { "rsync.exe" } else { "rsync" };
    let binary_path = temp_dir.path().join(binary_name);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let file = File::create(&binary_path).expect("create helper placeholder");
        let mut permissions = file.metadata().expect("metadata").permissions();
        permissions.set_mode(0o755);
        file.set_permissions(permissions).expect("chmod");
    }

    #[cfg(not(unix))]
    {
        File::create(&binary_path).expect("create helper placeholder");
    }

    {
        let _path_guard = EnvGuard::set_os("PATH", OsStr::new(""));
        assert!(
            !fallback_binary_available(OsStr::new("rsync")),
            "empty PATH should not locate the fallback binary"
        );
    }

    let joined = env::join_paths([temp_dir.path()]).expect("join paths");
    let _path_guard = EnvGuard::set_os("PATH", joined.as_os_str());
    assert!(
        fallback_binary_available(OsStr::new("rsync")),
        "updated PATH should locate the fallback binary"
    );
}

#[test]
fn fallback_binary_available_rechecks_after_negative_cache_ttl() {
    let _lock = env_lock().lock().expect("lock env");

    availability::availability_cache()
        .lock()
        .expect("fallback availability cache lock poisoned")
        .clear();

    let temp_dir = TempDir::new().expect("tempdir");
    let joined = env::join_paths([temp_dir.path()]).expect("join paths");
    let _path_guard = EnvGuard::set_os("PATH", joined.as_os_str());

    #[cfg(windows)]
    let _pathext_guard = EnvGuard::set("PATHEXT", ".exe");

    let binary_name = if cfg!(windows) { "rsync.exe" } else { "rsync" };
    let binary_path = temp_dir.path().join(binary_name);

    assert!(
        !fallback_binary_available(OsStr::new("rsync")),
        "missing fallback binary should not be reported as available",
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let file = File::create(&binary_path).expect("create fallback binary placeholder");
        let mut permissions = file.metadata().expect("metadata").permissions();
        permissions.set_mode(0o755);
        file.set_permissions(permissions).expect("chmod");
    }

    #[cfg(not(unix))]
    {
        File::create(&binary_path).expect("create fallback binary placeholder");
    }

    let wait = availability::NEGATIVE_CACHE_TTL + std::time::Duration::from_millis(50);
    std::thread::sleep(wait);

    assert!(
        fallback_binary_available(OsStr::new("rsync")),
        "fallback availability should be recomputed after the negative cache TTL expires",
    );
}

#[test]
fn fallback_binary_candidates_deduplicates_duplicate_path_entries() {
    let _lock = env_lock().lock().expect("lock env");

    let temp_dir = TempDir::new().expect("tempdir");
    let joined = env::join_paths([temp_dir.path(), temp_dir.path()]).expect("join paths");
    let _path_guard = EnvGuard::set_os("PATH", joined.as_os_str());

    #[cfg(windows)]
    let _pathext_guard = EnvGuard::set("PATHEXT", ".exe");

    let expected_name = if cfg!(windows) { "rsync.exe" } else { "rsync" };
    let expected_path = temp_dir.path().join(expected_name);
    let candidates = fallback_binary_candidates(OsStr::new("rsync"));

    assert!(
        candidates
            .iter()
            .any(|candidate| candidate == &expected_path),
        "expected candidate {expected_path:?} missing from {candidates:?}"
    );

    let occurrences = candidates
        .iter()
        .filter(|candidate| *candidate == &expected_path)
        .count();
    assert_eq!(
        occurrences, 1,
        "candidate should only appear once even when PATH repeats entries"
    );
}

#[test]
fn fallback_binary_candidates_include_current_directory_for_empty_path_entries() {
    let _lock = env_lock().lock().expect("lock env");

    let temp_dir = TempDir::new().expect("tempdir");
    let joined =
        env::join_paths([PathBuf::new(), temp_dir.path().to_path_buf()]).expect("join paths");
    let _path_guard = EnvGuard::set_os("PATH", joined.as_os_str());

    #[cfg(windows)]
    let _pathext_guard = EnvGuard::set("PATHEXT", ".exe");

    let candidates = fallback_binary_candidates(OsStr::new("rsync"));

    #[cfg(not(windows))]
    let expected = Path::new("rsync");
    #[cfg(windows)]
    let expected = Path::new("rsync.exe");

    assert!(
        candidates.iter().any(|candidate| candidate == expected),
        "current-directory candidate missing from {candidates:?}"
    );
}

#[cfg(windows)]
#[test]
fn fallback_binary_candidates_deduplicate_pathext_variants() {
    use std::fs;

    let _lock = env_lock().lock().expect("lock env");

    let temp_dir = TempDir::new().expect("tempdir");
    let joined = env::join_paths([temp_dir.path()]).expect("join paths");
    let _path_guard = EnvGuard::set_os("PATH", joined.as_os_str());

    let _pathext_guard = EnvGuard::set("PATHEXT", ".EXE;.exe;.Com");

    let expected_path = temp_dir.path().join("rsync.EXE");
    fs::write(&expected_path, b"echo rsync").expect("write fallback binary candidate");

    let candidates = fallback_binary_candidates(OsStr::new("rsync"));
    let occurrences = candidates
        .iter()
        .filter(|candidate| *candidate == &expected_path)
        .count();

    assert_eq!(
        occurrences, 1,
        "PATHEXT entries that differ only by case should not duplicate candidates"
    );
}

#[cfg(windows)]
#[test]
fn fallback_binary_candidates_expand_explicit_windows_paths() {
    use std::fs;

    let _lock = env_lock().lock().expect("lock env");

    let temp_dir = TempDir::new().expect("tempdir");
    let base_path = temp_dir.path().join("bin").join("rsync");
    fs::create_dir_all(base_path.parent().expect("parent")).expect("create parent directory");

    let exe_path = base_path.with_extension("exe");
    fs::write(&exe_path, b"echo rsync").expect("write fallback binary candidate");

    let _pathext_guard = EnvGuard::set("PATHEXT", ".exe;.cmd");

    let candidates = fallback_binary_candidates(base_path.as_os_str());

    assert!(
        candidates.iter().any(|candidate| candidate == &base_path),
        "explicit path without extension should remain in candidate list"
    );
    assert!(
        candidates.iter().any(|candidate| candidate == &exe_path),
        "explicit Windows path should expand to include PATHEXT variants"
    );
}
