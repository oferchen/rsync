//! Single resolver for workspace binaries under test.
//!
//! Every integration test that shells out to `oc-rsync` must run the binary
//! Cargo just built, never one left over from an earlier revision. The
//! package that declares the `oc-rsync` `[[bin]]` can say
//! `env!("CARGO_BIN_EXE_oc-rsync")`; every other package cannot - that is a
//! hard compile error. Those crates used to hand-roll a scan of
//! `target/{debug,release,dist}`, or walk up from `CARGO_MANIFEST_DIR`, which
//! resolves whatever happens to be on disk.
//!
//! This module replaces the scan with one Cargo-provided anchor: `build.rs`
//! derives the active profile directory from `OUT_DIR` and bakes it in as
//! `OC_RSYNC_TARGET_PROFILE_DIR`. Exactly one candidate path is ever
//! considered, and [`workspace_bin`] panics naming that path when it is
//! absent or not executable. There is no second candidate to fall back to and
//! no `Option` for a caller to turn into a silent skip.

use std::path::{Path, PathBuf};

/// The profile directory of the current Cargo invocation.
///
/// This is `<target-dir>/[<triple>/]<profile>` - the directory Cargo links
/// workspace binaries into - captured at compile time by this crate's
/// `build.rs`. It tracks custom profiles, cross-compilation triples and
/// relocated target directories, so no runtime probing is needed.
#[must_use]
pub fn target_profile_dir() -> &'static Path {
    Path::new(env!("OC_RSYNC_TARGET_PROFILE_DIR"))
}

/// The single path a workspace binary named `name` can occupy.
///
/// Returns the candidate whether or not it exists, for callers that need to
/// report or probe it (see [`workspace_bin`] and
/// [`crate::locate_workspace_binary`]).
#[must_use]
pub fn workspace_bin_path(name: &str) -> PathBuf {
    target_profile_dir().join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

/// Resolve a workspace binary, panicking loudly if it is not usable.
///
/// # Panics
///
/// Panics naming the exact path when the binary is missing, is not a regular
/// file, or (on Unix) is not executable. Failing here is deliberate: a helper
/// that returned `None` would let callers print `skip:` and return, and a skip
/// that can never be satisfied is indistinguishable from a pass.
#[must_use]
pub fn workspace_bin(name: &str) -> PathBuf {
    let candidate = workspace_bin_path(name);
    assert!(
        candidate.is_file(),
        "{name} binary missing at {}; refusing to fall back to a stale build \
         (run `cargo build --bin {name}` for this profile first)",
        candidate.display()
    );
    assert_executable(&candidate);
    candidate
}

/// Resolve the `oc-rsync` binary built by the current Cargo invocation.
///
/// # Panics
///
/// See [`workspace_bin`].
#[must_use]
pub fn oc_rsync_bin() -> PathBuf {
    workspace_bin("oc-rsync")
}

/// Assert the resolved path carries an execute bit.
///
/// A present-but-unexecutable file would otherwise surface as an opaque
/// `Permission denied` from `Command::spawn` deep inside a test body.
#[cfg(unix)]
fn assert_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
        .permissions()
        .mode();
    assert!(
        mode & 0o111 != 0,
        "{} exists but is not executable (mode {mode:o})",
        path.display()
    );
}

/// No-op on platforms without POSIX permission bits.
#[cfg(not(unix))]
fn assert_executable(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_dir_is_the_directory_holding_this_test_executable() {
        // Why: the whole mechanism rests on build.rs deriving the same
        // profile directory that the test harness is linked into. If those
        // ever diverge, every consumer would resolve a binary from a
        // different profile - exactly the stale-build hazard this replaces.
        let exe = std::env::current_exe().expect("current_exe");
        // `.../<profile>/deps/<test-bin>` -> `.../<profile>`.
        let from_exe = exe
            .parent()
            .and_then(Path::parent)
            .expect("test executable lives under <profile>/deps");
        assert_eq!(
            std::fs::canonicalize(from_exe).expect("canonicalize exe profile dir"),
            std::fs::canonicalize(target_profile_dir()).expect("canonicalize baked profile dir"),
        );
    }

    #[test]
    fn candidate_path_is_a_direct_child_of_the_profile_dir() {
        // Why: pins the layout contract consumers rely on. A binary nested
        // any deeper would mean build.rs picked the wrong ancestor.
        let p = workspace_bin_path("oc-rsync");
        assert_eq!(p.parent(), Some(target_profile_dir()));
        assert!(
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("oc-rsync"))
        );
    }

    #[test]
    #[should_panic(expected = "refusing to fall back to a stale build")]
    fn missing_binary_panics_naming_the_path() {
        // Why: this is the guarantee the resolver exists for. A missing
        // binary must abort the test loudly, never yield None for a caller
        // to convert into a vacuous skip.
        let _ = workspace_bin("definitely-not-built-xyzzy");
    }
}
