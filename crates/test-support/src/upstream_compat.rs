//! Cross-binary upstream-compat harness primitives.
//!
//! The upstream-compat ports move high-value edge cases from
//! `tools/ci/run_upstream_testsuite.sh` into nextest. Every port pits
//! oc-rsync against an installed upstream rsync release. This module
//! provides the shared primitives those ports use: env-var gate, version
//! enum, and upstream-binary location with self-skip semantics.
//!
//! The companion design is `docs/design/nxt-1-nextest-harness.md`. The
//! sibling internal-only harness (oc-rsync drives oc-rsync) is specified
//! in `docs/design/uts-nextest-edge-b-test-harness.md`; this module covers
//! only the cross-binary surface.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Upstream rsync version pinned by an upstream-compat test.
///
/// Versions correspond to the `target/interop/upstream-install/<version>/`
/// layout produced by `tools/ci/run_interop.sh`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UpstreamVersion {
    /// rsync 3.0.9.
    V3_0_9,
    /// rsync 3.1.3.
    V3_1_3,
    /// rsync 3.4.1.
    V3_4_1,
    /// rsync 3.4.4.
    V3_4_4,
    /// rsync 3.5.0.
    V3_5_0,
    /// rsync 3.5.1 - current upstream release.
    V3_5_1,
}

impl UpstreamVersion {
    /// Short string used in `target/interop/upstream-install/<dir>/`.
    pub fn directory(self) -> &'static str {
        match self {
            UpstreamVersion::V3_0_9 => "3.0.9",
            UpstreamVersion::V3_1_3 => "3.1.3",
            UpstreamVersion::V3_4_1 => "3.4.1",
            UpstreamVersion::V3_4_4 => "3.4.4",
            UpstreamVersion::V3_5_0 => "3.5.0",
            UpstreamVersion::V3_5_1 => "3.5.1",
        }
    }

    /// Env-var name used to override the resolved binary path.
    ///
    /// Example: `OC_RSYNC_UPSTREAM_BIN_3_4_4=/usr/local/bin/rsync`.
    pub fn env_var(self) -> &'static str {
        match self {
            UpstreamVersion::V3_0_9 => "OC_RSYNC_UPSTREAM_BIN_3_0_9",
            UpstreamVersion::V3_1_3 => "OC_RSYNC_UPSTREAM_BIN_3_1_3",
            UpstreamVersion::V3_4_1 => "OC_RSYNC_UPSTREAM_BIN_3_4_1",
            UpstreamVersion::V3_4_4 => "OC_RSYNC_UPSTREAM_BIN_3_4_4",
            UpstreamVersion::V3_5_0 => "OC_RSYNC_UPSTREAM_BIN_3_5_0",
            UpstreamVersion::V3_5_1 => "OC_RSYNC_UPSTREAM_BIN_3_5_1",
        }
    }
}

/// Resolved handle to an upstream rsync binary.
pub struct UpstreamRsync {
    binary: PathBuf,
    version: UpstreamVersion,
}

impl UpstreamRsync {
    /// Path to the resolved upstream rsync binary.
    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// Version this handle was resolved for.
    pub fn version(&self) -> UpstreamVersion {
        self.version
    }

    /// New `Command` rooted at the resolved binary, with no args yet.
    pub fn command(&self) -> Command {
        Command::new(&self.binary)
    }
}

/// Returns `true` when `OC_RSYNC_UPSTREAM_COMPAT=1` is set in the
/// environment.
///
/// Tests early-return without printing a skip line when this is `false`,
/// matching the `WHICHTESTS` env-gating convention upstream uses to keep
/// the standard PR nextest cell wall time near zero.
#[must_use]
pub fn upstream_compat_enabled() -> bool {
    matches!(
        std::env::var("OC_RSYNC_UPSTREAM_COMPAT").ok().as_deref(),
        Some("1"),
    )
}

/// Locate the upstream rsync binary for `version`.
///
/// Resolution order:
/// 1. `OC_RSYNC_UPSTREAM_BIN_<VERSION>` env var (CI override).
/// 2. `target/interop/upstream-install/<version>/bin/rsync` relative to
///    the workspace root resolved from `CARGO_MANIFEST_DIR`.
/// 3. macOS only: `/opt/homebrew/bin/rsync` (Homebrew's upstream build).
///
/// Returns `None` if no path resolves to an executable. Tests then
/// self-skip via [`require_upstream_rsync`].
#[must_use]
pub fn locate_upstream_rsync(version: UpstreamVersion) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(version.env_var()) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    upstream_install_bin(version.directory()).or_else(homebrew_rsync_bin)
}

/// macOS last-resort candidate: Homebrew installs upstream rsync at
/// `/opt/homebrew/bin/rsync`. The path alone proves nothing -
/// [`require_upstream_rsync`] verifies the `--version` banner, so a
/// Homebrew install of a different release is rejected with a skip.
/// `/usr/bin/rsync` (openrsync on macOS) is deliberately never probed.
#[cfg(target_os = "macos")]
fn homebrew_rsync_bin() -> Option<PathBuf> {
    let candidate = PathBuf::from("/opt/homebrew/bin/rsync");
    candidate.is_file().then_some(candidate)
}

/// No Homebrew fallback outside macOS.
#[cfg(not(target_os = "macos"))]
fn homebrew_rsync_bin() -> Option<PathBuf> {
    None
}

/// Locate the interop-harness install of upstream rsync `version`.
///
/// Resolves `target/interop/upstream-install/<version>/bin/rsync` anchored
/// at the workspace root, so the result is identical no matter which
/// directory Cargo happens to run the test from. Accepts any version
/// string the harness installs (e.g. `"3.4.2"`), not just the
/// [`UpstreamVersion`] variants. Returns `None` when the binary is absent,
/// letting callers self-skip or fall back.
#[must_use]
pub fn upstream_install_bin(version: &str) -> Option<PathBuf> {
    let candidate = workspace_root()?
        .join("target")
        .join("interop")
        .join("upstream-install")
        .join(version)
        .join("bin")
        .join("rsync");
    candidate.is_file().then_some(candidate)
}

/// Resolves the upstream oracle for `version`, verified by its `--version`
/// banner.
///
/// Without `OC_RSYNC_UPSTREAM_COMPAT=1` a missing or wrong oracle prints a
/// skip reason and returns `None`, so local runs without an upstream build
/// still pass. With it set, as the CI oracle step does, the same condition
/// panics: a skipped oracle cell must never be counted as a pass.
#[must_use]
pub fn require_upstream_rsync(version: UpstreamVersion) -> Option<UpstreamRsync> {
    let Some(binary) = locate_upstream_rsync(version) else {
        return oracle_unavailable(
            &format!(
                "upstream rsync {} not installed at \
                 target/interop/upstream-install/{}/bin/rsync (build it with \
                 `tools/ci/run_interop.sh build-only`, or override via {})",
                version.directory(),
                version.directory(),
                version.env_var(),
            ),
            upstream_compat_enabled(),
        );
    };

    // A path is not proof of provenance. `/usr/bin/rsync` on macOS is
    // openrsync, which speaks protocol 29 and shares none of the batch
    // format; silently testing against it would turn a wire-compat
    // assertion into a no-op. Take the version from the banner instead.
    let banner = match Command::new(&binary).arg("--version").output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
        other => {
            return oracle_unavailable(
                &format!(
                    "`{} --version` did not run cleanly ({other:?})",
                    binary.display()
                ),
                upstream_compat_enabled(),
            );
        }
    };
    let first_line = banner.lines().next().unwrap_or_default();
    let expected = format!("rsync  version {}", version.directory());
    if !first_line.starts_with(&expected) {
        return oracle_unavailable(
            &format!(
                "{} is not upstream rsync {} (banner: {first_line:?})",
                binary.display(),
                version.directory(),
            ),
            upstream_compat_enabled(),
        );
    }

    Some(UpstreamRsync { binary, version })
}

/// Reports an unusable oracle: a skip line and `None` when `required` is
/// false, a panic naming `reason` when it is true.
fn oracle_unavailable<T>(reason: &str, required: bool) -> Option<T> {
    assert!(
        !required,
        "upstream oracle required (OC_RSYNC_UPSTREAM_COMPAT=1) but unavailable: {reason}"
    );
    eprintln!("Skipping upstream-compat test: {reason}");
    None
}

/// Resolve the workspace root from the consuming test's `CARGO_MANIFEST_DIR`.
///
/// Cargo runs every test with its CWD and `CARGO_MANIFEST_DIR` set to the
/// PACKAGE root, not the workspace root, so `target/...` paths built
/// relative to either are wrong for any test target under
/// `crates/<name>/tests/`. This walks up from the manifest dir to the
/// first directory whose `Cargo.toml` declares `[workspace]`, which also
/// handles the root package (its manifest dir IS the workspace root).
#[must_use]
pub fn workspace_root() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR")?);
    manifest_dir
        .ancestors()
        .find(|dir| {
            std::fs::read_to_string(dir.join("Cargo.toml"))
                .is_ok_and(|manifest| manifest.contains("[workspace]"))
        })
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_root_is_an_ancestor_declaring_a_workspace() {
        // Why: every anchored upstream-binary lookup rests on this walk-up.
        // If it ever resolved a non-workspace dir (or None), the lookups
        // would silently miss target/interop and tests would dark-skip -
        // the exact failure mode the anchoring exists to eliminate.
        let root = workspace_root().expect("workspace root resolves under cargo");
        let manifest =
            std::fs::read_to_string(root.join("Cargo.toml")).expect("workspace Cargo.toml");
        assert!(manifest.contains("[workspace]"));
        assert!(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).starts_with(&root),
            "workspace root must be an ancestor of the consuming manifest dir"
        );
    }

    #[test]
    fn an_unavailable_oracle_skips_when_not_required() {
        // Why: local runs without an upstream build must keep passing, so the
        // optional path returns None instead of failing.
        assert!(oracle_unavailable::<()>("absent", false).is_none());
    }

    #[test]
    #[should_panic(expected = "upstream oracle required")]
    fn an_unavailable_oracle_fails_when_required() {
        // Why: under OC_RSYNC_UPSTREAM_COMPAT=1 a skip would be counted as a
        // pass in CI, so the required path must fail the test instead.
        let _ = oracle_unavailable::<()>("absent", true);
    }

    #[test]
    fn upstream_install_bin_is_workspace_anchored_and_cwd_independent() {
        // Why: the resolver must produce the same path no matter where
        // Cargo set the CWD; a CWD-relative probe is exactly the bug this
        // module replaces. The binary may legitimately be absent (macOS
        // dev machines), so assert on the anchored shape via the root
        // rather than on existence.
        let root = workspace_root().expect("workspace root resolves under cargo");
        let expected = root.join("target/interop/upstream-install/3.4.4/bin/rsync");
        match upstream_install_bin("3.4.4") {
            Some(found) => assert_eq!(found, expected),
            None => assert!(
                !expected.is_file(),
                "resolver returned None while {} exists",
                expected.display()
            ),
        }
    }
}
