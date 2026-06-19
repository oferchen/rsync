//! Cross-binary upstream-compat harness primitives (NXT-1).
//!
//! NXT-2..NXT-8 port high-value edge cases from
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

/// Upstream rsync version pinned by an NXT-* test.
///
/// Versions correspond to the `target/interop/upstream-install/<version>/`
/// layout produced by `tools/ci/run_interop.sh`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UpstreamVersion {
    /// rsync 3.0.9.
    V3_0_9,
    /// rsync 3.1.3.
    V3_1_3,
    /// rsync 3.4.4 - default for new NXT-* ports.
    V3_4_4,
}

impl UpstreamVersion {
    /// Short string used in `target/interop/upstream-install/<dir>/`.
    pub fn directory(self) -> &'static str {
        match self {
            UpstreamVersion::V3_0_9 => "3.0.9",
            UpstreamVersion::V3_1_3 => "3.1.3",
            UpstreamVersion::V3_4_4 => "3.4.4",
        }
    }

    /// Env-var name used to override the resolved binary path.
    ///
    /// Example: `OC_RSYNC_UPSTREAM_BIN_3_4_4=/usr/local/bin/rsync`.
    pub fn env_var(self) -> &'static str {
        match self {
            UpstreamVersion::V3_0_9 => "OC_RSYNC_UPSTREAM_BIN_3_0_9",
            UpstreamVersion::V3_1_3 => "OC_RSYNC_UPSTREAM_BIN_3_1_3",
            UpstreamVersion::V3_4_4 => "OC_RSYNC_UPSTREAM_BIN_3_4_4",
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
///
/// Returns `None` if neither path resolves to an executable. Tests then
/// self-skip via [`require_upstream_rsync`].
#[must_use]
pub fn locate_upstream_rsync(version: UpstreamVersion) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(version.env_var()) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    let workspace_root = workspace_root()?;
    let candidate = workspace_root
        .join("target")
        .join("interop")
        .join("upstream-install")
        .join(version.directory())
        .join("bin")
        .join("rsync");
    if candidate.is_file() {
        return Some(candidate);
    }
    None
}

/// Self-skip helper. Returns `Some(UpstreamRsync)` when the binary
/// exists, else prints a clear reason and returns `None`. Tests early-
/// return on `None` so nextest reports them as passing with the skip
/// reason in stderr.
#[must_use]
pub fn require_upstream_rsync(version: UpstreamVersion) -> Option<UpstreamRsync> {
    match locate_upstream_rsync(version) {
        Some(binary) => Some(UpstreamRsync { binary, version }),
        None => {
            eprintln!(
                "Skipping upstream-compat test: upstream rsync {} not installed at \
                 target/interop/upstream-install/{}/bin/rsync (override via {})",
                version.directory(),
                version.directory(),
                version.env_var(),
            );
            None
        }
    }
}

/// Resolve the workspace root from `CARGO_MANIFEST_DIR`.
///
/// `CARGO_MANIFEST_DIR` for the consuming test crate points at
/// `crates/<crate>/`. The workspace root is its parent's parent.
fn workspace_root() -> Option<PathBuf> {
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let manifest_dir = PathBuf::from(manifest_dir);
    let crates_dir = manifest_dir.parent()?;
    crates_dir.parent().map(Path::to_path_buf)
}
