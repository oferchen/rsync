//! Locator for the `lsh-stub` local-shell helper binary.
//!
//! [`LshRunnerStub`] resolves the path to the compiled `lsh-stub` executable
//! (the Rust port of upstream `support/lsh.sh`, see `src/bin/lsh-stub.rs`) so a
//! test can drive oc-rsync's remote-shell code paths locally. The path feeds
//! either `--rsh <path>` on the argv or `RSYNC_RSH=<path>` in the environment.
//!
//! Resolution prefers Cargo's `CARGO_BIN_EXE_lsh-stub` (set when a crate's
//! integration tests depend on `test-support`'s bin target) and falls back to
//! [`crate::locate_workspace_binary`]. Missing binary is a loud typed error,
//! not a silent skip; gate on [`crate::require_binary`] to self-skip.
//!
//! Unix-only: the stub itself depends on `sh`/`sudo`, and the remote-shell
//! ports that consume it are `#[cfg(unix)]` (design section 5.4).

use std::path::PathBuf;

use crate::skip::locate_workspace_binary;

/// The bin-target name of the compiled stub.
pub const LSH_STUB_BIN: &str = "lsh-stub";

/// Error raised when the `lsh-stub` helper cannot be located.
#[derive(Debug)]
pub enum LshError {
    /// The `lsh-stub` binary was not found via `CARGO_BIN_EXE_lsh-stub` nor in
    /// `target/{debug,release}/`. Build the workspace (or the `test-support`
    /// bin target) before running remote-shell ports.
    StubNotFound,
}

impl std::fmt::Display for LshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LshError::StubNotFound => write!(
                f,
                "lsh-stub binary not found (set CARGO_BIN_EXE_lsh-stub or build test-support)"
            ),
        }
    }
}

impl std::error::Error for LshError {}

/// Resolved handle to the `lsh-stub` remote-shell helper.
///
/// Construct with [`locate`](LshRunnerStub::locate); pass its
/// [`path`](LshRunnerStub::path) to `--rsh`, or apply it to a
/// [`crate::OcRsyncCliRunner`] via the `RSYNC_RSH` env var.
pub struct LshRunnerStub {
    path: PathBuf,
}

impl LshRunnerStub {
    /// Locate the compiled `lsh-stub` binary.
    ///
    /// Returns [`LshError::StubNotFound`] if the helper is not built, so the
    /// failure is loud rather than a silently-skipped remote-shell leg.
    pub fn locate() -> Result<Self, LshError> {
        let path = std::env::var_os("CARGO_BIN_EXE_lsh-stub")
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .or_else(|| locate_workspace_binary(LSH_STUB_BIN))
            .ok_or(LshError::StubNotFound)?;
        Ok(Self { path })
    }

    /// The absolute path to the stub, for use as an `--rsh` argument.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// The value to assign to `RSYNC_RSH` when driving oc-rsync via env rather
    /// than an explicit `--rsh` flag.
    #[must_use]
    pub fn rsh_env_value(&self) -> std::ffi::OsString {
        self.path.clone().into_os_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_finds_the_built_stub_or_reports_loudly() {
        // Why: the stub is a workspace [[bin]], so under `cargo test`
        // CARGO_BIN_EXE_lsh-stub is set and locate() must succeed and return a
        // real file. If it is somehow unbuilt the error must be the dedicated
        // StubNotFound variant, never a silent None - Rule 12.
        match LshRunnerStub::locate() {
            Ok(stub) => {
                assert!(
                    stub.path().is_file(),
                    "located stub path must be a real file: {}",
                    stub.path().display()
                );
                // The RSYNC_RSH value must equal the resolved path so the two
                // invocation styles (--rsh vs env) stay consistent.
                assert_eq!(stub.rsh_env_value(), stub.path().as_os_str());
            }
            Err(LshError::StubNotFound) => {
                // Acceptable only when the bin genuinely is not built; the
                // env var proves whether cargo wired it.
                assert!(
                    std::env::var_os("CARGO_BIN_EXE_lsh-stub").is_none(),
                    "CARGO_BIN_EXE_lsh-stub was set yet locate() failed"
                );
            }
        }
    }
}
