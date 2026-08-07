//! Resolution of the upstream rsync binary the fidelity matrix compares against.
//!
//! Every check in `cargo xtask validate` asserts "oc-rsync matches upstream".
//! That claim is only meaningful if "upstream" is the release oc-rsync targets.
//! A binary picked up from `PATH` may be any version - or, on macOS,
//! openrsync - so a PASS would mean "oc matches *something*" and a FAIL could
//! be an oracle-version difference rather than an oc defect.
//!
//! The oracle is therefore an explicit, verified input: it comes from the
//! interop harness's pinned install tree, its `--version` banner must report
//! [`ORACLE_VERSION`], and both the path and the banner are printed with the
//! results. A missing or wrong-version oracle refuses to run.

use std::path::{Path, PathBuf};

use crate::commands::interop::shared::upstream;
use crate::error::{TaskError, TaskResult};

/// The upstream release the fidelity matrix is defined against (protocol 32).
///
/// `tools/ci/run_interop.sh` builds and installs exactly this version, so the
/// pinned tree and this constant move together.
pub const ORACLE_VERSION: &str = "3.4.4";

/// Overrides the oracle binary for deliberate cross-version comparison.
///
/// When set, the named binary is used as-is and its version is reported but
/// not enforced. Unset - the default - is strict.
pub const ORACLE_ENV: &str = "OC_RSYNC_VALIDATE_UPSTREAM";

/// A resolved, version-verified upstream rsync to compare against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Oracle {
    /// Path to the upstream binary.
    pub path: PathBuf,
    /// First line of its `--version` output, printed alongside the results.
    pub banner: String,
}

/// Resolve the oracle for `workspace`, honouring [`ORACLE_ENV`].
pub fn resolve(workspace: &Path) -> TaskResult<Oracle> {
    let override_path = std::env::var_os(ORACLE_ENV).map(PathBuf::from);
    resolve_with(
        &upstream::install_root(workspace),
        override_path,
        upstream::version_banner,
    )
}

/// Resolution policy, with the install root, the override, and the version
/// probe supplied by the caller so it is exercisable without a real binary.
fn resolve_with(
    install_root: &Path,
    override_path: Option<PathBuf>,
    probe: impl Fn(&Path) -> Option<String>,
) -> TaskResult<Oracle> {
    let overridden = override_path.is_some();
    let path =
        override_path.unwrap_or_else(|| upstream::pinned_binary(install_root, ORACLE_VERSION));

    if !overridden && !path.is_file() {
        return Err(TaskError::Validation(format!(
            "upstream oracle missing: no rsync {ORACLE_VERSION} at {}\n\
             Build it with `bash tools/ci/run_interop.sh`, or point \
             {ORACLE_ENV} at a binary to compare against a different release.",
            path.display()
        )));
    }

    let Some(banner) = probe(&path) else {
        return Err(TaskError::Validation(format!(
            "upstream oracle unusable: `{} --version` produced no output",
            path.display()
        )));
    };

    let Some(found) = upstream::parse_release_version(&banner) else {
        return Err(TaskError::Validation(format!(
            "upstream oracle unusable: {} is not rsync - its banner reads `{banner}`",
            path.display()
        )));
    };

    if !overridden && found != ORACLE_VERSION {
        return Err(TaskError::Validation(format!(
            "upstream oracle version mismatch: {} reports {found}, the fidelity \
             matrix is defined against rsync {ORACLE_VERSION}\n\
             Rebuild it with `bash tools/ci/run_interop.sh`, or set \
             {ORACLE_ENV} to compare against {found} deliberately.",
            path.display()
        )));
    }

    Ok(Oracle { path, banner })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn banner(version: &str) -> String {
        format!("rsync  version {version}  protocol version 32")
    }

    /// The pinned binary at the expected version is accepted, and its banner is
    /// carried through so every result can be attributed to a named oracle.
    #[test]
    fn pinned_binary_at_the_expected_version_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let pinned = upstream::pinned_binary(root, ORACLE_VERSION);
        std::fs::create_dir_all(pinned.parent().unwrap()).unwrap();
        std::fs::write(&pinned, b"").unwrap();

        let oracle = resolve_with(root, None, |_| Some(banner(ORACLE_VERSION))).unwrap();
        assert_eq!(oracle.path, pinned);
        assert_eq!(oracle.banner, banner(ORACLE_VERSION));
    }

    /// An absent oracle must refuse to run rather than fall back to whatever
    /// rsync happens to be installed - a silent fallback is the whole defect.
    #[test]
    fn absent_pinned_binary_fails_with_a_named_diagnostic() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_with(dir.path(), None, |_| Some(banner(ORACLE_VERSION))).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("upstream oracle missing"), "{msg}");
        assert!(msg.contains(ORACLE_VERSION), "{msg}");
        assert!(msg.contains("run_interop.sh"), "{msg}");
        assert!(msg.contains(ORACLE_ENV), "{msg}");
    }

    /// A binary that is present but reports a different release must fail: a
    /// FAIL against 3.4.1 cannot be told apart from an oc defect.
    #[test]
    fn wrong_version_fails_and_names_both_versions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let pinned = upstream::pinned_binary(root, ORACLE_VERSION);
        std::fs::create_dir_all(pinned.parent().unwrap()).unwrap();
        std::fs::write(&pinned, b"").unwrap();

        let err = resolve_with(root, None, |_| Some(banner("3.4.1"))).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("version mismatch"), "{msg}");
        assert!(msg.contains("3.4.1"), "{msg}");
        assert!(msg.contains(ORACLE_VERSION), "{msg}");
    }

    /// The path is not authoritative: a directory named 3.4.4 may hold a
    /// non-rsync binary (openrsync on macOS), which must not be accepted.
    #[test]
    fn non_rsync_banner_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let pinned = upstream::pinned_binary(root, ORACLE_VERSION);
        std::fs::create_dir_all(pinned.parent().unwrap()).unwrap();
        std::fs::write(&pinned, b"").unwrap();

        let err = resolve_with(root, None, |_| Some("openrsync: 3.4.4".into())).unwrap_err();
        assert!(err.to_string().contains("is not rsync"), "{err}");
    }

    /// A binary that cannot be run at all is named rather than skipped.
    #[test]
    fn unprobeable_binary_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let pinned = upstream::pinned_binary(root, ORACLE_VERSION);
        std::fs::create_dir_all(pinned.parent().unwrap()).unwrap();
        std::fs::write(&pinned, b"").unwrap();

        let err = resolve_with(root, None, |_| None).unwrap_err();
        assert!(err.to_string().contains("unusable"), "{err}");
    }

    /// The override exists for deliberate cross-version runs, so it takes any
    /// rsync release - but it is still probed, so its version reaches the
    /// printed banner instead of being assumed.
    #[test]
    fn override_accepts_another_release_but_still_probes_it() {
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("rsync-3.1.3");
        let oracle =
            resolve_with(dir.path(), Some(other.clone()), |_| Some(banner("3.1.3"))).unwrap();
        assert_eq!(oracle.path, other);
        assert_eq!(oracle.banner, banner("3.1.3"));

        let err = resolve_with(dir.path(), Some(other), |_| None).unwrap_err();
        assert!(err.to_string().contains("unusable"), "{err}");
    }

    #[test]
    fn release_version_is_parsed_from_the_banner_not_the_path() {
        assert_eq!(
            upstream::parse_release_version(&banner("3.4.4")),
            Some("3.4.4")
        );
        assert_eq!(
            upstream::parse_release_version("rsync version 2.6.9  protocol version 29"),
            Some("2.6.9")
        );
        assert_eq!(upstream::parse_release_version("openrsync: 3.4.4"), None);
        assert_eq!(upstream::parse_release_version("rsync  version  x"), None);
    }
}
