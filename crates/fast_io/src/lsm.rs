//! Linux Security Module (LSM) detection helpers.
//!
//! Exposes a small, side-effect-free reader for `/sys/kernel/security/lsm`
//! plus higher-level classifiers used by the CLI `--lsm-status` diagnostic
//! and the receiver's `PermissionDenied` audit-log hint.
//!
//! The pseudofile lists the comma-separated names of every LSM the running
//! kernel has loaded - typically `lockdown,capability,landlock,yama,bpf`
//! on a vanilla 6.x kernel, with `selinux`, `apparmor`, or `smack` appended
//! on hosts where the corresponding mandatory access controller is active.
//!
//! All helpers short-circuit to "no LSMs visible" on non-Linux targets and
//! on Linux hosts without securityfs mounted (common in minimal containers).
//! Detection is purely observability; absence of the file never blocks
//! callers and never propagates an error.

use std::fs;
use std::sync::OnceLock;

/// Path to the kernel's active-LSM list pseudofile.
///
/// On Linux with securityfs mounted the file contains a comma-separated
/// list (for example `lockdown,capability,landlock,yama,apparmor,bpf`).
/// The file is absent on non-Linux hosts and on Linux containers without
/// `/sys` exposed.
#[cfg(target_os = "linux")]
pub const LSM_LIST_PATH: &str = "/sys/kernel/security/lsm";

/// Stub path on non-Linux targets. Reads always return `None`.
#[cfg(not(target_os = "linux"))]
pub const LSM_LIST_PATH: &str = "";

/// Names recognised as mandatory access control LSMs.
///
/// Used by [`has_mandatory_lsm`] to decide whether an `EACCES` from the
/// kernel was plausibly produced by an LSM policy decision worth pointing
/// the operator at. The list intentionally excludes:
///
/// - `capability` - universally loaded; not a MAC layer.
/// - `landlock` - unprivileged and per-process; denials surface through
///   the calling syscall's return code, not the audit log.
/// - `lockdown`, `yama`, `bpf` - narrow-scope minor LSMs whose denials
///   target kernel-tracing or attaching to other processes, not regular
///   file I/O, so they would not produce the `EACCES` shape this hint
///   is correlated with.
const MANDATORY_LSM_NAMES: &[&str] = &["selinux", "apparmor", "smack", "tomoyo"];

/// Reads `/sys/kernel/security/lsm` and returns the trimmed, comma-split
/// LSM names.
///
/// Returns `None` when:
/// - the host is not Linux,
/// - securityfs is not mounted (file does not exist),
/// - the file is unreadable (permission denied, I/O error),
/// - the file is empty after trimming whitespace.
///
/// The reader is uncached so callers that need a single fixed snapshot
/// should hold the result. See [`has_mandatory_lsm`] for a cached
/// classifier suitable for hot paths.
#[must_use]
pub fn read_active_lsms() -> Option<Vec<String>> {
    read_active_lsms_from(LSM_LIST_PATH)
}

/// Reads `path` as a comma-separated LSM list, factored so tests can pass
/// in a temp-file fixture instead of the live `/sys` pseudofile.
///
/// Trims leading and trailing whitespace from the file contents and then
/// from each comma-separated entry. Empty entries (consecutive commas or
/// stray whitespace) are dropped. Returns `None` when the file is missing,
/// unreadable, or empty after trimming.
#[must_use]
pub fn read_active_lsms_from(path: &str) -> Option<Vec<String>> {
    if path.is_empty() {
        return None;
    }
    let contents = fs::read_to_string(path).ok()?;
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        return None;
    }
    let names: Vec<String> = trimmed
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if names.is_empty() { None } else { Some(names) }
}

/// Returns `true` when the running kernel exposes at least one mandatory
/// access control LSM (SELinux, AppArmor, Smack, Tomoyo, lockdown, Yama,
/// or BPF-LSM).
///
/// The result is computed once on first call and cached for the process
/// lifetime via [`OnceLock`]; subsequent calls are a single atomic load.
/// This is the recommended classifier for hot paths that need to decide
/// whether to attach an "check the audit log" hint to an `EACCES` error.
///
/// Returns `false` when [`read_active_lsms`] returns `None`, when every
/// listed LSM is in the non-mandatory bucket (`capability`, `landlock`),
/// or on non-Linux targets.
#[must_use]
pub fn has_mandatory_lsm() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(|| classify_mandatory(read_active_lsms().as_deref()))
}

/// Returns `true` iff `lsms` contains at least one name in
/// [`MANDATORY_LSM_NAMES`]. Pure function exposed for testing.
#[must_use]
pub fn classify_mandatory(lsms: Option<&[String]>) -> bool {
    let Some(lsms) = lsms else {
        return false;
    };
    lsms.iter()
        .any(|name| MANDATORY_LSM_NAMES.contains(&name.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_lsm_fixture(contents: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().expect("create temp file");
        file.write_all(contents.as_bytes())
            .expect("write lsm fixture");
        file.flush().expect("flush lsm fixture");
        file
    }

    #[test]
    fn read_active_lsms_from_handles_trailing_newline() {
        let fixture = write_lsm_fixture("lockdown,capability,landlock,yama,bpf\n");
        let path = fixture.path().to_string_lossy().into_owned();
        let names = read_active_lsms_from(&path).expect("non-empty");
        assert_eq!(
            names,
            vec!["lockdown", "capability", "landlock", "yama", "bpf"]
        );
    }

    #[test]
    fn read_active_lsms_from_trims_whitespace_in_entries() {
        let fixture = write_lsm_fixture("  selinux , capability , bpf  \n");
        let path = fixture.path().to_string_lossy().into_owned();
        let names = read_active_lsms_from(&path).expect("non-empty");
        assert_eq!(names, vec!["selinux", "capability", "bpf"]);
    }

    #[test]
    fn read_active_lsms_from_drops_empty_entries() {
        let fixture = write_lsm_fixture("capability,,landlock,\n");
        let path = fixture.path().to_string_lossy().into_owned();
        let names = read_active_lsms_from(&path).expect("non-empty");
        assert_eq!(names, vec!["capability", "landlock"]);
    }

    #[test]
    fn read_active_lsms_from_returns_none_when_empty() {
        let fixture = write_lsm_fixture("   \n");
        let path = fixture.path().to_string_lossy().into_owned();
        assert!(read_active_lsms_from(&path).is_none());
    }

    #[test]
    fn read_active_lsms_from_returns_none_when_missing() {
        assert!(read_active_lsms_from("/nonexistent/path/lsm-fixture").is_none());
    }

    #[test]
    fn read_active_lsms_from_empty_path_returns_none() {
        assert!(read_active_lsms_from("").is_none());
    }

    #[test]
    fn classify_mandatory_detects_selinux() {
        let lsms = vec!["selinux".to_owned(), "capability".to_owned()];
        assert!(classify_mandatory(Some(lsms.as_slice())));
    }

    #[test]
    fn classify_mandatory_detects_apparmor() {
        let lsms = vec![
            "lockdown".to_owned(),
            "capability".to_owned(),
            "apparmor".to_owned(),
        ];
        assert!(classify_mandatory(Some(lsms.as_slice())));
    }

    #[test]
    fn classify_mandatory_false_for_capability_only() {
        let lsms = vec!["capability".to_owned()];
        assert!(!classify_mandatory(Some(lsms.as_slice())));
    }

    #[test]
    fn classify_mandatory_false_for_capability_plus_landlock() {
        let lsms = vec!["capability".to_owned(), "landlock".to_owned()];
        assert!(!classify_mandatory(Some(lsms.as_slice())));
    }

    #[test]
    fn classify_mandatory_false_for_lockdown_yama_bpf_only() {
        // Minor LSMs that don't produce file-I/O EACCES; the hint must
        // not fire on a kernel where only these are loaded.
        let lsms = vec![
            "lockdown".to_owned(),
            "capability".to_owned(),
            "yama".to_owned(),
            "bpf".to_owned(),
            "landlock".to_owned(),
        ];
        assert!(!classify_mandatory(Some(lsms.as_slice())));
    }

    #[test]
    fn classify_mandatory_false_for_none() {
        assert!(!classify_mandatory(None));
    }

    #[test]
    fn classify_mandatory_false_for_empty_slice() {
        let lsms: Vec<String> = Vec::new();
        assert!(!classify_mandatory(Some(lsms.as_slice())));
    }
}
