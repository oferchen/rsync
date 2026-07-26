//! Locator for the `lsh-stub` local-shell helper binary.
//!
//! [`LshRunnerStub`] resolves the path to the compiled `lsh-stub` executable
//! (the Rust port of upstream `support/lsh.sh`, see `src/bin/lsh-stub.rs`) so a
//! test can drive oc-rsync's remote-shell code paths locally. The path feeds
//! either `--rsh <path>` on the argv or `RSYNC_RSH=<path>` in the environment.
//!
//! Resolution goes through [`crate::locate_workspace_binary`], which considers
//! the single path the stub can occupy for the current Cargo profile. A
//! missing binary is a loud typed error naming that path, not a silent skip;
//! gate on [`crate::require_binary`] to self-skip.
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
    /// The `lsh-stub` binary was not present at the one path it can occupy for
    /// the current Cargo profile. Build the workspace (or the `test-support`
    /// bin target) before running remote-shell ports.
    StubNotFound,
}

impl std::fmt::Display for LshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LshError::StubNotFound => write!(
                f,
                "lsh-stub binary missing at {}; refusing to fall back to a stale build",
                crate::bin_path::workspace_bin_path(LSH_STUB_BIN).display()
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
        let path = locate_workspace_binary(LSH_STUB_BIN).ok_or(LshError::StubNotFound)?;
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
        // Why: resolution must yield the single profile-dir path and nothing
        // else, and a miss must be the dedicated StubNotFound variant rather
        // than a silent None - Rule 12. The Err arm asserts that path really
        // is absent, so the arm cannot pass vacuously.
        let expected = crate::bin_path::workspace_bin_path(LSH_STUB_BIN);
        match LshRunnerStub::locate() {
            Ok(stub) => {
                assert_eq!(
                    stub.path(),
                    expected,
                    "resolution must not reach outside the current profile dir"
                );
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
                assert!(
                    !expected.is_file(),
                    "locate() reported StubNotFound yet {} exists",
                    expected.display()
                );
            }
        }
    }
}
